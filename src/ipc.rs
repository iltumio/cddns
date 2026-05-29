use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// Get the socket path for IPC
pub fn socket_path() -> PathBuf {
    dirs::runtime_dir()
        .or_else(dirs::state_dir)
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

    /// Check if a service is actually running.
    ///
    /// We probe the socket by connecting rather than only checking that the
    /// socket file exists. The socket file is only removed on a graceful
    /// shutdown (`Drop for IpcServer`); an unclean termination (SIGKILL, OOM,
    /// panic, `docker restart`, host reboot, or systemd's stop-timeout
    /// escalating to SIGKILL) leaves a *stale* socket behind. Checking
    /// existence alone would treat that stale file as a live service and refuse
    /// to start, which under an auto-restarting supervisor becomes an endless
    /// "Service is already running" loop. A connect attempt distinguishes the
    /// two: a real listener accepts, a stale socket is refused — in which case
    /// we remove it so the next bind succeeds.
    pub fn is_service_running() -> bool {
        use std::os::unix::net::UnixStream;

        let path = socket_path();
        if !path.exists() {
            return false;
        }

        match UnixStream::connect(&path) {
            Ok(_) => true,
            Err(_) => {
                // Stale socket from an unclean shutdown — clean it up.
                std::fs::remove_file(&path).ok();
                false
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    /// Point `socket_path()` at an isolated temp directory for the duration of
    /// a test. `dirs::runtime_dir()` honours `$XDG_RUNTIME_DIR` on Linux.
    fn isolate_socket_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("cddns-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &dir);
        dir
    }

    #[test]
    fn is_service_running_handles_missing_stale_and_live_sockets() {
        let _dir = isolate_socket_dir();
        let path = socket_path();
        std::fs::remove_file(&path).ok();

        // No socket file at all -> not running.
        assert!(!IpcConnection::is_service_running());

        // A stale socket file with no listener (e.g. left by a SIGKILL or
        // reboot) -> not running, and the stale file is cleaned up.
        std::fs::write(&path, b"").unwrap();
        assert!(path.exists());
        assert!(!IpcConnection::is_service_running());
        assert!(
            !path.exists(),
            "stale socket should be removed so the next bind can succeed"
        );

        // A real listener bound to the socket -> running.
        let _listener = UnixListener::bind(&path).unwrap();
        assert!(IpcConnection::is_service_running());

        std::fs::remove_file(&path).ok();
    }
}
