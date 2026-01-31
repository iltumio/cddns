#!/usr/bin/env bash
#
# CDDNS LXC Container Creator for Proxmox
# Creates a Debian 12 LXC container with cddns installed and configured
#
# Usage:
#   Run on the Proxmox host as root:
#   bash create-lxc.sh
#

set -e

# Configuration
GITHUB_REPO="iltumio/cddns"
BINARY_NAME="cddns"
INSTALL_DIR="/opt/cddns"
CONFIG_DIR="/etc/cddns"
SERVICE_NAME="cddns"
BRIDGE="vmbr0"

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
CYAN='\033[0;36m'
BOLD='\033[1m'
NC='\033[0m'

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }
log_step() { echo -e "${CYAN}[STEP]${NC} $1"; }

# Prompt with default value
# Usage: prompt_value "Label" "default"
prompt_value() {
    local label="$1"
    local default="$2"
    local result

    if [[ -n "$default" ]]; then
        read -rp "  $label [$default]: " result
        echo "${result:-$default}"
    else
        while [[ -z "$result" ]]; do
            read -rp "  $label: " result
            if [[ -z "$result" ]]; then
                log_error "This field is required"
            fi
        done
        echo "$result"
    fi
}

# Prompt for choice between options
# Usage: prompt_choice "Label" "option1/option2" "default"
prompt_choice() {
    local label="$1"
    local options="$2"
    local default="$3"
    local result

    while true; do
        read -rp "  $label ($options) [$default]: " result
        result="${result:-$default}"
        if echo "$options" | tr '/' '\n' | grep -qx "$result"; then
            echo "$result"
            return
        fi
        log_error "Invalid choice. Options: $options"
    done
}

# ─── Verify environment ────────────────────────────────────────────────────────

if [[ $EUID -ne 0 ]]; then
    log_error "This script must be run as root on the Proxmox host"
    exit 1
fi

if ! command -v pct &>/dev/null || ! command -v pvesh &>/dev/null; then
    log_error "Proxmox tools not found. Run this script on the Proxmox host."
    exit 1
fi

# ─── Detect defaults ───────────────────────────────────────────────────────────

# Next available CT ID
next_ctid() {
    local id=100
    while pct status "$id" &>/dev/null; do
        ((id++))
    done
    echo "$id"
}

# First available storage that supports rootdir
default_storage() {
    pvesh get /storage --output-format json 2>/dev/null \
        | python3 -c "
import sys, json
stores = json.load(sys.stdin)
for s in stores:
    content = s.get('content', '')
    if 'rootdir' in content:
        print(s['storage'])
        break
" 2>/dev/null || echo "local-lvm"
}

DEFAULT_CTID=$(next_ctid)
DEFAULT_STORAGE=$(default_storage)

# ─── Interactive prompts ────────────────────────────────────────────────────────

echo -e "${BOLD}=== CDDNS LXC Container Setup ===${NC}"
echo ""
echo -e "${BOLD}Container Settings:${NC}"

CTID=$(prompt_value "CT ID" "$DEFAULT_CTID")
CT_HOSTNAME=$(prompt_value "Hostname" "cddns")
CT_MEMORY=$(prompt_value "Memory in MB" "128")
CT_DISK=$(prompt_value "Disk size in MB" "512")
CT_STORAGE=$(prompt_value "Storage" "$DEFAULT_STORAGE")

echo ""
echo -e "${BOLD}Network:${NC}"

NET_TYPE=$(prompt_choice "Type" "dhcp/static" "dhcp")

if [[ "$NET_TYPE" == "static" ]]; then
    NET_IP=$(prompt_value "IP (CIDR, e.g. 192.168.1.50/24)" "")
    NET_GW=$(prompt_value "Gateway" "")
    NET_DNS=$(prompt_value "DNS Server" "1.1.1.1")
fi

echo ""
echo -e "${BOLD}CDDNS Configuration:${NC}"

CF_TOKEN=$(prompt_value "Cloudflare API Token" "")
CF_ZONE=$(prompt_value "Zone (e.g. example.com)" "")
CF_RECORD=$(prompt_value "Record Name (e.g. home.example.com)" "")
CF_TYPE=$(prompt_choice "Record Type" "A/AAAA" "A")
CF_PROXIED=$(prompt_choice "Proxied" "yes/no" "no")
CF_TTL=$(prompt_value "TTL (1 = automatic)" "1")
CF_CRON=$(prompt_value "Cron Schedule" "0 */5 * * * *")

# Convert proxied to boolean
if [[ "$CF_PROXIED" == "yes" ]]; then
    CF_PROXIED_BOOL="true"
else
    CF_PROXIED_BOOL="false"
fi

# ─── Confirm ────────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}=== Summary ===${NC}"
echo "  CT ID:       $CTID"
echo "  Hostname:    $CT_HOSTNAME"
echo "  Memory:      ${CT_MEMORY}MB"
echo "  Disk:        ${CT_DISK}MB"
echo "  Storage:     $CT_STORAGE"
if [[ "$NET_TYPE" == "dhcp" ]]; then
    echo "  Network:     DHCP"
else
    echo "  Network:     $NET_IP (gw: $NET_GW, dns: $NET_DNS)"
fi
echo "  Zone:        $CF_ZONE"
echo "  Record:      $CF_RECORD ($CF_TYPE)"
echo ""
read -rp "Proceed? (y/N): " confirm
if [[ "$confirm" != "y" && "$confirm" != "Y" ]]; then
    echo "Aborted."
    exit 0
fi

echo ""

# ─── Download template ──────────────────────────────────────────────────────────

log_step "Checking Debian 12 template..."

pveam update &>/dev/null || true

# Find the Debian 12 template
TEMPLATE=$(pveam available --section system 2>/dev/null \
    | awk '/debian-12-standard/ {print $2}' \
    | sort -V | tail -1)

if [[ -z "$TEMPLATE" ]]; then
    log_error "Could not find Debian 12 template"
    exit 1
fi

# Check if already downloaded
if ! pveam list "$CT_STORAGE" 2>/dev/null | grep -q "$TEMPLATE"; then
    log_info "Downloading $TEMPLATE..."
    pveam download "$CT_STORAGE" "$TEMPLATE"
else
    log_info "Template $TEMPLATE already available"
fi

TEMPLATE_PATH="$CT_STORAGE:vztmpl/$TEMPLATE"

# ─── Create container ───────────────────────────────────────────────────────────

log_step "Creating LXC container $CTID..."

# Build network string
if [[ "$NET_TYPE" == "dhcp" ]]; then
    NET_STRING="name=eth0,bridge=$BRIDGE,ip=dhcp"
else
    NET_STRING="name=eth0,bridge=$BRIDGE,ip=$NET_IP,gw=$NET_GW"
fi

# Convert disk MB to GB for pct (minimum 0.5)
CT_DISK_GB=$(python3 -c "print(max(0.5, $CT_DISK / 1024))")

pct create "$CTID" "$TEMPLATE_PATH" \
    --hostname "$CT_HOSTNAME" \
    --memory "$CT_MEMORY" \
    --swap 0 \
    --rootfs "$CT_STORAGE:$CT_DISK_GB" \
    --net0 "$NET_STRING" \
    --unprivileged 1 \
    --features nesting=1 \
    --start 0

log_info "Container $CTID created"

# Set DNS for static config
if [[ "$NET_TYPE" == "static" && -n "$NET_DNS" ]]; then
    pct set "$CTID" --nameserver "$NET_DNS"
fi

# ─── Start container ────────────────────────────────────────────────────────────

log_step "Starting container..."
pct start "$CTID"

# Wait for container to be fully up
log_info "Waiting for container to initialize..."
sleep 3

# Wait for network connectivity
for i in $(seq 1 30); do
    if pct exec "$CTID" -- ping -c1 -W1 1.1.1.1 &>/dev/null; then
        break
    fi
    if [[ $i -eq 30 ]]; then
        log_error "Container has no network connectivity after 30 seconds"
        exit 1
    fi
    sleep 1
done

log_info "Container is up and has network access"

# ─── Install cddns inside container ─────────────────────────────────────────────

log_step "Installing cddns inside container..."

# Install dependencies
pct exec "$CTID" -- bash -c "apt-get update -qq && apt-get install -y -qq curl ca-certificates >/dev/null 2>&1"

# Detect architecture inside container
CT_ARCH=$(pct exec "$CTID" -- uname -m)
case $CT_ARCH in
    x86_64)  TARGET="x86_64-unknown-linux-musl" ;;
    aarch64) TARGET="aarch64-unknown-linux-musl" ;;
    armv7l)  TARGET="armv7-unknown-linux-musleabihf" ;;
    *)
        log_error "Unsupported architecture inside container: $CT_ARCH"
        pct stop "$CTID"
        exit 1
        ;;
esac

# Create directories
pct exec "$CTID" -- mkdir -p "$INSTALL_DIR" "$CONFIG_DIR"

# Download binary
DOWNLOAD_URL="https://github.com/$GITHUB_REPO/releases/latest/download/cddns-$TARGET"

if pct exec "$CTID" -- curl -sSLf "$DOWNLOAD_URL" -o "$INSTALL_DIR/$BINARY_NAME" 2>/dev/null; then
    log_info "Downloaded cddns binary"
else
    log_warn "Could not download pre-built binary. Building from source..."

    pct exec "$CTID" -- bash -c "
        apt-get install -y -qq build-essential git pkg-config libssl-dev >/dev/null 2>&1
        if ! command -v cargo &>/dev/null; then
            curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
            source \"\$HOME/.cargo/env\"
        fi
        git clone https://github.com/$GITHUB_REPO.git /tmp/cddns-build
        cd /tmp/cddns-build
        \"\$HOME/.cargo/bin/cargo\" build --release
        cp target/release/$BINARY_NAME $INSTALL_DIR/$BINARY_NAME
        cd /
        rm -rf /tmp/cddns-build
    "
    log_info "Built cddns from source"
fi

# Make executable and symlink
pct exec "$CTID" -- chmod +x "$INSTALL_DIR/$BINARY_NAME"
pct exec "$CTID" -- ln -sf "$INSTALL_DIR/$BINARY_NAME" "/usr/local/bin/$BINARY_NAME"

# ─── Write configuration ────────────────────────────────────────────────────────

log_step "Writing cddns configuration..."

pct exec "$CTID" -- bash -c "cat > $CONFIG_DIR/config.toml << 'CDDNS_EOF'
[cloudflare]
api_token = \"$CF_TOKEN\"

[[records]]
zone = \"$CF_ZONE\"
name = \"$CF_RECORD\"
record_type = \"$CF_TYPE\"
proxied = $CF_PROXIED_BOOL
ttl = $CF_TTL

[settings]
ipv4_url = \"https://api.ipify.org\"
ipv6_url = \"https://api6.ipify.org\"

[service]
cron = \"$CF_CRON\"
run_on_start = true
CDDNS_EOF"

# Restrict config permissions
pct exec "$CTID" -- chmod 600 "$CONFIG_DIR/config.toml"

# ─── Create systemd service ─────────────────────────────────────────────────────

log_step "Creating systemd service..."

pct exec "$CTID" -- bash -c "cat > /etc/systemd/system/$SERVICE_NAME.service << SYSD_EOF
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

NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadOnlyPaths=/
ReadWritePaths=$CONFIG_DIR

[Install]
WantedBy=multi-user.target
SYSD_EOF"

pct exec "$CTID" -- systemctl daemon-reload
pct exec "$CTID" -- systemctl enable --now "$SERVICE_NAME"

log_info "Service enabled and started"

# ─── Wait and verify ────────────────────────────────────────────────────────────

sleep 2

SERVICE_STATUS=$(pct exec "$CTID" -- systemctl is-active "$SERVICE_NAME" 2>/dev/null || echo "unknown")

# Get container IP
CT_IP=$(pct exec "$CTID" -- hostname -I 2>/dev/null | awk '{print $1}')
CT_IP="${CT_IP:-unknown}"

# ─── Print summary ──────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}=== CDDNS LXC Container Created ===${NC}"
echo ""
echo -e "  Container ID:  ${CYAN}$CTID${NC}"
echo -e "  Hostname:      ${CYAN}$CT_HOSTNAME${NC}"
echo -e "  IP Address:    ${CYAN}$CT_IP${NC}"
echo -e "  Status:        ${GREEN}running${NC}"
echo ""
echo -e "  Service:       ${GREEN}$SERVICE_STATUS${NC}"
echo -e "  Config:        $CONFIG_DIR/config.toml"
echo -e "  Zone:          $CF_ZONE"
echo -e "  Record:        $CF_RECORD"
echo ""
echo "Useful commands:"
echo "  pct enter $CTID                              # shell into container"
echo "  pct exec $CTID -- systemctl status $SERVICE_NAME   # check service"
echo "  pct exec $CTID -- journalctl -u $SERVICE_NAME -f   # view logs"
echo "  pct exec $CTID -- cddns ui -c $CONFIG_DIR/config.toml  # run TUI"
echo "  pct stop $CTID                               # stop container"
echo "  pct destroy $CTID                            # remove container"
echo ""
