#!/bin/bash
# upgrade-post-power-cycle.sh
# Run after the host power cycles during a DPU firmware upgrade.
# Sources dpuinstall.sh from the working directory to reuse SSH helpers and
# check_hbn_container without re-running the install steps.
#
# Unlike the initial-provisioning post-power-cycle.sh, this script does NOT
# write or apply netplan. It verifies that the BlueField p0 MAC address is
# unchanged from before the upgrade, so the existing host netplan stays valid.
#
# Usage:
#   ./upgrade-post-power-cycle.sh
#
# Options:
#   --help   Show this help message
#
# Must be run as root.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# fd 3 initially = terminal (for early die() before log ready);
# reassigned below to tee so it writes to both log and terminal.
exec 3>&2
# Print an error to the console fd and abort.
die() { echo "ERROR: $*" >&3; exit 1; }

# Print this script's header comment as usage text and exit.
usage() {
    grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
    exit 1
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --help|-h) usage ;;
        *) die "Unknown option: $1" ;;
    esac
done

[[ "$(id -u)" -ne 0 ]] && die "must be run as root"
LOG_FILE="$SCRIPT_DIR/upgrade-post-power-cycle.log"
# Keep the original stderr on fd 4: when there is no usable /dev/tty (e.g. a
# session without a controlling terminal), tee dies and the first write to
# fd 3 would kill the script silently with SIGPIPE.
exec 4>&3
exec 2>>"$LOG_FILE"
if { : > /dev/tty; } 2>/dev/null; then
    exec 3> >(tee -a "$LOG_FILE" >/dev/tty)
else
    exec 3> >(tee -a "$LOG_FILE" >&4)
fi
echo "============================================================" >&3
echo "  Logging to: $LOG_FILE" >&3
echo "============================================================" >&3

# ── Validate the working directory ─────────────────────────────────────────────

VERSION_CFG="$SCRIPT_DIR/dpu_fw_version.cfg"
[[ -f "$VERSION_CFG" ]] || die "dpu_fw_version.cfg not found at $VERSION_CFG"
# shellcheck source=/dev/null
source "$VERSION_CFG"
[[ -z "${DOCA_VERSION:-}" ]] && die "DOCA_VERSION not set in $VERSION_CFG"
[[ -z "${HBN_VERSION:-}" ]]  && die "HBN_VERSION not set in $VERSION_CFG"

BACKUP_P0_MAC="$SCRIPT_DIR/backup/p0_mac"
[[ -s "$BACKUP_P0_MAC" ]] \
    || die "saved p0 MAC not found at $BACKUP_P0_MAC — did upgrade-dpu-fw.sh complete its backup phase?"
EXPECTED_MAC="$(cat "$BACKUP_P0_MAC")"

# ── Source helpers and the (unchanged) provisioning functions ─────────────────
# The BASH_SOURCE guard in dpuinstall.sh prevents the main install block
# from executing when sourced.

# shellcheck source=upgrade-lib.sh
source "$SCRIPT_DIR/upgrade-lib.sh"
# shellcheck source=/dev/null
source "$SCRIPT_DIR/dpuinstall.sh"

[[ -f "$TOUCHFILE_HBN_DEPLOYED" ]] \
    || die "HBN deployment touchfile not found — power cycle may have happened before upgrade-dpu-fw.sh completed. Re-run upgrade-dpu-fw.sh."

trap cleanup EXIT
set -eux

update_progress 11
# update_progress 11 sets CUR_STEP=FINAL_STEP, which would make the cleanup
# EXIT trap report success even when the MAC validation below fails. Keep
# CUR_STEP below final until every check has passed.
CUR_STEP=$(( FINAL_STEP - 1 ))
check_hbn_container
# check_hbn_container re-runs start_rshim/setup_tmfifo, which rewind CUR_STEP.
update_progress 11
CUR_STEP=$(( FINAL_STEP - 1 ))

# ── Validate the BlueField p0 MAC instead of writing netplan ──────────────────

echo "Validating that the BlueField p0 MAC address is unchanged..." >&3
ACTUAL_MAC="$(detect_bluefield_p0_mac 3 10)" \
    || die "could not detect the BlueField p0 MAC after the upgrade (see $LOG_FILE)"

# backup/p0_mac is the immutable pre-flash record. An operator who has dealt
# with a hardware swap acknowledges the new MAC in a SEPARATE file so the
# original stays available for audit and a re-run cannot self-compare.
BACKUP_P0_MAC_REPLACED="$SCRIPT_DIR/backup/p0_mac.replaced"

if macs_equal "$EXPECTED_MAC" "$ACTUAL_MAC"; then
    echo "BlueField p0 MAC unchanged since the pre-flash backup: $ACTUAL_MAC" >&3
elif [ -s "$BACKUP_P0_MAC_REPLACED" ] && macs_equal "$(cat "$BACKUP_P0_MAC_REPLACED")" "$ACTUAL_MAC"; then
    echo "BlueField p0 MAC changed (pre-flash: $EXPECTED_MAC, now: $ACTUAL_MAC) and the" >&3
    echo "replacement was acknowledged in $BACKUP_P0_MAC_REPLACED." >&3
    echo "Ensure the host netplan has been updated to the new MAC." >&3
else
    echo "ERROR: BlueField p0 MAC changed during the upgrade!" >&3
    echo "  before: $EXPECTED_MAC" >&3
    echo "  after:  $ACTUAL_MAC" >&3
    echo "The existing host netplan matches the old MAC, so host networking will" >&3
    echo "not come up through the DPU. If the DPU hardware was replaced, update" >&3
    echo "the MAC in your netplan config (e.g. /etc/netplan/99_config.yaml)," >&3
    echo "run 'netplan generate && netplan apply', then acknowledge the new MAC with" >&3
    echo "  echo '$ACTUAL_MAC' > $BACKUP_P0_MAC_REPLACED" >&3
    echo "and re-run this script. (The pre-flash record in $BACKUP_P0_MAC is kept as-is.)" >&3
    exit 1
fi

# The before/after comparison proves the DPU did not change during THIS
# upgrade — but not that the host netplan references it. A replaced DPU that
# was swapped before the upgrade records the new card's MAC on both sides, so
# only a netplan cross-check gives real assurance about host networking.
if grep -rqiF "$ACTUAL_MAC" /etc/netplan/ 2>/dev/null; then
    echo "Host netplan references $ACTUAL_MAC — network configuration is consistent." >&3
else
    echo "WARNING: no file under /etc/netplan/ references $ACTUAL_MAC." >&3
    echo "If this host's netplan matches the DPU by MAC (site controllers do), host" >&3
    echo "networking will NOT come up: update the MAC in your netplan config and run" >&3
    echo "'netplan generate && netplan apply'. If your netplan matches interfaces by" >&3
    echo "name instead, this warning can be ignored." >&3
fi

CUR_STEP=$FINAL_STEP
echo "DPU firmware upgrade complete"
