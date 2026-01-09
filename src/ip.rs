use anyhow::{Context, Result};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use tracing::{debug, info};

use crate::config::RecordType;

/// Fetches the current public IP address
pub async fn get_public_ip(record_type: RecordType, ipv4_url: &str, ipv6_url: &str) -> Result<IpAddr> {
    match record_type {
        RecordType::A => {
            let ip = get_public_ipv4(ipv4_url).await?;
            Ok(IpAddr::V4(ip))
        }
        RecordType::AAAA => {
            let ip = get_public_ipv6(ipv6_url).await?;
            Ok(IpAddr::V6(ip))
        }
    }
}

/// Fetches the current public IPv4 address
async fn get_public_ipv4(url: &str) -> Result<Ipv4Addr> {
    debug!("Fetching public IPv4 from {}", url);

    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch public IPv4 from {}", url))?;

    let ip_str = response
        .text()
        .await
        .context("Failed to read IPv4 response body")?;

    let ip: Ipv4Addr = ip_str
        .trim()
        .parse()
        .with_context(|| format!("Failed to parse IPv4 address: {}", ip_str.trim()))?;

    info!("Detected public IPv4: {}", ip);
    Ok(ip)
}

/// Fetches the current public IPv6 address
async fn get_public_ipv6(url: &str) -> Result<Ipv6Addr> {
    debug!("Fetching public IPv6 from {}", url);

    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("Failed to fetch public IPv6 from {}", url))?;

    let ip_str = response
        .text()
        .await
        .context("Failed to read IPv6 response body")?;

    let ip: Ipv6Addr = ip_str
        .trim()
        .parse()
        .with_context(|| format!("Failed to parse IPv6 address: {}", ip_str.trim()))?;

    info!("Detected public IPv6: {}", ip);
    Ok(ip)
}
