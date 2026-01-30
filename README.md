# cddns

A Cloudflare Dynamic DNS updater written in Rust. Automatically keeps your Cloudflare DNS records pointing to your current public IP address.

## Features

- Supports both IPv4 (A records) and IPv6 (AAAA records)
- Multiple DNS records per configuration
- Background service mode with cron scheduling
- Interactive TUI for real-time monitoring and control
- IPC communication between TUI and service
- Dry-run mode for testing

## Installation

```bash
cargo build --release
```

The binary will be at `target/release/cddns`.

## Quick Start

1. Copy the example config and fill in your details:

```bash
cp config.example.toml config.toml
```

2. Set your [Cloudflare API token](https://dash.cloudflare.com/profile/api-tokens) (requires `Zone:Read` and `DNS:Edit` permissions) and configure your DNS records.

3. Run an update:

```bash
cddns config -f config.toml
```

## Usage

```
cddns [OPTIONS] <COMMAND>
```

### Global Options

| Option | Description |
|--------|-------------|
| `-v, --verbose` | Enable debug logging |

### Commands

#### `config` - Update DNS from a config file

```bash
cddns config -f config.toml
cddns config -f config.toml --dry-run
```

#### `update` - Update DNS using CLI arguments

```bash
cddns update -t <API_TOKEN> -z example.com -r home.example.com
```

The API token can also be provided via the `CF_API_TOKEN` environment variable.

| Option | Description | Default |
|--------|-------------|---------|
| `-t, --api-token` | Cloudflare API token | `CF_API_TOKEN` env |
| `-z, --zone` | Zone/domain name | |
| `-r, --record` | DNS record name | |
| `-T, --record-type` | `A` or `AAAA` | `A` |
| `-p, --proxied` | Enable Cloudflare proxy | `false` |
| `--ttl` | TTL in seconds | `1` (auto) |
| `-i, --ip` | Force a specific IP | auto-detect |
| `-n, --dry-run` | Don't apply changes | |

#### `service` - Run as a background service

```bash
cddns service -c config.toml
```

Runs in the background with cron-scheduled updates. Exposes a Unix socket for IPC control.

#### `ui` - Interactive TUI

```bash
cddns ui -c config.toml
```

Provides a terminal interface for configuring records, monitoring service status, and triggering updates. Connects to a running service via IPC if available.

**Key bindings (normal mode):**

| Key | Action |
|-----|--------|
| `e` | Edit mode |
| `Tab` / `j` / `Down` | Next field |
| `Shift+Tab` / `k` / `Up` | Previous field |
| `Space` | Toggle field (record type, proxied) |
| `i` | Detect IP |
| `u` / `Enter` | Update DNS |
| `s` | Save config |
| `S` | Start service |
| `X` | Stop service |
| `r` | Refresh service status |
| `d` | Detach (quit TUI, keep service) |
| `?` | Help |
| `q` | Quit |

## Configuration

See [`config.example.toml`](config.example.toml) for a full example.

```toml
[cloudflare]
api_token = "your-api-token-here"

[[records]]
zone = "example.com"
name = "home.example.com"
record_type = "A"
proxied = false
ttl = 1

[settings]
ipv4_url = "https://api.ipify.org"
ipv6_url = "https://api6.ipify.org"
# force_ip = "1.2.3.4"

[service]
cron = "0 */5 * * * *"
run_on_start = true
```

### Service cron format

The cron expression uses a 7-field format: `sec min hour day_of_month month day_of_week year`.

Examples:
- `0 */5 * * * *` - Every 5 minutes
- `0 0 * * * *` - Every hour
- `0 0 0 * * *` - Daily at midnight

## License

MIT
