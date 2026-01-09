use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Get the socket path for IPC
pub fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .or_else(|| dirs::state_dir())
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("cddns.sock")
}

/// Commands that can be sent to the service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Command {
    /// Get current service status
    GetStatus,
    /// Trigger an immediate update
    TriggerUpdate,
    /// Stop the service
    Stop,
    /// Ping to check if service is alive
    Ping,
}

/// Responses from the service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Service status information
    Status(ServiceStatus),
    /// Update triggered successfully
    UpdateTriggered,
    /// Update completed with result
    UpdateResult { success: bool, message: String },
    /// Service is stopping
    Stopping,
    /// Pong response
    Pong,
    /// Error occurred
    Error(String),
    /// Log message from service
    Log(LogMessage),
}

/// Current service status
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceStatus {
    /// Whether the service is running
    pub running: bool,
    /// Current cron schedule
    pub cron: String,
    /// Last update time (ISO 8601)
    pub last_update: Option<String>,
    /// Last update result
    pub last_result: Option<String>,
    /// Current detected IP
    pub current_ip: Option<String>,
    /// Number of records configured
    pub record_count: usize,
    /// Next scheduled run (ISO 8601)
    pub next_run: Option<String>,
}

/// Log message from service
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogMessage {
    pub level: String,
    pub message: String,
    pub timestamp: String,
}

/// IPC Server for the service
pub struct IpcServer {
    listener: UnixListener,
}

impl IpcServer {
    /// Create a new IPC server
    pub async fn new() -> Result<Self> {
        let path = socket_path();

        // Remove existing socket if present
        if path.exists() {
            std::fs::remove_file(&path).ok();
        }

        // Create parent directory if needed
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }

        let listener = UnixListener::bind(&path)
            .with_context(|| format!("Failed to bind to socket: {}", path.display()))?;

        Ok(Self { listener })
    }

    /// Accept a new connection
    pub async fn accept(&self) -> Result<IpcConnection> {
        let (stream, _) = self.listener.accept().await?;
        Ok(IpcConnection::new(stream))
    }

    /// Get the socket path
    pub fn path(&self) -> PathBuf {
        socket_path()
    }
}

impl Drop for IpcServer {
    fn drop(&mut self) {
        // Clean up socket file
        let path = socket_path();
        std::fs::remove_file(&path).ok();
    }
}

/// IPC Connection (used by both server and client)
pub struct IpcConnection {
    stream: BufReader<UnixStream>,
}

impl IpcConnection {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream: BufReader::new(stream),
        }
    }

    /// Connect to the service
    pub async fn connect() -> Result<Self> {
        let path = socket_path();
        let stream = UnixStream::connect(&path)
            .await
            .with_context(|| format!("Failed to connect to socket: {}", path.display()))?;
        Ok(Self::new(stream))
    }

    /// Check if service is running
    pub fn is_service_running() -> bool {
        let path = socket_path();
        path.exists()
    }

    /// Send a command
    pub async fn send_command(&mut self, cmd: &Command) -> Result<()> {
        let json = serde_json::to_string(cmd)?;
        self.stream.get_mut().write_all(json.as_bytes()).await?;
        self.stream.get_mut().write_all(b"\n").await?;
        self.stream.get_mut().flush().await?;
        Ok(())
    }

    /// Receive a response
    pub async fn receive_response(&mut self) -> Result<Response> {
        let mut line = String::new();
        self.stream.read_line(&mut line).await?;
        let response: Response = serde_json::from_str(&line)?;
        Ok(response)
    }

    /// Send a response (for server side)
    pub async fn send_response(&mut self, resp: &Response) -> Result<()> {
        let json = serde_json::to_string(resp)?;
        self.stream.get_mut().write_all(json.as_bytes()).await?;
        self.stream.get_mut().write_all(b"\n").await?;
        self.stream.get_mut().flush().await?;
        Ok(())
    }

    /// Receive a command (for server side)
    pub async fn receive_command(&mut self) -> Result<Command> {
        let mut line = String::new();
        self.stream.read_line(&mut line).await?;
        let cmd: Command = serde_json::from_str(&line)?;
        Ok(cmd)
    }
}

/// Client helper to send a command and get a response
pub async fn send_command(cmd: Command) -> Result<Response> {
    let mut conn = IpcConnection::connect().await?;
    conn.send_command(&cmd).await?;
    conn.receive_response().await
}
