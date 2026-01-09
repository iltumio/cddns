use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{broadcast, RwLock};
use tokio_cron_scheduler::{Job, JobScheduler};
use tracing::{error, info, warn};

use crate::cloudflare::{DdnsClient, UpdateResult};
use crate::config::Config;
use crate::ip::get_public_ip;
use crate::ipc::{Command, IpcServer, LogMessage, Response, ServiceStatus};

/// Shared state for the service
pub struct ServiceState {
    pub config: Config,
    pub config_path: PathBuf,
    pub last_update: Option<DateTime<Utc>>,
    pub last_result: Option<String>,
    pub current_ip: Option<IpAddr>,
    pub next_run: Option<DateTime<Utc>>,
    pub running: bool,
}

impl ServiceState {
    pub fn to_status(&self) -> ServiceStatus {
        ServiceStatus {
            running: self.running,
            cron: self.config.service.cron.clone(),
            last_update: self.last_update.map(|t| t.to_rfc3339()),
            last_result: self.last_result.clone(),
            current_ip: self.current_ip.map(|ip| ip.to_string()),
            record_count: self.config.records.len(),
            next_run: self.next_run.map(|t| t.to_rfc3339()),
        }
    }
}

/// Run the DDNS service with cron scheduling and IPC
pub async fn run(config_path: PathBuf) -> Result<()> {
    // Check if service is already running
    if crate::ipc::IpcConnection::is_service_running() {
        anyhow::bail!("Service is already running. Use 'cddns ui' to connect to it.");
    }

    info!("Starting CDDNS service...");

    // Load initial configuration
    let config = Config::load(&config_path)?;
    info!("Loaded configuration from {}", config_path.display());
    info!("Cron schedule: {}", config.service.cron);

    // Validate cron expression early
    let cron_expr = config.service.cron.clone();

    // Create shared state
    let state = Arc::new(RwLock::new(ServiceState {
        config,
        config_path: config_path.clone(),
        last_update: None,
        last_result: None,
        current_ip: None,
        next_run: None,
        running: true,
    }));

    // Create broadcast channel for logs
    let (log_tx, _) = broadcast::channel::<LogMessage>(100);

    // Create shutdown channel
    let (shutdown_tx, mut shutdown_rx) = broadcast::channel::<()>(1);

    // Start IPC server
    let ipc_server = IpcServer::new().await?;
    info!("IPC server listening on {}", ipc_server.path().display());

    // Spawn IPC handler
    let ipc_state = state.clone();
    let ipc_log_tx = log_tx.clone();
    let ipc_shutdown_tx = shutdown_tx.clone();
    let ipc_handle = tokio::spawn(async move {
        handle_ipc(ipc_server, ipc_state, ipc_log_tx, ipc_shutdown_tx).await;
    });

    // Run initial update if configured
    {
        let state_guard = state.read().await;
        if state_guard.config.service.run_on_start {
            info!("Running initial update...");
            drop(state_guard);
            let result = run_update(state.clone(), Some(log_tx.clone())).await;
            if let Err(e) = result {
                error!("Initial update failed: {}", e);
            }
        }
    }

    // Create scheduler
    let mut scheduler = JobScheduler::new()
        .await
        .context("Failed to create job scheduler")?;

    // Clone state for the job closure
    let job_state = state.clone();
    let job_log_tx = log_tx.clone();

    // Create the cron job
    let job = Job::new_async(cron_expr.as_str(), move |_uuid, _lock| {
        let state = job_state.clone();
        let log_tx = job_log_tx.clone();
        Box::pin(async move {
            info!("Cron triggered: running scheduled update...");

            // Reload config to pick up any changes
            {
                let mut state_guard = state.write().await;
                match Config::load(&state_guard.config_path) {
                    Ok(new_config) => {
                        state_guard.config = new_config;
                    }
                    Err(e) => {
                        warn!("Failed to reload config, using existing: {}", e);
                    }
                }
            }

            if let Err(e) = run_update(state, Some(log_tx)).await {
                error!("Scheduled update failed: {}", e);
            }
        })
    })
    .context("Failed to create cron job. Check your cron expression.")?;

    scheduler
        .add(job)
        .await
        .context("Failed to add job to scheduler")?;

    // Start the scheduler
    scheduler
        .start()
        .await
        .context("Failed to start scheduler")?;

    info!("Service started. Press Ctrl+C to stop.");

    // Wait for shutdown signal or IPC stop command
    tokio::select! {
        _ = wait_for_shutdown() => {
            info!("Received shutdown signal");
        }
        _ = shutdown_rx.recv() => {
            info!("Received stop command via IPC");
        }
    }

    // Mark as not running
    {
        let mut state_guard = state.write().await;
        state_guard.running = false;
    }

    info!("Shutting down service...");

    // Abort IPC handler
    ipc_handle.abort();

    scheduler
        .shutdown()
        .await
        .context("Failed to shutdown scheduler")?;

    info!("Service stopped.");
    Ok(())
}

/// Handle IPC connections
async fn handle_ipc(
    server: IpcServer,
    state: Arc<RwLock<ServiceState>>,
    log_tx: broadcast::Sender<LogMessage>,
    shutdown_tx: broadcast::Sender<()>,
) {
    loop {
        match server.accept().await {
            Ok(mut conn) => {
                let state = state.clone();
                let log_tx = log_tx.clone();
                let shutdown_tx = shutdown_tx.clone();

                tokio::spawn(async move {
                    loop {
                        match conn.receive_command().await {
                            Ok(cmd) => {
                                let response = match cmd {
                                    Command::Ping => Response::Pong,
                                    Command::GetStatus => {
                                        let state_guard = state.read().await;
                                        Response::Status(state_guard.to_status())
                                    }
                                    Command::TriggerUpdate => {
                                        // Reload config
                                        {
                                            let mut state_guard = state.write().await;
                                            if let Ok(new_config) =
                                                Config::load(&state_guard.config_path)
                                            {
                                                state_guard.config = new_config;
                                            }
                                        }

                                        // Run update
                                        let result =
                                            run_update(state.clone(), Some(log_tx.clone())).await;
                                        match result {
                                            Ok(_) => Response::UpdateResult {
                                                success: true,
                                                message: "Update completed successfully"
                                                    .to_string(),
                                            },
                                            Err(e) => Response::UpdateResult {
                                                success: false,
                                                message: e.to_string(),
                                            },
                                        }
                                    }
                                    Command::Stop => {
                                        let _ = shutdown_tx.send(());
                                        Response::Stopping
                                    }
                                };

                                if conn.send_response(&response).await.is_err() {
                                    break;
                                }

                                // If stopping, exit the loop
                                if matches!(response, Response::Stopping) {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
            }
            Err(e) => {
                error!("Failed to accept IPC connection: {}", e);
            }
        }
    }
}

/// Run a single update cycle
pub async fn run_update(
    state: Arc<RwLock<ServiceState>>,
    log_tx: Option<broadcast::Sender<LogMessage>>,
) -> Result<()> {
    let config = {
        let state_guard = state.read().await;
        state_guard.config.clone()
    };

    let client = DdnsClient::new(&config.cloudflare.api_token)?;

    let mut success_count = 0;
    let mut error_count = 0;
    let mut last_ip = None;
    let mut results = Vec::new();

    for record in &config.records {
        let msg = format!("Processing {} record: {}", record.record_type, record.name);
        info!("{}", msg);
        send_log(&log_tx, "INFO", &msg);

        // Get the IP to use
        let ip = match config.settings.force_ip {
            Some(ip) => {
                let msg = format!("Using forced IP: {}", ip);
                info!("{}", msg);
                send_log(&log_tx, "INFO", &msg);
                ip
            }
            None => {
                match get_public_ip(
                    record.record_type,
                    &config.settings.ipv4_url,
                    &config.settings.ipv6_url,
                )
                .await
                {
                    Ok(ip) => {
                        last_ip = Some(ip);
                        ip
                    }
                    Err(e) => {
                        let msg = format!("Failed to get public IP for {}: {}", record.name, e);
                        error!("{}", msg);
                        send_log(&log_tx, "ERROR", &msg);
                        error_count += 1;
                        continue;
                    }
                }
            }
        };

        // Update the record
        match client.update_ddns(record, ip).await {
            Ok(result) => {
                let msg = match &result {
                    UpdateResult::Created => {
                        format!("Created new record: {} -> {}", record.name, ip)
                    }
                    UpdateResult::Updated { old_ip, new_ip } => {
                        format!(
                            "Updated record: {} ({} -> {})",
                            record.name,
                            old_ip
                                .map(|ip| ip.to_string())
                                .unwrap_or_else(|| "unknown".to_string()),
                            new_ip
                        )
                    }
                    UpdateResult::Unchanged => {
                        format!("Record unchanged: {} already points to {}", record.name, ip)
                    }
                };
                info!("{}", msg);
                send_log(&log_tx, "INFO", &msg);
                results.push(msg);
                success_count += 1;
            }
            Err(e) => {
                let msg = format!("Failed to update {}: {}", record.name, e);
                error!("{}", msg);
                send_log(&log_tx, "ERROR", &msg);
                error_count += 1;
            }
        }
    }

    let summary = format!(
        "Update cycle completed: {} successful, {} failed",
        success_count, error_count
    );
    info!("{}", summary);
    send_log(&log_tx, "INFO", &summary);

    // Update state
    {
        let mut state_guard = state.write().await;
        state_guard.last_update = Some(Utc::now());
        state_guard.last_result = Some(if error_count > 0 {
            format!("{} failed", error_count)
        } else {
            "Success".to_string()
        });
        if let Some(ip) = last_ip {
            state_guard.current_ip = Some(ip);
        }
    }

    if error_count > 0 {
        anyhow::bail!("{} record(s) failed to update", error_count);
    }

    Ok(())
}

fn send_log(log_tx: &Option<broadcast::Sender<LogMessage>>, level: &str, message: &str) {
    if let Some(tx) = log_tx {
        let _ = tx.send(LogMessage {
            level: level.to_string(),
            message: message.to_string(),
            timestamp: Local::now().to_rfc3339(),
        });
    }
}

/// Wait for shutdown signal (Ctrl+C)
async fn wait_for_shutdown() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};

        let mut sigterm =
            signal(SignalKind::terminate()).expect("Failed to create SIGTERM handler");
        let mut sigint = signal(SignalKind::interrupt()).expect("Failed to create SIGINT handler");

        tokio::select! {
            _ = sigterm.recv() => {
                info!("Received SIGTERM");
            }
            _ = sigint.recv() => {
                info!("Received SIGINT");
            }
        }
    }

    #[cfg(windows)]
    {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl+C");
        info!("Received Ctrl+C");
    }
}
