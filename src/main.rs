mod cloudflare;
mod config;
mod ip;

use anyhow::Result;
use clap::Parser;
use std::path::PathBuf;
use tracing::{error, info, warn, Level};
use tracing_subscriber::EnvFilter;

use crate::cloudflare::{DdnsClient, UpdateResult};
use crate::config::Config;
use crate::ip::get_public_ip;

/// Cloudflare DDNS Updater
#[derive(Parser, Debug)]
#[command(name = "cddns")]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to the configuration file
    #[arg(short, long, default_value = "config.toml")]
    config: PathBuf,

    /// Run in dry-run mode (don't actually update records)
    #[arg(short = 'n', long)]
    dry_run: bool,

    /// Verbose output
    #[arg(short, long)]
    verbose: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Initialize logging
    let filter = if args.verbose {
        EnvFilter::from_default_env().add_directive(Level::DEBUG.into())
    } else {
        EnvFilter::from_default_env().add_directive(Level::INFO.into())
    };

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();

    // Load configuration
    info!("Loading configuration from: {}", args.config.display());
    let config = Config::load(&args.config)?;

    if args.dry_run {
        warn!("Running in dry-run mode - no changes will be made");
    }

    // Create Cloudflare client
    let client = DdnsClient::new(&config.cloudflare.api_token)?;

    // Process each record
    let mut success_count = 0;
    let mut error_count = 0;

    for record in &config.records {
        info!(
            "Processing {} record: {}",
            record.record_type, record.name
        );

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

        if args.dry_run {
            info!(
                "[DRY-RUN] Would update {} to {}",
                record.name, ip
            );
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
                            old_ip.map(|ip| ip.to_string()).unwrap_or_else(|| "unknown".to_string()),
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
