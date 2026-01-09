use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style, Stylize},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;
use std::net::IpAddr;
use std::path::PathBuf;
use tracing::Level;

use crate::cloudflare::{DdnsClient, UpdateResult};
use crate::config::{Config, RecordType};
use crate::ip::get_public_ip;

/// Default config file path
const DEFAULT_CONFIG_PATH: &str = "config.toml";

/// Application state for the TUI
pub struct App {
    /// Path to the config file
    config_path: PathBuf,
    /// Current input mode
    mode: InputMode,
    /// Current screen
    screen: Screen,
    /// API token input
    api_token: String,
    /// Zone input
    zone: String,
    /// Record name input
    record_name: String,
    /// Record type selection
    record_type: RecordType,
    /// Proxied toggle
    proxied: bool,
    /// TTL input
    ttl: String,
    /// Currently selected input field
    selected_field: usize,
    /// Status messages/logs
    logs: Vec<LogEntry>,
    /// Current public IP (if detected)
    current_ip: Option<IpAddr>,
    /// Whether an update is in progress
    updating: bool,
    /// Last update result
    last_result: Option<String>,
    /// List state for logs
    log_state: ListState,
    /// Whether config has unsaved changes
    dirty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum InputMode {
    Normal,
    Editing,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Screen {
    Main,
    Help,
}

#[derive(Debug, Clone)]
struct LogEntry {
    level: Level,
    message: String,
    /// If true, display as success (green) instead of normal level color
    success: bool,
}

impl LogEntry {
    fn new(level: Level, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
            success: false,
        }
    }

    fn success(message: impl Into<String>) -> Self {
        Self {
            level: Level::INFO,
            message: message.into(),
            success: true,
        }
    }

    fn style(&self) -> Style {
        if self.success {
            return Style::default().fg(Color::Green);
        }
        match self.level {
            Level::ERROR => Style::default().fg(Color::Red),
            Level::WARN => Style::default().fg(Color::Yellow),
            Level::INFO => Style::default().fg(Color::Cyan),
            Level::DEBUG => Style::default().fg(Color::Gray),
            Level::TRACE => Style::default().fg(Color::DarkGray),
        }
    }
}

impl Default for App {
    fn default() -> Self {
        Self {
            config_path: PathBuf::from(DEFAULT_CONFIG_PATH),
            mode: InputMode::Normal,
            screen: Screen::Main,
            api_token: String::new(),
            zone: String::new(),
            record_name: String::new(),
            record_type: RecordType::A,
            proxied: false,
            ttl: "1".to_string(),
            selected_field: 0,
            logs: vec![LogEntry::new(
                Level::INFO,
                "Welcome to CDDNS! Press 'e' to edit fields, Enter to update DNS.",
            )],
            current_ip: None,
            updating: false,
            last_result: None,
            log_state: ListState::default(),
            dirty: false,
        }
    }
}

impl App {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the config file path
    pub fn with_config_path(mut self, path: PathBuf) -> Self {
        self.config_path = path;
        self
    }

    /// Load config from file if it exists
    pub fn load_config(&mut self, config: &Config) {
        self.api_token = config.cloudflare.api_token.clone();
        if let Some(record) = config.records.first() {
            self.zone = record.zone.clone();
            self.record_name = record.name.clone();
            self.record_type = record.record_type;
            self.proxied = record.proxied;
            self.ttl = record.ttl.to_string();
        }
        self.dirty = false;
        self.log(
            Level::INFO,
            &format!("Loaded configuration from {}", self.config_path.display()),
        );
    }

    /// Build config from current app state
    fn build_config(&self) -> Result<Config> {
        let ttl: u32 = self.ttl.parse().unwrap_or(1);
        Config::from_args(
            self.api_token.clone(),
            self.zone.clone(),
            self.record_name.clone(),
            self.record_type,
            self.proxied,
            ttl,
            None,
        )
    }

    /// Save current config to file
    fn save_config(&mut self) -> Result<()> {
        let config = self.build_config()?;
        config.save(&self.config_path)?;
        self.dirty = false;
        self.log_success(&format!(
            "Configuration saved to {}",
            self.config_path.display()
        ));
        Ok(())
    }

    /// Mark config as dirty (has unsaved changes)
    fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    fn log(&mut self, level: Level, message: &str) {
        self.logs.push(LogEntry::new(level, message));
        self.scroll_logs_to_bottom();
    }

    fn log_success(&mut self, message: &str) {
        self.logs.push(LogEntry::success(message));
        self.scroll_logs_to_bottom();
    }

    fn scroll_logs_to_bottom(&mut self) {
        self.log_state
            .select(Some(self.logs.len().saturating_sub(1)));
    }

    fn field_count() -> usize {
        6 // api_token, zone, record_name, record_type, proxied, ttl
    }

    fn next_field(&mut self) {
        self.selected_field = (self.selected_field + 1) % Self::field_count();
    }

    fn prev_field(&mut self) {
        self.selected_field = if self.selected_field == 0 {
            Self::field_count() - 1
        } else {
            self.selected_field - 1
        };
    }

    fn toggle_record_type(&mut self) {
        self.record_type = match self.record_type {
            RecordType::A => RecordType::AAAA,
            RecordType::AAAA => RecordType::A,
        };
        self.mark_dirty();
    }

    fn toggle_proxied(&mut self) {
        self.proxied = !self.proxied;
        self.mark_dirty();
    }

    fn current_input(&mut self) -> Option<&mut String> {
        match self.selected_field {
            0 => Some(&mut self.api_token),
            1 => Some(&mut self.zone),
            2 => Some(&mut self.record_name),
            5 => Some(&mut self.ttl),
            _ => None,
        }
    }

    fn handle_char_input(&mut self, c: char) {
        if let Some(input) = self.current_input() {
            input.push(c);
            self.mark_dirty();
        } else {
            // Handle toggle fields
            match self.selected_field {
                3 => self.toggle_record_type(),
                4 => self.toggle_proxied(),
                _ => {}
            }
        }
    }

    fn handle_backspace(&mut self) {
        if let Some(input) = self.current_input() {
            if input.pop().is_some() {
                self.mark_dirty();
            }
        }
    }
}

/// Run the TUI application
pub async fn run(config_path: Option<PathBuf>) -> Result<()> {
    // Determine config path
    let config_path = config_path.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH));

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Create app state
    let mut app = App::new().with_config_path(config_path.clone());

    // Try to load existing config
    if config_path.exists() {
        match Config::load(&config_path) {
            Ok(config) => {
                app.load_config(&config);
            }
            Err(e) => {
                app.log(Level::WARN, &format!("Failed to load config: {}", e));
            }
        }
    } else {
        app.log(
            Level::INFO,
            &format!(
                "No config file found at {}. Will create on save.",
                config_path.display()
            ),
        );
    }

    // Run app
    let result = run_app(&mut terminal, &mut app).await;

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    result
}

async fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        terminal.draw(|f| ui(f, app))?;

        if event::poll(std::time::Duration::from_millis(100))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match app.screen {
                    Screen::Help => {
                        if matches!(
                            key.code,
                            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q')
                        ) {
                            app.screen = Screen::Main;
                        }
                    }
                    Screen::Main => match app.mode {
                        InputMode::Normal => match key.code {
                            KeyCode::Char('q') => return Ok(()),
                            KeyCode::Char('?') => app.screen = Screen::Help,
                            KeyCode::Char('e') | KeyCode::Char('i') => {
                                app.mode = InputMode::Editing;
                            }
                            KeyCode::Tab | KeyCode::Down | KeyCode::Char('j') => {
                                app.next_field();
                            }
                            KeyCode::BackTab | KeyCode::Up | KeyCode::Char('k') => {
                                app.prev_field();
                            }
                            KeyCode::Char(' ') => {
                                // Toggle for boolean fields
                                match app.selected_field {
                                    3 => app.toggle_record_type(),
                                    4 => app.toggle_proxied(),
                                    _ => {}
                                }
                            }
                            KeyCode::Enter => {
                                // Trigger update
                                if !app.updating {
                                    perform_update(app).await;
                                }
                            }
                            KeyCode::Char('d') => {
                                // Detect IP
                                detect_ip(app).await;
                            }
                            KeyCode::Char('s') => {
                                // Save config
                                if let Err(e) = app.save_config() {
                                    app.log(Level::ERROR, &format!("Failed to save config: {}", e));
                                }
                            }
                            _ => {}
                        },
                        InputMode::Editing => match key.code {
                            KeyCode::Esc => {
                                app.mode = InputMode::Normal;
                            }
                            KeyCode::Enter => {
                                app.mode = InputMode::Normal;
                            }
                            KeyCode::Tab => {
                                app.next_field();
                            }
                            KeyCode::BackTab => {
                                app.prev_field();
                            }
                            KeyCode::Char(c) => {
                                app.handle_char_input(c);
                            }
                            KeyCode::Backspace => {
                                app.handle_backspace();
                            }
                            _ => {}
                        },
                    },
                }
            }
        }
    }
}

async fn detect_ip(app: &mut App) {
    app.log(Level::INFO, "Detecting public IP...");

    let ip_url = match app.record_type {
        RecordType::A => crate::config::default_ipv4_url(),
        RecordType::AAAA => crate::config::default_ipv6_url(),
    };

    match get_public_ip(app.record_type, &ip_url, &ip_url).await {
        Ok(ip) => {
            app.current_ip = Some(ip);
            app.log_success(&format!("Detected IP: {}", ip));
        }
        Err(e) => {
            app.log(Level::ERROR, &format!("Failed to detect IP: {}", e));
        }
    }
}

async fn perform_update(app: &mut App) {
    app.updating = true;
    app.log(Level::INFO, "Starting DNS update...");

    // Build config
    let config = match app.build_config() {
        Ok(cfg) => cfg,
        Err(e) => {
            app.log(Level::ERROR, &format!("Invalid configuration: {}", e));
            app.updating = false;
            return;
        }
    };

    // Create client
    let client = match DdnsClient::new(&config.cloudflare.api_token) {
        Ok(c) => c,
        Err(e) => {
            app.log(Level::ERROR, &format!("Failed to create client: {}", e));
            app.updating = false;
            return;
        }
    };

    // Get IP
    let ip = match &app.current_ip {
        Some(ip) => *ip,
        None => {
            app.log(Level::INFO, "Detecting public IP...");
            let ip_url = match app.record_type {
                RecordType::A => crate::config::default_ipv4_url(),
                RecordType::AAAA => crate::config::default_ipv6_url(),
            };
            match get_public_ip(app.record_type, &ip_url, &ip_url).await {
                Ok(ip) => {
                    app.current_ip = Some(ip);
                    app.log_success(&format!("Detected IP: {}", ip));
                    ip
                }
                Err(e) => {
                    app.log(Level::ERROR, &format!("Failed to detect IP: {}", e));
                    app.updating = false;
                    return;
                }
            }
        }
    };

    // Update record
    let record = &config.records[0];
    match client.update_ddns(record, ip).await {
        Ok(result) => {
            let msg = match result {
                UpdateResult::Created => format!("Created new record: {} -> {}", record.name, ip),
                UpdateResult::Updated { old_ip, new_ip } => {
                    format!(
                        "Updated: {} ({} -> {})",
                        record.name,
                        old_ip
                            .map(|i| i.to_string())
                            .unwrap_or_else(|| "?".to_string()),
                        new_ip
                    )
                }
                UpdateResult::Unchanged => {
                    format!("Unchanged: {} already points to {}", record.name, ip)
                }
            };
            app.log_success(&msg);
            app.last_result = Some(msg);

            // Auto-save config after successful update
            if app.dirty {
                if let Err(e) = app.save_config() {
                    app.log(Level::WARN, &format!("Failed to auto-save config: {}", e));
                }
            }
        }
        Err(e) => {
            app.log(Level::ERROR, &format!("Update failed: {}", e));
        }
    }

    app.updating = false;
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Min(12),   // Form
            Constraint::Length(3), // Status
            Constraint::Min(6),    // Logs
            Constraint::Length(3), // Help
        ])
        .split(f.area());

    // Title with dirty indicator
    let title_text = if app.dirty {
        "Cloudflare DDNS Updater [*]"
    } else {
        "Cloudflare DDNS Updater"
    };
    let title = Paragraph::new(title_text)
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(title, chunks[0]);

    // Form
    render_form(f, app, chunks[1]);

    // Status bar
    let status_text = if app.updating {
        "Updating...".to_string()
    } else if let Some(ip) = app.current_ip {
        format!("Current IP: {}", ip)
    } else {
        "Press 'd' to detect IP".to_string()
    };
    let status = Paragraph::new(status_text)
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(status, chunks[2]);

    // Logs
    render_logs(f, app, chunks[3]);

    // Help bar
    let help_text = match app.mode {
        InputMode::Normal => {
            "q:Quit  e:Edit  Tab/j/k:Nav  Space:Toggle  Enter:Update  d:Detect IP  s:Save  ?:Help"
        }
        InputMode::Editing => "Esc:Stop editing  Tab:Next field  Enter:Done",
    };
    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(help, chunks[4]);

    // Help popup
    if app.screen == Screen::Help {
        render_help_popup(f);
    }
}

fn render_form(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("Configuration ({})", app.config_path.display()));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let fields = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
            Constraint::Length(2),
        ])
        .split(inner);

    let editing = app.mode == InputMode::Editing;

    // API Token (masked)
    let token_display = if app.api_token.is_empty() {
        "<not set>".to_string()
    } else {
        "*".repeat(app.api_token.len().min(40))
    };
    render_field(
        f,
        "API Token",
        &token_display,
        app.selected_field == 0,
        editing,
        fields[0],
    );

    // Zone
    render_field(
        f,
        "Zone",
        &app.zone,
        app.selected_field == 1,
        editing,
        fields[1],
    );

    // Record Name
    render_field(
        f,
        "Record Name",
        &app.record_name,
        app.selected_field == 2,
        editing,
        fields[2],
    );

    // Record Type (toggle)
    let type_display = format!("{} (Space to toggle)", app.record_type);
    render_field(
        f,
        "Record Type",
        &type_display,
        app.selected_field == 3,
        editing,
        fields[3],
    );

    // Proxied (toggle)
    let proxied_display = format!(
        "{} (Space to toggle)",
        if app.proxied { "Yes" } else { "No" }
    );
    render_field(
        f,
        "Proxied",
        &proxied_display,
        app.selected_field == 4,
        editing,
        fields[4],
    );

    // TTL
    render_field(
        f,
        "TTL",
        &app.ttl,
        app.selected_field == 5,
        editing,
        fields[5],
    );
}

fn render_field(
    f: &mut Frame,
    label: &str,
    value: &str,
    selected: bool,
    editing: bool,
    area: Rect,
) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(14), Constraint::Min(1)])
        .split(area);

    let label_style = if selected {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };

    let value_style = if selected && editing {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    } else if selected {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::White)
    };

    let cursor = if selected && editing { "_" } else { "" };

    let label_widget = Paragraph::new(format!("{}: ", label)).style(label_style);
    let value_widget = Paragraph::new(format!("{}{}", value, cursor)).style(value_style);

    f.render_widget(label_widget, chunks[0]);
    f.render_widget(value_widget, chunks[1]);
}

fn render_logs(f: &mut Frame, app: &App, area: Rect) {
    let items: Vec<ListItem> = app
        .logs
        .iter()
        .map(|entry| ListItem::new(Line::from(Span::styled(&entry.message, entry.style()))))
        .collect();

    let logs = List::new(items)
        .block(Block::default().borders(Borders::ALL).title("Logs"))
        .highlight_style(Style::default().add_modifier(Modifier::BOLD));

    f.render_stateful_widget(logs, area, &mut app.log_state.clone());
}

fn render_help_popup(f: &mut Frame) {
    let area = centered_rect(60, 70, f.area());
    f.render_widget(Clear, area);

    let help_text = vec![
        Line::from("Keyboard Shortcuts".bold().cyan()),
        Line::from(""),
        Line::from(vec![
            Span::styled("q", Style::default().fg(Color::Yellow)),
            Span::raw("          - Quit application"),
        ]),
        Line::from(vec![
            Span::styled("e / i", Style::default().fg(Color::Yellow)),
            Span::raw("      - Enter edit mode"),
        ]),
        Line::from(vec![
            Span::styled("Esc", Style::default().fg(Color::Yellow)),
            Span::raw("        - Exit edit mode"),
        ]),
        Line::from(vec![
            Span::styled("Tab / j", Style::default().fg(Color::Yellow)),
            Span::raw("    - Next field"),
        ]),
        Line::from(vec![
            Span::styled("Shift+Tab / k", Style::default().fg(Color::Yellow)),
            Span::raw(" - Previous field"),
        ]),
        Line::from(vec![
            Span::styled("Space", Style::default().fg(Color::Yellow)),
            Span::raw("      - Toggle selected option"),
        ]),
        Line::from(vec![
            Span::styled("Enter", Style::default().fg(Color::Yellow)),
            Span::raw("      - Update DNS record"),
        ]),
        Line::from(vec![
            Span::styled("d", Style::default().fg(Color::Yellow)),
            Span::raw("          - Detect current public IP"),
        ]),
        Line::from(vec![
            Span::styled("s", Style::default().fg(Color::Yellow)),
            Span::raw("          - Save configuration to file"),
        ]),
        Line::from(vec![
            Span::styled("?", Style::default().fg(Color::Yellow)),
            Span::raw("          - Toggle this help"),
        ]),
        Line::from(""),
        Line::from("Press any key to close".italic().dark_gray()),
    ];

    let help = Paragraph::new(Text::from(help_text))
        .block(Block::default().borders(Borders::ALL).title("Help"))
        .wrap(Wrap { trim: false });

    f.render_widget(help, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}
