#!/usr/bin/env bash
#
# CDDNS Installation Script for Proxmox LXC/VM
# This script installs cddns as a systemd service
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/iltumio/cddns/main/proxmox/install.sh | bash
#   or
#   ./install.sh
#

set -e

# Configuration
INSTALL_DIR="/opt/cddns"
CONFIG_DIR="/etc/cddns"
BINARY_NAME="cddns"
SERVICE_NAME="cddns"
GITHUB_REPO="iltumio/cddns"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Check if running as root
if [[ $EUID -ne 0 ]]; then
    log_error "This script must be run as root"
    exit 1
fi

# Detect architecture
ARCH=$(uname -m)
case $ARCH in
    x86_64)
        TARGET="x86_64-unknown-linux-musl"
        ;;
    aarch64)
        TARGET="aarch64-unknown-linux-musl"
        ;;
    armv7l)
        TARGET="armv7-unknown-linux-musleabihf"
        ;;
    *)
        log_error "Unsupported architecture: $ARCH"
        exit 1
        ;;
esac

log_info "Detected architecture: $ARCH ($TARGET)"

# Create directories
log_info "Creating directories..."
mkdir -p "$INSTALL_DIR"
mkdir -p "$CONFIG_DIR"

# Download or build binary
if [[ -f "./target/release/$BINARY_NAME" ]]; then
    log_info "Using local binary..."
    cp "./target/release/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
elif [[ -f "./$BINARY_NAME" ]]; then
    log_info "Using binary from current directory..."
    cp "./$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
else
    log_info "Downloading latest release..."
    # Try to download from GitHub releases
    LATEST_URL="https://github.com/$GITHUB_REPO/releases/latest/download/cddns-$TARGET"
    if curl -sSLf "$LATEST_URL" -o "$INSTALL_DIR/$BINARY_NAME" 2>/dev/null; then
        log_info "Downloaded from GitHub releases"
    else
        log_warn "Could not download pre-built binary. Building from source..."
        
        # Check if Rust is installed
        if ! command -v cargo &> /dev/null; then
            log_info "Installing Rust..."
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
            source "$HOME/.cargo/env"
        fi
        
        # Clone and build
        log_info "Cloning repository..."
        git clone "https://github.com/$GITHUB_REPO.git" /tmp/cddns-build
        cd /tmp/cddns-build
        
        log_info "Building..."
        cargo build --release
        cp "target/release/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
        
        # Cleanup
        cd /
        rm -rf /tmp/cddns-build
    fi
fi

# Make binary executable
chmod +x "$INSTALL_DIR/$BINARY_NAME"

# Create symlink
ln -sf "$INSTALL_DIR/$BINARY_NAME" "/usr/local/bin/$BINARY_NAME"

# Create default config if not exists
if [[ ! -f "$CONFIG_DIR/config.toml" ]]; then
    log_info "Creating default configuration..."
    cat > "$CONFIG_DIR/config.toml" << 'EOF'
# CDDNS Configuration
# Edit this file with your Cloudflare credentials and DNS records

[cloudflare]
# API Token - Create at: https://dash.cloudflare.com/profile/api-tokens
# Required permissions: Zone:Read, DNS:Edit
api_token = "your-api-token-here"

# DNS records to update
[[records]]
zone = "example.com"
name = "home.example.com"
record_type = "A"
proxied = false
ttl = 1

# Optional settings
[settings]
ipv4_url = "https://api.ipify.org"
ipv6_url = "https://api6.ipify.org"

# Service settings
[service]
cron = "0 */5 * * * *"  # Every 5 minutes
run_on_start = true
EOF
    log_warn "Please edit $CONFIG_DIR/config.toml with your settings"
fi

# Create systemd service
log_info "Creating systemd service..."
cat > "/etc/systemd/system/$SERVICE_NAME.service" << EOF
[Unit]
Description=Cloudflare DDNS Updater
Documentation=https://github.com/$GITHUB_REPO
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart=$INSTALL_DIR/$BINARY_NAME service -c $CONFIG_DIR/config.toml
Restart=on-failure
RestartSec=10
StandardOutput=journal
StandardError=journal

# Security hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadOnlyPaths=/
ReadWritePaths=$CONFIG_DIR

[Install]
WantedBy=multi-user.target
EOF

# Reload systemd
systemctl daemon-reload

log_info "Installation complete!"
echo ""
echo "Next steps:"
echo "  1. Edit the configuration: nano $CONFIG_DIR/config.toml"
echo "  2. Enable the service:     systemctl enable $SERVICE_NAME"
echo "  3. Start the service:      systemctl start $SERVICE_NAME"
echo "  4. Check status:           systemctl status $SERVICE_NAME"
echo "  5. View logs:              journalctl -u $SERVICE_NAME -f"
echo ""
echo "Or run manually:             $BINARY_NAME service -c $CONFIG_DIR/config.toml"
echo "Or use the TUI:              $BINARY_NAME ui -c $CONFIG_DIR/config.toml"
