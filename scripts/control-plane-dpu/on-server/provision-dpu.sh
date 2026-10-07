#!/bin/bash
# provision-dpu.sh
# Provisions the DPU on this host. Run from the working directory created by
# install.sh. Can be re-run after a failure — completed steps are skipped via
# touchfiles in the touchfiles/ subdirectory.
#
# Usage:
#   ./provision-dpu.sh --server-name <hostname>
#
# Options:
#   --server-name NAME   Hostname of this server. Must match a directory under
#                        servers/ in the working directory.
#   --help               Show this help message

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# fd 3 initially = terminal (for early die() before log ready);
# reassigned below to tee so it writes to both log and terminal.
exec 3>&2
die() { echo "ERROR: $*" >&3; exit 1; }
[[ "$(id -u)" -ne 0 ]] && die "must be run as root"
LOG_FILE="$SCRIPT_DIR/provision.log"
exec 2>>"$LOG_FILE"
exec 3> >(tee -a "$LOG_FILE" >/dev/tty)
echo "============================================================" >&3
echo "  Logging to: $LOG_FILE" >&3
echo "============================================================" >&3

log() { echo "[$(date '+%H:%M:%S')] $*"; }

usage() {
    grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
    exit 1
}

# ── Validate working directory ─────────────────────────────────────────────────

VERSION_CFG="$SCRIPT_DIR/dpu_fw_version.cfg"
[[ -f "$VERSION_CFG" ]] || die "dpu_fw_version.cfg not found — run install.sh first"
# shellcheck source=/dev/null
source "$VERSION_CFG"
[[ -z "${DOCA_VERSION:-}" ]] && die "DOCA_VERSION not set in $VERSION_CFG"
[[ -z "${HBN_VERSION:-}" ]]  && die "HBN_VERSION not set in $VERSION_CFG"

[[ -f "$SCRIPT_DIR/dpuinstall.sh" ]] \
    || die "dpuinstall.sh not found in $SCRIPT_DIR — run install.sh first"

# ── Argument parsing ───────────────────────────────────────────────────────────

SERVER_NAME=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --server-name) [[ -z "${2:-}" ]] && die "--server-name requires a value"; SERVER_NAME="$2"; shift 2 ;;
        --help|-h) usage ;;
        *) die "Unknown option: $1" ;;
    esac
done

[[ -z "$SERVER_NAME" ]] && die "--server-name is required"

# ── Validate server name ───────────────────────────────────────────────────────

SERVERS_DIR="$SCRIPT_DIR/servers"
[[ -d "$SERVERS_DIR" ]] || die "servers/ directory not found in $SCRIPT_DIR — run install.sh first"

STARTUP_YAML="$SERVERS_DIR/$SERVER_NAME/startup.yaml"
NETPLAN_YAML="$SERVERS_DIR/$SERVER_NAME/99_config.yaml"

if [[ ! -d "$SERVERS_DIR/$SERVER_NAME" ]]; then
    echo "ERROR: unknown server name: $SERVER_NAME" >&3
    echo "Available nodes:" >&3
    for d in "$SERVERS_DIR"/*/; do
        echo "  $(basename "$d")" >&3
    done
    exit 1
fi

[[ -f "$STARTUP_YAML" ]] \
    || die "startup.yaml not found for $SERVER_NAME — ISO may be incomplete"
[[ -f "$NETPLAN_YAML" ]] \
    || die "99_config.yaml not found for $SERVER_NAME — ISO may be incomplete"

# ── Run provisioning ───────────────────────────────────────────────────────────

log "Provisioning server: $SERVER_NAME"
log "DOCA: $DOCA_VERSION  HBN: $HBN_VERSION"
log "startup.yaml: $STARTUP_YAML"

exec bash "$SCRIPT_DIR/dpuinstall.sh" --startup-yaml "$STARTUP_YAML" --server-name "$SERVER_NAME"
