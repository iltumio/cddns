#!/usr/bin/env bash
#
# CDDNS Uninstallation Script
# This script removes cddns and its systemd service
#
# Usage:
#   sudo ./uninstall.sh
#   sudo ./uninstall.sh --keep-config  # Keep configuration files
#

set -e

# Configuration
INSTALL_DIR="/opt/cddns"
CONFIG_DIR="/etc/cddns"
BINARY_NAME="cddns"
SERVICE_NAME="cddns"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log_info() { echo -e "${GREEN}[INFO]${NC} $1"; }
log_warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
log_error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Parse arguments
KEEP_CONFIG=false
for arg in "$@"; do
    case $arg in
        --keep-config)
            KEEP_CONFIG=true
            shift
            ;;
    esac
done

# Check if running as root
if [[ $EUID -ne 0 ]]; then
    log_error "This script must be run as root"
    exit 1
fi

log_info "Uninstalling cddns..."

# Stop and disable service
if systemctl is-active --quiet "$SERVICE_NAME" 2>/dev/null; then
    log_info "Stopping service..."
    systemctl stop "$SERVICE_NAME"
fi

if systemctl is-enabled --quiet "$SERVICE_NAME" 2>/dev/null; then
    log_info "Disabling service..."
    systemctl disable "$SERVICE_NAME"
fi

# Remove systemd service file
if [[ -f "/etc/systemd/system/$SERVICE_NAME.service" ]]; then
    log_info "Removing systemd service..."
    rm -f "/etc/systemd/system/$SERVICE_NAME.service"
    systemctl daemon-reload
fi

# Remove binary and symlink
if [[ -L "/usr/local/bin/$BINARY_NAME" ]]; then
    log_info "Removing symlink..."
    rm -f "/usr/local/bin/$BINARY_NAME"
fi

if [[ -d "$INSTALL_DIR" ]]; then
    log_info "Removing installation directory..."
    rm -rf "$INSTALL_DIR"
fi

# Remove config (unless --keep-config)
if [[ "$KEEP_CONFIG" == "false" ]]; then
    if [[ -d "$CONFIG_DIR" ]]; then
        log_info "Removing configuration..."
        rm -rf "$CONFIG_DIR"
    fi
else
    log_warn "Configuration preserved at: $CONFIG_DIR"
fi

# Remove IPC socket if exists
SOCKET_PATH="${XDG_RUNTIME_DIR:-/tmp}/cddns.sock"
if [[ -S "$SOCKET_PATH" ]]; then
    log_info "Removing IPC socket..."
    rm -f "$SOCKET_PATH"
fi

log_info "Uninstallation complete!"

if [[ "$KEEP_CONFIG" == "true" ]]; then
    echo ""
    echo "Configuration files preserved at: $CONFIG_DIR"
    echo "To completely remove, run: sudo rm -rf $CONFIG_DIR"
fi
