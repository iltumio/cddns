mod cloudflare;
mod config;
mod ip;
mod tui;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::net::IpAddr;
use std::path::PathBuf;
use tracing::{error, info, warn, Level};
use tracing_subscriber::EnvFilter;

use crate::cloudflare::{DdnsClient, UpdateResult};
use crate::config::{Config, RecordType};
use crate::ip::get_public_ip;

/// Cloudflare DDNS Updater
#[derive(Parser, Debug)]
#[command(name = "cddns")]
#[command(author, version, about = "A simple Cloudflare DDNS updater", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Verbose output
    #[arg(short, long, global = true)]
    verbose: bool,
}

#[derive(Subcommand, Debug)]
enum Commands {
    /// Update DNS record using a config file
    Config {
        /// Path to the configuration file
        #[arg(short, long, default_value = "config.toml")]
        file: PathBuf,

        /// Run in dry-run mode (don't actually update records)
        #[arg(short = 'n', long)]
        dry_run: bool,
    },

    /// Update DNS record using command-line arguments
    Update {
        /// Cloudflare API token
        #[arg(short = 't', long, env = "CF_API_TOKEN")]
        api_token: String,

        /// Zone name (e.g., "example.com")
        #[arg(short, long)]
        zone: String,

        /// Full DNS record name (e.g., "home.example.com")
        #[arg(short, long)]
        record: String,

        /// Record type: A (IPv4) or AAAA (IPv6)
        #[arg(short = 'T', long, default_value = "A")]
        record_type: RecordType,

        /// Enable Cloudflare proxy for this record
        #[arg(short, long)]
        proxied: bool,

        /// TTL in seconds (1 = automatic)
        #[arg(long, default_value = "1")]
        ttl: u32,

        /// Force a specific IP instead of auto-detecting
        #[arg(short, long)]
        ip: Option<IpAddr>,

        /// Run in dry-run mode (don't actually update records)
        #[arg(short = 'n', long)]
        dry_run: bool,
    },

    /// Open the interactive TUI
    Ui {
        /// Optional: Load config file into TUI
        #[arg(short, long)]
        config: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // For TUI mode, don't initialize logging (it interferes with the terminal)
    let is_tui = matches!(cli.command, Some(Commands::Ui { .. }));

    if !is_tui {
        // Initialize logging
        let filter = if cli.verbose {
            EnvFilter::from_default_env().add_directive(Level::DEBUG.into())
        } else {
            EnvFilter::from_default_env().add_directive(Level::INFO.into())
        };

        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
    }

    match cli.command {
        Some(Commands::Config { file, dry_run }) => run_with_config(&file, dry_run).await,
        Some(Commands::Update {
            api_token,
            zone,
            record,
            record_type,
            proxied,
            ttl,
            ip,
            dry_run,
        }) => {
            run_with_args(
                api_token,
                zone,
                record,
                record_type,
                proxied,
                ttl,
                ip,
                dry_run,
            )
            .await
        }
        Some(Commands::Ui { config }) => {
            // Pass the config path to TUI - it will handle loading/saving
            tui::run(config).await
        }
        None => {
            // Default behavior: try to load config.toml
            if PathBuf::from("config.toml").exists() {
                run_with_config(&PathBuf::from("config.toml"), false).await
            } else {
                eprintln!("No config file found. Use one of the following:");
                eprintln!("  cddns config -f <config.toml>  - Use a config file");
                eprintln!("  cddns update -t <token> -z <zone> -r <record>  - Use CLI arguments");
                eprintln!("  cddns ui  - Open interactive TUI");
                eprintln!("\nRun 'cddns --help' for more information.");
                std::process::exit(1);
            }
        }
    }
}

async fn run_with_config(path: &PathBuf, dry_run: bool) -> Result<()> {
    info!("Loading configuration from: {}", path.display());
    let config = Config::load(path)?;

    if dry_run {
        warn!("Running in dry-run mode - no changes will be made");
    }

    run_update(&config, dry_run).await
}

async fn run_with_args(
    api_token: String,
    zone: String,
    record: String,
    record_type: RecordType,
    proxied: bool,
    ttl: u32,
    force_ip: Option<IpAddr>,
    dry_run: bool,
) -> Result<()> {
    info!("Updating {} record: {}", record_type, record);

    let config = Config::from_args(api_token, zone, record, record_type, proxied, ttl, force_ip)?;

    if dry_run {
        warn!("Running in dry-run mode - no changes will be made");
    }

    run_update(&config, dry_run).await
}

async fn run_update(config: &Config, dry_run: bool) -> Result<()> {
    // Create Cloudflare client
    let client = DdnsClient::new(&config.cloudflare.api_token)?;

    // Process each record
    let mut success_count = 0;
    let mut error_count = 0;

    for record in &config.records {
        info!("Processing {} record: {}", record.record_type, record.name);

        // Get the IP to use
        let ip = match config.settings.force_ip {
            Some(ip) => {
                info!("Using forced IP: {}", ip);
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
                    Ok(ip) => ip,
                    Err(e) => {
                        error!("Failed to get public IP for {}: {}", record.name, e);
                        error_count += 1;
                        continue;
                    }
                }
            }
        };

        if dry_run {
            info!("[DRY-RUN] Would update {} to {}", record.name, ip);
            success_count += 1;
            continue;
        }

        // Update the record
        match client.update_ddns(record, ip).await {
            Ok(result) => {
                match result {
                    UpdateResult::Created => {
                        info!("Created new record: {} -> {}", record.name, ip);
                    }
                    UpdateResult::Updated { old_ip, new_ip } => {
                        info!(
                            "Updated record: {} ({} -> {})",
                            record.name,
                            old_ip
                                .map(|ip| ip.to_string())
                                .unwrap_or_else(|| "unknown".to_string()),
                            new_ip
                        );
                    }
                    UpdateResult::Unchanged => {
                        info!("Record unchanged: {} already points to {}", record.name, ip);
                    }
                }
                success_count += 1;
            }
            Err(e) => {
                error!("Failed to update {}: {}", record.name, e);
                error_count += 1;
            }
        }
    }

    // Summary
    info!(
        "Completed: {} successful, {} failed",
        success_count, error_count
    );

    if error_count > 0 {
        std::process::exit(1);
    }

    Ok(())
}
