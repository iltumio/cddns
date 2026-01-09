use anyhow::{Context, Result};
use cloudflare::endpoints::dns::dns::{
    CreateDnsRecord, CreateDnsRecordParams, DnsContent, DnsRecord, ListDnsRecords,
    ListDnsRecordsParams, UpdateDnsRecord, UpdateDnsRecordParams,
};
use cloudflare::endpoints::zones::zone::{ListZones, ListZonesParams};
use cloudflare::framework::auth::Credentials;
use cloudflare::framework::client::async_api::Client;
use cloudflare::framework::client::ClientConfig;
use cloudflare::framework::Environment;
use std::net::IpAddr;
use tracing::{debug, info, warn};

use crate::config::{RecordConfig, RecordType};

/// Cloudflare DDNS client wrapper
pub struct DdnsClient {
    client: Client,
}

impl DdnsClient {
    /// Create a new DDNS client with the given API token
    pub fn new(api_token: &str) -> Result<Self> {
        let credentials = Credentials::UserAuthToken {
            token: api_token.to_string(),
        };

        let client = Client::new(
            credentials,
            ClientConfig::default(),
            Environment::Production,
        )
        .context("Failed to create Cloudflare client")?;

        Ok(Self { client })
    }

    /// Get the zone ID for a given zone name
    pub async fn get_zone_id(&self, zone_name: &str) -> Result<String> {
        debug!("Looking up zone ID for: {}", zone_name);

        let endpoint = ListZones {
            params: ListZonesParams {
                name: Some(zone_name.to_string()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .context("Failed to list zones")?;

        let zone = response
            .result
            .into_iter()
            .find(|z| z.name == zone_name)
            .with_context(|| format!("Zone not found: {}", zone_name))?;

        debug!("Found zone ID: {} for {}", zone.id, zone_name);
        Ok(zone.id)
    }

    /// Find an existing DNS record by name and type
    pub async fn find_record(
        &self,
        zone_id: &str,
        record_name: &str,
        record_type: RecordType,
    ) -> Result<Option<DnsRecord>> {
        debug!(
            "Looking for {} record: {} in zone {}",
            record_type, record_name, zone_id
        );

        let endpoint = ListDnsRecords {
            zone_identifier: zone_id,
            params: ListDnsRecordsParams {
                name: Some(record_name.to_string()),
                ..Default::default()
            },
        };

        let response = self
            .client
            .request(&endpoint)
            .await
            .context("Failed to list DNS records")?;

        // Filter by record type
        let record = response.result.into_iter().find(|r| {
            let matches_type = match (&r.content, record_type) {
                (DnsContent::A { .. }, RecordType::A) => true,
                (DnsContent::AAAA { .. }, RecordType::AAAA) => true,
                _ => false,
            };
            matches_type && r.name == record_name
        });

        if let Some(ref r) = record {
            debug!("Found existing record: {} -> {:?}", r.name, r.content);
        } else {
            debug!(
                "No existing {} record found for {}",
                record_type, record_name
            );
        }

        Ok(record)
    }

    /// Update an existing DNS record with a new IP
    pub async fn update_record(
        &self,
        zone_id: &str,
        record: &DnsRecord,
        new_ip: IpAddr,
        proxied: bool,
        ttl: u32,
    ) -> Result<()> {
        let content = match new_ip {
            IpAddr::V4(ip) => DnsContent::A { content: ip },
            IpAddr::V6(ip) => DnsContent::AAAA { content: ip },
        };

        let endpoint = UpdateDnsRecord {
            zone_identifier: zone_id,
            identifier: &record.id,
            params: UpdateDnsRecordParams {
                name: &record.name,
                content,
                ttl: Some(ttl),
                proxied: Some(proxied),
            },
        };

        self.client
            .request(&endpoint)
            .await
            .context("Failed to update DNS record")?;

        info!("Updated {} -> {}", record.name, new_ip);
        Ok(())
    }

    /// Create a new DNS record
    pub async fn create_record(
        &self,
        zone_id: &str,
        record_name: &str,
        ip: IpAddr,
        proxied: bool,
        ttl: u32,
    ) -> Result<()> {
        let content = match ip {
            IpAddr::V4(ip) => DnsContent::A { content: ip },
            IpAddr::V6(ip) => DnsContent::AAAA { content: ip },
        };

        let endpoint = CreateDnsRecord {
            zone_identifier: zone_id,
            params: CreateDnsRecordParams {
                name: record_name,
                content,
                ttl: Some(ttl),
                proxied: Some(proxied),
                priority: None,
            },
        };

        self.client
            .request(&endpoint)
            .await
            .context("Failed to create DNS record")?;

        info!("Created {} -> {}", record_name, ip);
        Ok(())
    }

    /// Update a DNS record configuration with the given IP
    /// Creates the record if it doesn't exist, updates it if the IP has changed
    pub async fn update_ddns(
        &self,
        record_config: &RecordConfig,
        ip: IpAddr,
    ) -> Result<UpdateResult> {
        // Get the zone ID
        let zone_id = self.get_zone_id(&record_config.zone).await?;

        // Find existing record
        let existing = self
            .find_record(&zone_id, &record_config.name, record_config.record_type)
            .await?;

        match existing {
            Some(record) => {
                // Check if IP has changed
                let current_ip = extract_ip(&record.content);

                if current_ip == Some(ip) {
                    debug!("{} already points to {}, skipping", record_config.name, ip);
                    return Ok(UpdateResult::Unchanged);
                }

                // Update the record
                self.update_record(
                    &zone_id,
                    &record,
                    ip,
                    record_config.proxied,
                    record_config.ttl,
                )
                .await?;

                Ok(UpdateResult::Updated {
                    old_ip: current_ip,
                    new_ip: ip,
                })
            }
            None => {
                // Create new record
                warn!(
                    "Record {} not found, creating new {} record",
                    record_config.name, record_config.record_type
                );

                self.create_record(
                    &zone_id,
                    &record_config.name,
                    ip,
                    record_config.proxied,
                    record_config.ttl,
                )
                .await?;

                Ok(UpdateResult::Created)
            }
        }
    }
}

/// Result of a DDNS update operation
#[derive(Debug)]
pub enum UpdateResult {
    /// Record was created (didn't exist before)
    Created,
    /// Record was updated with new IP
    Updated {
        old_ip: Option<IpAddr>,
        new_ip: IpAddr,
    },
    /// Record already had the correct IP
    Unchanged,
}

/// Extract IP address from DNS content
fn extract_ip(content: &DnsContent) -> Option<IpAddr> {
    match content {
        DnsContent::A { content } => Some(IpAddr::V4(*content)),
        DnsContent::AAAA { content } => Some(IpAddr::V6(*content)),
        _ => None,
    }
}
