use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::net::IpAddr;
use std::path::Path;

/// Main configuration structure
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct Config {
    /// Cloudflare API configuration
    pub cloudflare: CloudflareConfig,
    /// DNS records to update
    pub records: Vec<RecordConfig>,
    /// Optional settings
    #[serde(default)]
    pub settings: Settings,
    /// Service mode settings
    #[serde(default)]
    pub service: ServiceConfig,
}

/// Cloudflare authentication configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct CloudflareConfig {
    /// API token (recommended) - requires Zone:Read and DNS:Edit permissions
    pub api_token: String,
}

/// DNS record configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct RecordConfig {
    /// The zone name (e.g., "example.com")
    pub zone: String,
    /// The full record name (e.g., "home.example.com")
    pub name: String,
    /// Record type: "A" for IPv4, "AAAA" for IPv6
    #[serde(default = "default_record_type")]
    pub record_type: RecordType,
    /// Whether the record should be proxied through Cloudflare
    #[serde(default)]
    pub proxied: bool,
    /// TTL in seconds (1 = automatic)
    #[serde(default = "default_ttl")]
    pub ttl: u32,
}

/// Optional settings
#[derive(Debug, Deserialize, Serialize, Default, Clone)]
pub struct Settings {
    /// URL to fetch public IPv4 address
    #[serde(default = "default_ipv4_url")]
    pub ipv4_url: String,
    /// URL to fetch public IPv6 address
    #[serde(default = "default_ipv6_url")]
    pub ipv6_url: String,
    /// Optional: Force a specific IP instead of auto-detecting
    #[serde(skip_serializing_if = "Option::is_none")]
    pub force_ip: Option<IpAddr>,
}

/// Service mode configuration
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct ServiceConfig {
    /// Cron expression for scheduling updates (e.g., "*/5 * * * *" for every 5 minutes)
    #[serde(default = "default_cron")]
    pub cron: String,
    /// Whether to run an update immediately on service start
    #[serde(default = "default_run_on_start")]
    pub run_on_start: bool,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            cron: default_cron(),
            run_on_start: default_run_on_start(),
        }
    }
}

fn default_cron() -> String {
    "*/5 * * * *".to_string() // Every 5 minutes
}

fn default_run_on_start() -> bool {
    true
}

/// Supported DNS record types for DDNS
#[allow(clippy::upper_case_acronyms)]
#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Default)]
pub enum RecordType {
    #[default]
    A,
    AAAA,
}

impl std::fmt::Display for RecordType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RecordType::A => write!(f, "A"),
            RecordType::AAAA => write!(f, "AAAA"),
        }
    }
}

impl std::str::FromStr for RecordType {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_uppercase().as_str() {
            "A" => Ok(RecordType::A),
            "AAAA" => Ok(RecordType::AAAA),
            _ => anyhow::bail!("Invalid record type: {}. Use 'A' or 'AAAA'", s),
        }
    }
}

fn default_record_type() -> RecordType {
    RecordType::A
}

fn default_ttl() -> u32 {
    1 // Automatic TTL
}

pub fn default_ipv4_url() -> String {
    "https://api.ipify.org".to_string()
}

pub fn default_ipv6_url() -> String {
    "https://api6.ipify.org".to_string()
}

impl Config {
    /// Load configuration from a TOML file
    pub fn load<P: AsRef<Path>>(path: P) -> Result<Self> {
        let path = path.as_ref();
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;

        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;

        config.validate()?;

        Ok(config)
    }

    /// Create a configuration from CLI arguments
    pub fn from_args(
        api_token: String,
        zone: String,
        record_name: String,
        record_type: RecordType,
        proxied: bool,
        ttl: u32,
        force_ip: Option<IpAddr>,
    ) -> Result<Self> {
        let config = Config {
            cloudflare: CloudflareConfig { api_token },
            records: vec![RecordConfig {
                zone,
                name: record_name,
                record_type,
                proxied,
                ttl,
            }],
            settings: Settings {
                ipv4_url: default_ipv4_url(),
                ipv6_url: default_ipv6_url(),
                force_ip,
            },
            service: ServiceConfig::default(),
        };

        config.validate()?;
        Ok(config)
    }

    /// Validate the configuration
    pub fn validate(&self) -> Result<()> {
        if self.cloudflare.api_token.is_empty() {
            anyhow::bail!("Cloudflare API token cannot be empty");
        }

        if self.records.is_empty() {
            anyhow::bail!("At least one DNS record must be configured");
        }

        for record in &self.records {
            if record.zone.is_empty() {
                anyhow::bail!("Record zone cannot be empty");
            }
            if record.name.is_empty() {
                anyhow::bail!("Record name cannot be empty");
            }
        }

        Ok(())
    }

    /// Save configuration to a TOML file
    pub fn save<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        let path = path.as_ref();
        let content = toml::to_string_pretty(self).context("Failed to serialize config")?;

        fs::write(path, content)
            .with_context(|| format!("Failed to write config file: {}", path.display()))?;

        Ok(())
    }
}
