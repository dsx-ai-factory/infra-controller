#!/bin/bash
# Run after the host power cycles during DPU provisioning.
# Sources dpuinstall.sh from the working directory to reuse SSH helpers
# and check_hbn_container without re-running the install steps.
#
# Usage:
#   ./post-power-cycle.sh --server-name <hostname>
#
# Options:
#   --server-name NAME   Hostname of this server (same value used with install.sh)
#   --help               Show this help message
#
# Must be run as root.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# fd 3 initially = terminal (for early die() before log ready);
# reassigned below to tee so it writes to both log and terminal.
exec 3>&2
die() { echo "ERROR: $*" >&3; exit 1; }
[[ "$(id -u)" -ne 0 ]] && die "must be run as root"
LOG_FILE="$SCRIPT_DIR/post-power-cycle.log"
exec 2>>"$LOG_FILE"
exec 3> >(tee -a "$LOG_FILE" >/dev/tty)
echo "============================================================" >&3
echo "  Logging to: $LOG_FILE" >&3
echo "============================================================" >&3

usage() {
    grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
    exit 1
}

SERVER_NAME=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --server-name) [[ -z "${2:-}" ]] && die "--server-name requires a value"; SERVER_NAME="$2"; shift 2 ;;
        --help|-h) usage ;;
        *) die "Unknown option: $1" ;;
    esac
done

[[ -z "$SERVER_NAME" ]] && die "--server-name is required"

# ── Locate the working directory from the version config ──────────────────────

VERSION_CFG="$SCRIPT_DIR/dpu_fw_version.cfg"
[[ -f "$VERSION_CFG" ]] || die "dpu_fw_version.cfg not found at $VERSION_CFG"
# shellcheck source=/dev/null
source "$VERSION_CFG"
[[ -z "${DOCA_VERSION:-}" ]] && die "DOCA_VERSION not set in $VERSION_CFG"
[[ -z "${HBN_VERSION:-}" ]]  && die "HBN_VERSION not set in $VERSION_CFG"

WORK_DIR="/var/lib/dpu-install/${DOCA_VERSION}_${HBN_VERSION}"
[[ -d "$WORK_DIR" ]] || die "Working directory not found: $WORK_DIR — has install.sh been run?"

# ── Source dpuinstall.sh to load functions ─────────────────────────────────────
# The BASH_SOURCE guard in dpuinstall.sh prevents the main install block
# from executing when sourced.

# shellcheck source=dpuinstall.sh
source "$WORK_DIR/dpuinstall.sh"

[[ -f "$TOUCHFILE_HBN_DEPLOYED" ]] || die "HBN deployment touchfile not found — power cycle may have happened too early or install.sh did not complete"

trap cleanup EXIT
set -eux

update_progress 11
check_hbn_container
# check_hbn_container re-runs start_rshim/setup_tmfifo, which rewind CUR_STEP.
update_progress 11

if [ -f "$TOUCHFILE_NETPLAN_CONFIGURED" ]; then
    echo "Netplan was configured before power cycle — skipping."
else
    bash "$SCRIPT_DIR/setup_netplan.sh" --server-name "$SERVER_NAME"
fi

CUR_STEP=$FINAL_STEP
echo "DPU provisioning complete"
