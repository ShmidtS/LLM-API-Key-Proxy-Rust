use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, Tabs};
use serde_json::Value;

const TAB_TITLES: [&str; 3] = ["Providers", "Model Cache", "Usage"];
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3000";
const DEFAULT_MODEL_CACHE_TTL_SECS: u64 = 300;

#[derive(Debug)]
struct App {
    current_tab: usize,
    client: reqwest::Client,
    base_url: String,
    admin_token: Option<String>,
    data: DashboardData,
    last_error: Option<String>,
}

impl App {
    fn new() -> Self {
        Self {
            current_tab: 0,
            client: reqwest::Client::new(),
            base_url: std::env::var("PROXY_TUI_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE_URL.to_owned()),
            admin_token: std::env::var("ADMIN_TOKEN")
                .ok()
                .filter(|token| !token.is_empty()),
            data: DashboardData::default(),
            last_error: None,
        }
    }

    async fn refresh(&mut self) {
        match self.fetch_dashboard().await {
            Ok(data) => {
                self.data = data;
                self.last_error = None;
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    async fn fetch_dashboard(&self) -> Result<DashboardData> {
        let stats = self
            .get_json("/admin/stats")
            .await
            .context("GET /admin/stats failed")?;
        let models = self
            .get_json("/v1/models")
            .await
            .context("GET /v1/models failed")?;

        DashboardData::from_api_values(stats, models)
    }

    async fn get_json(&self, path: &str) -> Result<Value> {
        let url = format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut request = self.client.get(url);
        if let Some(token) = self.admin_token.as_deref() {
            request = request.bearer_auth(token);
        }

        Ok(request.send().await?.error_for_status()?.json().await?)
    }

    fn next_tab(&mut self) {
        self.current_tab = (self.current_tab + 1) % TAB_TITLES.len();
    }

    fn previous_tab(&mut self) {
        self.current_tab = if self.current_tab == 0 {
            TAB_TITLES.len() - 1
        } else {
            self.current_tab - 1
        };
    }

    async fn handle_event(&mut self, event: Event) -> bool {
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return false;
            }

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return true,
                KeyCode::Char('r') => self.refresh().await,
                KeyCode::Right | KeyCode::Tab => self.next_tab(),
                KeyCode::Left | KeyCode::BackTab => self.previous_tab(),
                _ => {}
            }
        }

        false
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DashboardData {
    providers: Vec<ProviderRow>,
    cache: Vec<CacheRow>,
    usage: Vec<UsageRow>,
}

impl DashboardData {
    fn from_api_values(stats: Value, models: Value) -> Result<Self> {
        let mut provider_rows = parse_provider_rows(&stats);
        let model_counts = parse_model_counts(&models);
        let usage_rows = parse_usage_rows(&stats);
        let mut cache_rows = parse_cache_rows(&stats, &model_counts);

        for (provider, count) in model_counts {
            if let Some(row) = provider_rows.iter_mut().find(|row| row.id == provider) {
                row.cached_models = count;
            } else {
                provider_rows.push(ProviderRow {
                    id: provider.clone(),
                    base_url: "unknown".to_owned(),
                    status: ProviderStatus::Ok,
                    latency_ms: None,
                    cached_models: count,
                    cache_ttl_secs: DEFAULT_MODEL_CACHE_TTL_SECS,
                });
            }

            if !cache_rows.iter().any(|row| row.provider == provider) {
                cache_rows.push(CacheRow {
                    provider,
                    cached_models: count,
                    ttl_secs: DEFAULT_MODEL_CACHE_TTL_SECS,
                });
            }
        }

        provider_rows.sort_by(|left, right| left.id.cmp(&right.id));
        cache_rows.sort_by(|left, right| left.provider.cmp(&right.provider));

        Ok(Self {
            providers: provider_rows,
            cache: cache_rows,
            usage: usage_rows,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProviderRow {
    id: String,
    base_url: String,
    status: ProviderStatus,
    latency_ms: Option<u64>,
    cached_models: usize,
    cache_ttl_secs: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProviderStatus {
    Ok,
    Error,
    CircuitOpen,
}

impl ProviderStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Error => "Error",
            Self::CircuitOpen => "Circuit Open",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CacheRow {
    provider: String,
    cached_models: usize,
    ttl_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UsageRow {
    provider: String,
    prompt_tokens: u64,
    completion_tokens: u64,
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut app = App::new();
    app.refresh().await;
    let result = run_app(&mut terminal, &mut app).await;
    restore_terminal(&mut terminal)?;
    result
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    Ok(Terminal::new(backend)?)
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;

        if event::poll(Duration::from_millis(100))? && app.handle_event(event::read()?).await {
            break;
        }
    }

    Ok(())
}

fn render(frame: &mut ratatui::Frame<'_>, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(1),
        ])
        .split(frame.area());

    let titles = TAB_TITLES
        .iter()
        .map(|title| Line::from(Span::styled(*title, Style::default().fg(Color::Cyan))))
        .collect::<Vec<_>>();
    let tabs = Tabs::new(titles)
        .select(app.current_tab)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("LLM API Key Proxy"),
        )
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, chunks[0]);

    match app.current_tab {
        0 => render_providers(frame, chunks[1], &app.data),
        1 => render_cache(frame, chunks[1], &app.data),
        2 => render_usage(frame, chunks[1], &app.data),
        _ => {}
    }

    let status = app.last_error.as_deref().map_or(
        "q quit | r refresh | ←/→ switch tabs".to_owned(),
        |error| format!("q quit | r refresh | ←/→ switch tabs | error: {error}"),
    );
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::Gray)),
        chunks[2],
    );
}

fn render_providers(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    data: &DashboardData,
) {
    let header = Row::new(["Provider", "Base URL", "Status", "Latency", "Models"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = data.providers.iter().map(|provider| {
        Row::new([
            provider.id.clone(),
            provider.base_url.clone(),
            provider.status.label().to_owned(),
            format_latency(provider.latency_ms),
            provider.cached_models.to_string(),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(18),
            Constraint::Percentage(45),
            Constraint::Length(14),
            Constraint::Length(12),
            Constraint::Length(8),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Provider Health"),
    )
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn render_cache(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, data: &DashboardData) {
    let header = Row::new(["Provider", "Cached Models", "TTL"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = data.cache.iter().map(|cache| {
        Row::new([
            cache.provider.clone(),
            cache.cached_models.to_string(),
            format!("{}s", cache.ttl_secs),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(20),
            Constraint::Length(16),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Model Cache"))
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn render_usage(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, data: &DashboardData) {
    let header = Row::new(["Provider", "Prompt Tokens", "Completion Tokens"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = data.usage.iter().map(|usage| {
        Row::new([
            usage.provider.clone(),
            usage.prompt_tokens.to_string(),
            usage.completion_tokens.to_string(),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(20),
            Constraint::Length(18),
            Constraint::Length(20),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Usage"))
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn parse_provider_rows(stats: &Value) -> Vec<ProviderRow> {
    stats
        .get("providers")
        .and_then(Value::as_array)
        .map(|providers| {
            providers
                .iter()
                .filter_map(|provider| match provider {
                    Value::String(id) => Some(ProviderRow {
                        id: id.clone(),
                        base_url: "unknown".to_owned(),
                        status: ProviderStatus::Ok,
                        latency_ms: None,
                        cached_models: 0,
                        cache_ttl_secs: DEFAULT_MODEL_CACHE_TTL_SECS,
                    }),
                    Value::Object(object) => {
                        let id = object.get("id")?.as_str()?.to_owned();
                        Some(ProviderRow {
                            base_url: object
                                .get("base_url")
                                .and_then(Value::as_str)
                                .unwrap_or("unknown")
                                .to_owned(),
                            status: object
                                .get("status")
                                .and_then(Value::as_str)
                                .map(parse_status)
                                .unwrap_or(ProviderStatus::Ok),
                            latency_ms: object.get("latency_ms").and_then(Value::as_u64),
                            cached_models: object
                                .get("cached_models")
                                .and_then(Value::as_u64)
                                .unwrap_or_default()
                                as usize,
                            cache_ttl_secs: object
                                .get("cache_ttl_secs")
                                .and_then(Value::as_u64)
                                .unwrap_or(DEFAULT_MODEL_CACHE_TTL_SECS),
                            id,
                        })
                    }
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

fn parse_cache_rows(stats: &Value, model_counts: &BTreeMap<String, usize>) -> Vec<CacheRow> {
    let mut rows = stats
        .get("providers")
        .and_then(Value::as_array)
        .map(|providers| {
            providers
                .iter()
                .filter_map(|provider| {
                    let object = provider.as_object()?;
                    let id = object.get("id")?.as_str()?.to_owned();
                    Some(CacheRow {
                        cached_models: object
                            .get("cached_models")
                            .and_then(Value::as_u64)
                            .map(|count| count as usize)
                            .or_else(|| model_counts.get(&id).copied())
                            .unwrap_or_default(),
                        ttl_secs: object
                            .get("cache_ttl_secs")
                            .and_then(Value::as_u64)
                            .unwrap_or(DEFAULT_MODEL_CACHE_TTL_SECS),
                        provider: id,
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if rows.is_empty() {
        rows = model_counts
            .iter()
            .map(|(provider, count)| CacheRow {
                provider: provider.clone(),
                cached_models: *count,
                ttl_secs: DEFAULT_MODEL_CACHE_TTL_SECS,
            })
            .collect();
    }

    rows
}

fn parse_model_counts(models: &Value) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    if let Some(data) = models.get("data").and_then(Value::as_array) {
        for model in data {
            if let Some(provider) = model.get("owned_by").and_then(Value::as_str) {
                *counts.entry(provider.to_owned()).or_insert(0) += 1;
            }
        }
    }
    counts
}

fn parse_usage_rows(stats: &Value) -> Vec<UsageRow> {
    let mut usage = BTreeMap::<String, (u64, u64)>::new();

    if let Some(providers) = stats.get("providers").and_then(Value::as_array) {
        for provider in providers.iter().filter_map(Value::as_object) {
            if let Some(id) = provider.get("id").and_then(Value::as_str) {
                let entry = usage.entry(id.to_owned()).or_default();
                entry.0 += provider
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                entry.1 += provider
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
            }
        }
    }

    if let Some(entries) = stats.get("usage").and_then(Value::as_array) {
        for item in entries {
            if let Some(provider) = item.get("provider").and_then(Value::as_str) {
                let entry = usage.entry(provider.to_owned()).or_default();
                entry.0 += item
                    .get("prompt_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
                entry.1 += item
                    .get("completion_tokens")
                    .and_then(Value::as_u64)
                    .unwrap_or_default();
            }
        }
    }

    let providers = stats
        .get("providers")
        .and_then(Value::as_array)
        .map(|providers| {
            providers
                .iter()
                .filter_map(|provider| match provider {
                    Value::String(id) => Some(id.clone()),
                    Value::Object(object) => object
                        .get("id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                    _ => None,
                })
                .collect::<BTreeSet<_>>()
        })
        .unwrap_or_default();
    for provider in providers {
        usage.entry(provider).or_default();
    }

    usage
        .into_iter()
        .map(|(provider, (prompt_tokens, completion_tokens))| UsageRow {
            provider,
            prompt_tokens,
            completion_tokens,
        })
        .collect()
}

fn parse_status(status: &str) -> ProviderStatus {
    match status.to_ascii_lowercase().as_str() {
        "error" => ProviderStatus::Error,
        "circuit_open" | "circuit open" | "open" => ProviderStatus::CircuitOpen,
        _ => ProviderStatus::Ok,
    }
}

fn format_latency(latency_ms: Option<u64>) -> String {
    latency_ms.map_or_else(|| "n/a".to_owned(), |latency| format!("{latency}ms"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn app_new_creates_default_state() {
        let app = App::new();

        assert_eq!(app.current_tab, 0);
        assert!(app.data.providers.is_empty());
        assert!(app.last_error.is_none());
    }

    #[test]
    fn parses_admin_stats_models_and_aggregates_usage() {
        let stats = serde_json::json!({
            "providers": [
                {
                    "id": "openai",
                    "base_url": "https://api.openai.com/v1",
                    "status": "OK",
                    "latency_ms": 42,
                    "cached_models": 2,
                    "cache_ttl_secs": 300,
                    "prompt_tokens": 100,
                    "completion_tokens": 50
                }
            ],
            "usage": [
                {"provider": "openai", "prompt_tokens": 5, "completion_tokens": 7},
                {"provider": "openai", "prompt_tokens": 8, "completion_tokens": 9}
            ]
        });
        let models = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "gpt-4o", "object": "model", "created": 0, "owned_by": "openai"},
                {"id": "gpt-4o-mini", "object": "model", "created": 0, "owned_by": "openai"}
            ]
        });

        let snapshot = DashboardData::from_api_values(stats, models).expect("api values parse");

        assert_eq!(snapshot.providers[0].id, "openai");
        assert_eq!(snapshot.providers[0].base_url, "https://api.openai.com/v1");
        assert_eq!(snapshot.providers[0].status, ProviderStatus::Ok);
        assert_eq!(snapshot.providers[0].latency_ms, Some(42));
        assert_eq!(snapshot.cache[0].cached_models, 2);
        assert_eq!(snapshot.cache[0].ttl_secs, 300);
        assert_eq!(snapshot.usage[0].prompt_tokens, 113);
        assert_eq!(snapshot.usage[0].completion_tokens, 66);
    }
}
