#!/bin/bash
# Installs the per-node netplan config for the DPU host network interface.
#
# The netplan file in the ISO/ZIP was generated with a placeholder MAC
# (aa:aa:aa:aa:aa:aa). This script detects the actual BlueField p0 MAC
# address on this host and substitutes it before writing the config to
# /etc/netplan/99_config.yaml and applying it.
#
# Must be run as root on the site controller host.
#
# Usage:
#   ./setup_netplan.sh --server-name <hostname>
#
# Options:
#   --server-name NAME   Hostname of this server. Used to locate
#                        servers/<NAME>/99_config.yaml relative to this script.
#   --help               Show this help message

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PLACEHOLDER_MAC="aa:aa:aa:aa:aa:aa"
NETPLAN_DEST="/etc/netplan/99_config.yaml"

die()  { echo "ERROR: $*" >&3 2>/dev/null || echo "ERROR: $*" >&2; exit 1; }
log()  { echo "[$(date '+%H:%M:%S')] $*"; }

usage() {
    grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
    exit 1
}

SERVER_NAME=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --server-name) SERVER_NAME="${2:-}"; shift 2 ;;
        --help|-h) usage ;;
        *) die "Unknown option: $1" ;;
    esac
done

[[ -z "$SERVER_NAME" ]] && die "--server-name is required"

NETPLAN_SRC="$SCRIPT_DIR/servers/$SERVER_NAME/99_config.yaml"
[[ -f "$NETPLAN_SRC" ]] || die "Netplan source not found: $NETPLAN_SRC"

if [ "$(id -u)" -ne 0 ]; then
    die "must be run as root"
fi

# ── Detect the BlueField p0 MAC address ───────────────────────────────────────

command -v lshw >/dev/null 2>&1 || die "lshw not found — required to detect the BlueField p0 MAC address"

mac=""
for _attempt in 1 2 3; do
    log "Detecting BlueField p0 MAC address (attempt ${_attempt}/3)..."
    mac=$(lshw -c network -quiet \
        | awk '$1 == "description:" {desc=$2}
               $1 == "product:"     {product=$0}
               $1 == "logical"      {logical_name=$3}
               $1 == "serial:"      {print logical_name, $2, desc, product}' \
        | grep "Ethernet" | grep "BlueField" | grep p0 | sed -n '1p' | awk '{print $2}' || true)
    if [[ -n "$mac" ]]; then
        break
    fi
    if [[ "$_attempt" -lt 3 ]]; then
        echo "BlueField p0 not found (attempt ${_attempt}/3), retrying in 10s..." >&3
        sleep 10
    fi
done

if [[ -z "$mac" ]]; then
    die "BlueField p0 network interface not found after 3 attempts. Cannot determine MAC address."
fi

[[ "$mac" =~ ^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$ ]] \
    || die "Detected value is not a MAC address: '$mac' — lshw output format may have changed"

log "Detected MAC: $mac"

# ── Install netplan with real MAC substituted ──────────────────────────────────

log "Installing netplan to $NETPLAN_DEST..."
grep -qF "$PLACEHOLDER_MAC" "$NETPLAN_SRC" \
    || die "Placeholder MAC $PLACEHOLDER_MAC not found in $NETPLAN_SRC — refusing to apply"

_tmp="$(mktemp)"
chmod 600 "$_tmp"
sed "s/$PLACEHOLDER_MAC/$mac/g" "$NETPLAN_SRC" > "$_tmp"
grep -qF "$PLACEHOLDER_MAC" "$_tmp" && { rm -f "$_tmp"; die "MAC substitution incomplete"; }
[[ -f "$NETPLAN_DEST" ]] && cp -a "$NETPLAN_DEST" "${NETPLAN_DEST}.bak"
mv "$_tmp" "$NETPLAN_DEST"

log "Applying netplan..."
netplan generate || die "netplan generate rejected $NETPLAN_DEST"
netplan apply

log "Netplan configured and applied for $SERVER_NAME (MAC: $mac)"
