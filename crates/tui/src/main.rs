use std::io;
use std::time::Duration;

use anyhow::Result;
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
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Row, Table, Tabs};
use rotator::{
    AuthType, ModelInfoService, ModelMetadata, ProviderDefinition, ProviderRegistry, UsageEntry,
    UsageManager,
};

const TAB_TITLES: [&str; 3] = ["Provider Status", "Model Filter", "Quota Viewer"];

#[derive(Debug)]
struct App {
    current_tab: usize,
    providers: Vec<ProviderDefinition>,
    models: Vec<ModelMetadata>,
    usage: Vec<UsageEntry>,
    filter_text: String,
    selected_model: usize,
    filter_enabled: bool,
}

impl App {
    fn new() -> Self {
        let registry = ProviderRegistry::new();
        let model_service = ModelInfoService::new();
        let usage_manager = UsageManager::with_path("data/key_usage.json");

        Self {
            current_tab: 0,
            providers: registry.all_providers(),
            models: model_service.get_all_models(),
            usage: usage_manager.get_all_usage(),
            filter_text: String::new(),
            selected_model: 0,
            filter_enabled: false,
        }
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

    fn next_model(&mut self) {
        let len = self.filtered_models().len();
        if len > 0 {
            self.selected_model = (self.selected_model + 1) % len;
        }
    }

    fn previous_model(&mut self) {
        let len = self.filtered_models().len();
        if len > 0 {
            self.selected_model = if self.selected_model == 0 {
                len - 1
            } else {
                self.selected_model - 1
            };
        }
    }

    fn toggle_filter(&mut self) {
        self.filter_enabled = !self.filter_enabled;
        self.selected_model = 0;
    }

    fn filtered_models(&self) -> Vec<&ModelMetadata> {
        if !self.filter_enabled || self.filter_text.is_empty() {
            return self.models.iter().collect();
        }

        let service = ModelInfoService::new();
        service
            .find_models(&self.filter_text)
            .iter()
            .filter_map(|matched| {
                self.models
                    .iter()
                    .find(|model| model.model_id == matched.model_id)
            })
            .collect()
    }

    fn handle_event(&mut self, event: Event) -> bool {
        if let Event::Key(key) = event {
            if key.kind != KeyEventKind::Press {
                return false;
            }

            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return true,
                KeyCode::Tab => self.next_tab(),
                KeyCode::BackTab => self.previous_tab(),
                KeyCode::Down if self.current_tab == 1 => self.next_model(),
                KeyCode::Up if self.current_tab == 1 => self.previous_model(),
                KeyCode::Enter if self.current_tab == 1 => self.toggle_filter(),
                KeyCode::Backspace if self.current_tab == 1 => {
                    self.filter_text.pop();
                    self.selected_model = 0;
                }
                KeyCode::Char(ch) if self.current_tab == 1 => {
                    self.filter_text.push(ch);
                    self.selected_model = 0;
                }
                _ => {}
            }
        }

        false
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut terminal = setup_terminal()?;
    let mut app = App::new();
    let result = run_app(&mut terminal, &mut app);
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

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;

        if event::poll(Duration::from_millis(100))? && app.handle_event(event::read()?) {
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
        0 => render_providers(frame, chunks[1], app),
        1 => render_models(frame, chunks[1], app),
        2 => render_usage(frame, chunks[1], app),
        _ => {}
    }

    let help = Paragraph::new("Tab/Shift+Tab switch tabs | q/Esc quit | Model tab: type regex, Enter toggle filter, arrows navigate")
        .style(Style::default().fg(Color::Gray));
    frame.render_widget(help, chunks[2]);
}

fn render_providers(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let header = Row::new(["Provider ID", "Base URL", "Auth Type", "Models", "Status"]).style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = app.providers.iter().map(|provider| {
        Row::new([
            provider.id.clone(),
            provider.base_url.clone(),
            auth_type_label(&provider.auth_type).to_string(),
            app.models
                .iter()
                .filter(|model| model.provider == provider.id)
                .count()
                .to_string(),
            "Active".to_string(),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(18),
            Constraint::Percentage(38),
            Constraint::Length(12),
            Constraint::Length(8),
            Constraint::Length(12),
        ],
    )
    .header(header)
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("Provider Status"),
    )
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn render_models(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(4)])
        .split(area);

    let filter_state = if app.filter_enabled {
        "enabled"
    } else {
        "typing"
    };
    let filter = Paragraph::new(app.filter_text.as_str()).block(
        Block::default()
            .borders(Borders::ALL)
            .title(format!("Regex Filter ({filter_state})")),
    );
    frame.render_widget(filter, chunks[0]);

    let models = app.filtered_models();
    let items = models
        .iter()
        .enumerate()
        .map(|(index, model)| {
            let selected = if index == app.selected_model {
                "> "
            } else {
                "  "
            };
            ListItem::new(format!(
                "{selected}{} | {} | ctx {} | ${:.4}/${:.4} per 1k",
                model.model_id,
                model.provider,
                model.context_length,
                model.pricing_input_per_1k,
                model.pricing_output_per_1k
            ))
        })
        .collect::<Vec<_>>();
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("Models"));
    frame.render_widget(list, chunks[1]);
}

fn render_usage(frame: &mut ratatui::Frame<'_>, area: ratatui::layout::Rect, app: &App) {
    let header = Row::new([
        "Provider",
        "Key",
        "Prompt Tokens",
        "Completion Tokens",
        "Total Tokens",
    ])
    .style(
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );
    let rows = app.usage.iter().map(|entry| {
        Row::new([
            entry.provider.clone(),
            mask_key(&entry.key),
            entry.prompt_tokens.to_string(),
            entry.completion_tokens.to_string(),
            entry.total_tokens.to_string(),
        ])
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(16),
            Constraint::Length(24),
            Constraint::Length(16),
            Constraint::Length(20),
            Constraint::Length(14),
        ],
    )
    .header(header)
    .block(Block::default().borders(Borders::ALL).title("Quota Viewer"))
    .column_spacing(1);
    frame.render_widget(table, area);
}

fn auth_type_label(auth_type: &AuthType) -> &'static str {
    match auth_type {
        AuthType::ApiKey => "API Key",
        AuthType::OAuth => "OAuth",
        AuthType::Bearer => "Bearer",
    }
}

fn mask_key(key: &str) -> String {
    if key.len() <= 8 {
        return "****".to_string();
    }

    format!("{}****{}", &key[..4], &key[key.len() - 4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn app_new_creates_default_state() {
        let app = App::new();

        assert_eq!(app.current_tab, 0);
        assert!(!app.providers.is_empty());
        assert!(!app.models.is_empty());
        assert!(app.filter_text.is_empty());
    }
}
