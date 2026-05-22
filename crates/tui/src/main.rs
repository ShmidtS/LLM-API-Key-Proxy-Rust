use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::Path;
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
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table, Tabs, Wrap};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::process::Command;

const TAB_TITLES: [&str; 3] = ["Providers", "Model Cache", "Usage"];
const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3000";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 3000;
const LAUNCHER_CONFIG_PATH: &str = "launcher_config.json";
const DEFAULT_MODEL_CACHE_TTL_SECS: u64 = 300;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppMode {
    MainMenu,
    Dashboard,
    ConfigMenu,
    About,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LauncherConfig {
    host: String,
    port: u16,
    enable_raw_logging: bool,
    base_url: String,
}

impl Default for LauncherConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            enable_raw_logging: false,
            base_url: DEFAULT_BASE_URL.to_owned(),
        }
    }
}

impl LauncherConfig {
    fn load() -> Self {
        fs::read_to_string(LAUNCHER_CONFIG_PATH)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    fn save(&self) -> Result<()> {
        let contents = serde_json::to_string_pretty(self)?;
        fs::write(LAUNCHER_CONFIG_PATH, contents).context("write launcher_config.json")
    }

    fn refresh_base_url(&mut self) {
        self.base_url = format!("http://{}:{}", self.host, self.port);
    }
}

#[derive(Debug)]
struct App {
    current_tab: usize,
    client: reqwest::Client,
    base_url: String,
    admin_token: Option<String>,
    data: DashboardData,
    last_error: Option<String>,
    mode: AppMode,
    config: LauncherConfig,
    config_draft: LauncherConfig,
    edit_field: Option<ConfigField>,
    edit_buffer: String,
    message: Option<String>,
    onboarding_warning: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfigField {
    Host,
    Port,
}

impl App {
    fn new() -> Self {
        let mut config = LauncherConfig::load();
        config.refresh_base_url();
        let base_url = config.base_url.clone();
        Self {
            current_tab: 0,
            client: reqwest::Client::new(),
            base_url,
            admin_token: std::env::var("ADMIN_TOKEN")
                .ok()
                .filter(|token| !token.is_empty()),
            data: DashboardData::default(),
            last_error: None,
            mode: AppMode::MainMenu,
            config_draft: config.clone(),
            config,
            edit_field: None,
            edit_buffer: String::new(),
            message: None,
            onboarding_warning: setup_required(),
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

            if self.edit_field.is_some() {
                self.handle_config_input(key.code);
                return false;
            }

            match self.mode {
                AppMode::MainMenu => self.handle_main_menu_key(key.code).await,
                AppMode::Dashboard => self.handle_dashboard_key(key.code).await,
                AppMode::ConfigMenu => self.handle_config_menu_key(key.code),
                AppMode::About => self.handle_about_key(key.code),
            }
        } else {
            false
        }
    }

    async fn handle_main_menu_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => true,
            KeyCode::Char('d') | KeyCode::Char('1') => {
                self.switch_to_dashboard().await;
                false
            }
            KeyCode::Char('c') | KeyCode::Char('2') => {
                self.config_draft = self.config.clone();
                self.mode = AppMode::ConfigMenu;
                self.message = None;
                false
            }
            KeyCode::Char('r') | KeyCode::Char('3') => {
                self.run_proxy().await;
                self.switch_to_dashboard().await;
                false
            }
            KeyCode::Char('a') | KeyCode::Char('4') => {
                self.mode = AppMode::About;
                self.message = None;
                false
            }
            KeyCode::Char('5') => true,
            _ => false,
        }
    }

    async fn handle_dashboard_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => true,
            KeyCode::Char('m') => {
                self.mode = AppMode::MainMenu;
                false
            }
            KeyCode::Char('r') => {
                self.refresh().await;
                false
            }
            KeyCode::Right | KeyCode::Tab => {
                self.next_tab();
                false
            }
            KeyCode::Left | KeyCode::BackTab => {
                self.previous_tab();
                false
            }
            _ => false,
        }
    }

    fn handle_config_menu_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc => {
                self.config_draft = self.config.clone();
                self.mode = AppMode::MainMenu;
                self.message = None;
            }
            KeyCode::Char('h') => {
                self.edit_field = Some(ConfigField::Host);
                self.edit_buffer = self.config_draft.host.clone();
            }
            KeyCode::Char('p') => {
                self.edit_field = Some(ConfigField::Port);
                self.edit_buffer = self.config_draft.port.to_string();
            }
            KeyCode::Char('r') => {
                self.config_draft.enable_raw_logging = !self.config_draft.enable_raw_logging;
                self.save_config_draft();
            }
            KeyCode::Char('s') => {
                self.save_config_draft();
                self.mode = AppMode::MainMenu;
            }
            _ => {}
        }
        false
    }

    fn handle_config_input(&mut self, code: KeyCode) {
        match code {
            KeyCode::Enter => self.commit_config_input(),
            KeyCode::Esc => {
                self.edit_field = None;
                self.edit_buffer.clear();
            }
            KeyCode::Backspace => {
                self.edit_buffer.pop();
            }
            KeyCode::Char(character) => {
                if matches!(self.edit_field, Some(ConfigField::Port)) {
                    if character.is_ascii_digit() {
                        self.edit_buffer.push(character);
                    }
                } else {
                    self.edit_buffer.push(character);
                }
            }
            _ => {}
        }
    }

    fn commit_config_input(&mut self) {
        match self.edit_field {
            Some(ConfigField::Host) => {
                let host = self.edit_buffer.trim();
                if host.is_empty() {
                    self.message = Some("Host cannot be empty".to_owned());
                } else {
                    self.config_draft.host = host.to_owned();
                    self.save_config_draft();
                }
            }
            Some(ConfigField::Port) => match self.edit_buffer.trim().parse::<u16>() {
                Ok(port) => {
                    self.config_draft.port = port;
                    self.save_config_draft();
                }
                Err(_) => self.message = Some("Port must be 0-65535".to_owned()),
            },
            None => {}
        }
        self.edit_field = None;
        self.edit_buffer.clear();
    }

    fn handle_about_key(&mut self, code: KeyCode) -> bool {
        match code {
            KeyCode::Char('q') | KeyCode::Esc | KeyCode::Char('m') => {
                self.mode = AppMode::MainMenu;
                false
            }
            _ => false,
        }
    }

    async fn switch_to_dashboard(&mut self) {
        self.config.refresh_base_url();
        self.base_url = self.config.base_url.clone();
        self.mode = AppMode::Dashboard;
        self.refresh().await;
    }

    async fn run_proxy(&mut self) {
        let port = self.config.port.to_string();
        match Command::new("cargo")
            .args([
                "run",
                "--bin",
                "proxy_app",
                "--",
                "--host",
                &self.config.host,
                "--port",
                &port,
            ])
            .spawn()
        {
            Ok(_) => self.message = Some("Proxy started".to_owned()),
            Err(error) => self.message = Some(format!("Proxy start failed: {error}")),
        }
    }

    fn save_config_draft(&mut self) {
        self.config_draft.refresh_base_url();
        match self.config_draft.save() {
            Ok(()) => {
                self.config = self.config_draft.clone();
                self.base_url = self.config.base_url.clone();
                self.message = Some("Configuration saved".to_owned());
            }
            Err(error) => self.message = Some(format!("Configuration save failed: {error}")),
        }
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
    match app.mode {
        AppMode::MainMenu => render_main_menu(frame, app),
        AppMode::Dashboard => render_dashboard(frame, app),
        AppMode::ConfigMenu => render_config_menu(frame, app),
        AppMode::About => render_about(frame),
    }
}

fn render_main_menu(frame: &mut ratatui::Frame<'_>, app: &App) {
    let constraints = if app.onboarding_warning {
        vec![
            Constraint::Length(5),
            Constraint::Min(8),
            Constraint::Length(1),
        ]
    } else {
        vec![Constraint::Min(8), Constraint::Length(1)]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(frame.area());
    let mut body_index = 0;

    if app.onboarding_warning {
        frame.render_widget(
            Paragraph::new(
                "Setup required: .env missing or PROXY_API_KEY not set. Run `cargo run --bin proxy_app -- --add-credential` first.",
            )
            .style(Style::default().fg(Color::Red).add_modifier(Modifier::BOLD))
            .block(Block::default().borders(Borders::ALL).title("Setup required"))
            .wrap(Wrap { trim: true }),
            chunks[0],
        );
        body_index = 1;
    }

    let menu = vec![
        Line::from("1. Dashboard (d)"),
        Line::from("2. Configuration (c)"),
        Line::from("3. Run Proxy (r)"),
        Line::from("4. About (a)"),
        Line::from("5. Exit (q/Esc)"),
        Line::from(""),
        Line::from(format!("Current endpoint: {}", app.config.base_url)),
    ];
    frame.render_widget(
        Paragraph::new(menu).block(
            Block::default()
                .borders(Borders::ALL)
                .title("LLM API Key Proxy Launcher"),
        ),
        chunks[body_index],
    );
    render_message(frame, chunks[body_index + 1], app);
}

fn render_config_menu(frame: &mut ratatui::Frame<'_>, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10), Constraint::Length(1)])
        .split(frame.area());
    let edit = match app.edit_field {
        Some(ConfigField::Host) => format!("Editing host: {}", app.edit_buffer),
        Some(ConfigField::Port) => format!("Editing port: {}", app.edit_buffer),
        None => "".to_owned(),
    };
    let body = vec![
        Line::from(format!("Host: {}", app.config_draft.host)),
        Line::from(format!("Port: {}", app.config_draft.port)),
        Line::from(format!(
            "Enable raw logging: {}",
            app.config_draft.enable_raw_logging
        )),
        Line::from(format!("Base URL: {}", app.config_draft.base_url)),
        Line::from(""),
        Line::from("h edit host | p edit port | r toggle raw logging"),
        Line::from("s save and return | q return without saving"),
        Line::from("Enter accepts edit | Esc cancels edit"),
        Line::from(edit),
    ];
    frame.render_widget(
        Paragraph::new(body).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Configuration"),
        ),
        chunks[0],
    );
    render_message(frame, chunks[1], app);
}

fn render_about(frame: &mut ratatui::Frame<'_>) {
    let body = vec![
        Line::from("LLM API Key Proxy"),
        Line::from(format!("Version: {}", env!("CARGO_PKG_VERSION"))),
        Line::from("GitHub: https://github.com/ShmidtS/LLM-API-Key-Proxy-Rust"),
        Line::from(""),
        Line::from("m/q/Esc return to main menu"),
    ];
    frame.render_widget(
        Paragraph::new(body).block(Block::default().borders(Borders::ALL).title("About")),
        frame.area(),
    );
}

fn render_dashboard(frame: &mut ratatui::Frame<'_>, app: &App) {
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

    let mut status = "m menu | q quit | r refresh | ←/→ switch tabs".to_owned();
    if let Some(message) = app.message.as_deref() {
        status.push_str(&format!(" | {message}"));
    }
    if let Some(error) = app.last_error.as_deref() {
        status.push_str(&format!(" | error: {error}"));
    }
    frame.render_widget(
        Paragraph::new(status).style(Style::default().fg(Color::Gray)),
        chunks[2],
    );
}

fn render_message(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let message = app.message.as_deref().unwrap_or_default();
    frame.render_widget(
        Paragraph::new(message).style(Style::default().fg(Color::Gray)),
        area,
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

fn setup_required() -> bool {
    !Path::new(".env").exists()
        || std::env::var("PROXY_API_KEY")
            .map(|value| value.trim().is_empty())
            .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launcher_config_default_matches_expected_values() {
        let config = LauncherConfig::default();

        assert_eq!(config.host, DEFAULT_HOST);
        assert_eq!(config.port, DEFAULT_PORT);
        assert!(!config.enable_raw_logging);
        assert_eq!(config.base_url, DEFAULT_BASE_URL);
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
