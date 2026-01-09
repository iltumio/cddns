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
use std::process::Stdio;
use tracing::Level;

use crate::cloudflare::{DdnsClient, UpdateResult};
use crate::config::{Config, RecordType};
use crate::ip::get_public_ip;
use crate::ipc::{self, Command, IpcConnection, Response, ServiceStatus};

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
    /// Cron expression
    cron: String,
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
    /// Service status
    service_status: Option<ServiceStatus>,
    /// Whether we're connected to a running service
    connected_to_service: bool,
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
            cron: "0 */5 * * * *".to_string(),
            selected_field: 0,
            logs: vec![LogEntry::new(
                Level::INFO,
                "Welcome to CDDNS! Press '?' for help.",
            )],
            current_ip: None,
            updating: false,
            last_result: None,
            log_state: ListState::default(),
            dirty: false,
            service_status: None,
            connected_to_service: false,
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
        self.cron = config.service.cron.clone();
        self.dirty = false;
        self.log(
            Level::INFO,
            &format!("Loaded configuration from {}", self.config_path.display()),
        );
    }

    /// Build config from current app state
    fn build_config(&self) -> Result<Config> {
        let ttl: u32 = self.ttl.parse().unwrap_or(1);
        let mut config = Config::from_args(
            self.api_token.clone(),
            self.zone.clone(),
            self.record_name.clone(),
            self.record_type,
            self.proxied,
            ttl,
            None,
        )?;
        config.service.cron = self.cron.clone();
        Ok(config)
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
        7 // api_token, zone, record_name, record_type, proxied, ttl, cron
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
            6 => Some(&mut self.cron),
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

    /// Check if service is running and update status
    async fn refresh_service_status(&mut self) {
        if IpcConnection::is_service_running() {
            match ipc::send_command(Command::GetStatus).await {
                Ok(Response::Status(status)) => {
                    self.service_status = Some(status);
                    self.connected_to_service = true;
                }
                _ => {
                    self.service_status = None;
                    self.connected_to_service = false;
                }
            }
        } else {
            self.service_status = None;
            self.connected_to_service = false;
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

    // Check if service is running
    app.refresh_service_status().await;
    if app.connected_to_service {
        app.log_success("Connected to running service");
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
    let mut last_status_refresh = std::time::Instant::now();

    loop {
        // Periodically refresh service status
        if last_status_refresh.elapsed() > std::time::Duration::from_secs(2) {
            app.refresh_service_status().await;
            last_status_refresh = std::time::Instant::now();
        }

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
                            KeyCode::Char('d') => {
                                // Detach - quit TUI but keep service running
                                if app.connected_to_service {
                                    app.log(Level::INFO, "Detaching from service...");
                                    return Ok(());
                                } else {
                                    app.log(
                                        Level::WARN,
                                        "No service running. Use 'S' to start service first.",
                                    );
                                }
                            }
                            KeyCode::Char('?') => app.screen = Screen::Help,
                            KeyCode::Char('e') => {
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
                            KeyCode::Enter | KeyCode::Char('u') => {
                                // Trigger update
                                if !app.updating {
                                    if app.connected_to_service {
                                        // Send update command to service
                                        trigger_service_update(app).await;
                                    } else {
                                        perform_update(app).await;
                                    }
                                }
                            }
                            KeyCode::Char('i') => {
                                // Detect IP
                                detect_ip(app).await;
                            }
                            KeyCode::Char('s') => {
                                // Save config
                                if let Err(e) = app.save_config() {
                                    app.log(Level::ERROR, &format!("Failed to save config: {}", e));
                                }
                            }
                            KeyCode::Char('S') => {
                                // Start service
                                if !app.connected_to_service {
                                    start_service(app).await;
                                } else {
                                    app.log(Level::WARN, "Service is already running");
                                }
                            }
                            KeyCode::Char('X') => {
                                // Stop service
                                if app.connected_to_service {
                                    stop_service(app).await;
                                } else {
                                    app.log(Level::WARN, "Service is not running");
                                }
                            }
                            KeyCode::Char('r') => {
                                // Refresh service status
                                app.refresh_service_status().await;
                                if app.connected_to_service {
                                    app.log(Level::INFO, "Service status refreshed");
                                } else {
                                    app.log(Level::INFO, "Service is not running");
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

async fn trigger_service_update(app: &mut App) {
    app.log(Level::INFO, "Triggering service update...");

    match ipc::send_command(Command::TriggerUpdate).await {
        Ok(Response::UpdateResult { success, message }) => {
            if success {
                app.log_success(&message);
            } else {
                app.log(Level::ERROR, &message);
            }
            app.refresh_service_status().await;
        }
        Ok(_) => {
            app.log(Level::ERROR, "Unexpected response from service");
        }
        Err(e) => {
            app.log(
                Level::ERROR,
                &format!("Failed to communicate with service: {}", e),
            );
        }
    }
}

async fn start_service(app: &mut App) {
    // Save config first
    if app.dirty {
        if let Err(e) = app.save_config() {
            app.log(Level::ERROR, &format!("Failed to save config: {}", e));
            return;
        }
    }

    app.log(Level::INFO, "Starting service...");

    // Get the path to the current executable
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            app.log(
                Level::ERROR,
                &format!("Failed to get executable path: {}", e),
            );
            return;
        }
    };

    // Spawn service in background
    let result = std::process::Command::new(&exe)
        .arg("service")
        .arg("-c")
        .arg(&app.config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();

    match result {
        Ok(_) => {
            app.log_success("Service started in background");
            // Wait a moment for service to start
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            app.refresh_service_status().await;
        }
        Err(e) => {
            app.log(Level::ERROR, &format!("Failed to start service: {}", e));
        }
    }
}

async fn stop_service(app: &mut App) {
    app.log(Level::INFO, "Stopping service...");

    match ipc::send_command(Command::Stop).await {
        Ok(Response::Stopping) => {
            app.log_success("Service is stopping...");
            // Wait a moment for service to stop
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            app.refresh_service_status().await;
        }
        Ok(_) => {
            app.log(Level::ERROR, "Unexpected response from service");
        }
        Err(e) => {
            app.log(Level::ERROR, &format!("Failed to stop service: {}", e));
        }
    }
}

fn ui(f: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3), // Title
            Constraint::Length(5), // Service status
            Constraint::Min(14),   // Form
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

    // Service status
    render_service_status(f, app, chunks[1]);

    // Form
    render_form(f, app, chunks[2]);

    // Status bar
    let status_text = if app.updating {
        "Updating...".to_string()
    } else if let Some(ip) = app.current_ip {
        format!("Current IP: {}", ip)
    } else {
        "Press 'i' to detect IP".to_string()
    };
    let status = Paragraph::new(status_text)
        .style(Style::default().fg(Color::Yellow))
        .block(Block::default().borders(Borders::ALL).title("Status"));
    f.render_widget(status, chunks[3]);

    // Logs
    render_logs(f, app, chunks[4]);

    // Help bar
    let help_text = match app.mode {
        InputMode::Normal => {
            if app.connected_to_service {
                "q:Quit d:Detach e:Edit i:IP u:Update s:Save S:Start X:Stop r:Refresh ?:Help"
            } else {
                "q:Quit e:Edit i:IP u:Update s:Save S:Start ?:Help"
            }
        }
        InputMode::Editing => "Esc:Done Tab:Next",
    };
    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::DarkGray))
        .block(Block::default().borders(Borders::ALL));
    f.render_widget(help, chunks[5]);

    // Help popup
    if app.screen == Screen::Help {
        render_help_popup(f);
    }
}

fn render_service_status(f: &mut Frame, app: &App, area: Rect) {
    let (status_text, status_color) = if let Some(ref status) = app.service_status {
        let lines = vec![
            Line::from(vec![
                Span::styled("Status: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    if status.running { "Running" } else { "Stopped" },
                    Style::default().fg(if status.running {
                        Color::Green
                    } else {
                        Color::Red
                    }),
                ),
                Span::raw("  "),
                Span::styled("Cron: ", Style::default().fg(Color::Gray)),
                Span::styled(&status.cron, Style::default().fg(Color::White)),
            ]),
            Line::from(vec![
                Span::styled("Last update: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    status.last_update.as_deref().unwrap_or("Never"),
                    Style::default().fg(Color::White),
                ),
                Span::raw("  "),
                Span::styled("Result: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    status.last_result.as_deref().unwrap_or("-"),
                    Style::default().fg(Color::White),
                ),
            ]),
            Line::from(vec![
                Span::styled("IP: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    status.current_ip.as_deref().unwrap_or("Unknown"),
                    Style::default().fg(Color::Cyan),
                ),
                Span::raw("  "),
                Span::styled("Records: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    status.record_count.to_string(),
                    Style::default().fg(Color::White),
                ),
            ]),
        ];
        (Text::from(lines), Color::Green)
    } else {
        (
            Text::from(Line::from(vec![
                Span::styled("Status: ", Style::default().fg(Color::Gray)),
                Span::styled("Not running", Style::default().fg(Color::Red)),
                Span::raw("  (Press 'S' to start)"),
            ])),
            Color::Red,
        )
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled("Service", Style::default().fg(status_color)));

    let paragraph = Paragraph::new(status_text).block(block);
    f.render_widget(paragraph, area);
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
    let type_display = format!("{} (Space)", app.record_type);
    render_field(
        f,
        "Record Type",
        &type_display,
        app.selected_field == 3,
        editing,
        fields[3],
    );

    // Proxied (toggle)
    let proxied_display = format!("{} (Space)", if app.proxied { "Yes" } else { "No" });
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

    // Cron
    render_field(
        f,
        "Cron",
        &app.cron,
        app.selected_field == 6,
        editing,
        fields[6],
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
    let area = centered_rect(70, 80, f.area());
    f.render_widget(Clear, area);

    let help_text = vec![
        Line::from("Keyboard Shortcuts".bold().cyan()),
        Line::from(""),
        Line::from("Navigation".bold().yellow()),
        Line::from(vec![
            Span::styled("  Tab / j / Down", Style::default().fg(Color::Cyan)),
            Span::raw("  - Next field"),
        ]),
        Line::from(vec![
            Span::styled("  Shift+Tab / k / Up", Style::default().fg(Color::Cyan)),
            Span::raw("  - Previous field"),
        ]),
        Line::from(vec![
            Span::styled("  Space", Style::default().fg(Color::Cyan)),
            Span::raw("  - Toggle selected option"),
        ]),
        Line::from(""),
        Line::from("Actions".bold().yellow()),
        Line::from(vec![
            Span::styled("  e", Style::default().fg(Color::Cyan)),
            Span::raw("  - Enter edit mode"),
        ]),
        Line::from(vec![
            Span::styled("  i", Style::default().fg(Color::Cyan)),
            Span::raw("  - Detect current public IP"),
        ]),
        Line::from(vec![
            Span::styled("  u / Enter", Style::default().fg(Color::Cyan)),
            Span::raw("  - Update DNS record"),
        ]),
        Line::from(vec![
            Span::styled("  s", Style::default().fg(Color::Cyan)),
            Span::raw("  - Save configuration"),
        ]),
        Line::from(""),
        Line::from("Service Control".bold().yellow()),
        Line::from(vec![
            Span::styled("  S", Style::default().fg(Color::Cyan)),
            Span::raw("  - Start background service"),
        ]),
        Line::from(vec![
            Span::styled("  X", Style::default().fg(Color::Cyan)),
            Span::raw("  - Stop background service"),
        ]),
        Line::from(vec![
            Span::styled("  r", Style::default().fg(Color::Cyan)),
            Span::raw("  - Refresh service status"),
        ]),
        Line::from(vec![
            Span::styled("  d", Style::default().fg(Color::Cyan)),
            Span::raw("  - Detach (quit TUI, keep service)"),
        ]),
        Line::from(""),
        Line::from("Other".bold().yellow()),
        Line::from(vec![
            Span::styled("  q", Style::default().fg(Color::Cyan)),
            Span::raw("  - Quit application"),
        ]),
        Line::from(vec![
            Span::styled("  ?", Style::default().fg(Color::Cyan)),
            Span::raw("  - Toggle this help"),
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
