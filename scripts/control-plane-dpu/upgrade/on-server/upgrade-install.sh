#!/bin/bash
# upgrade-install.sh
# Installs DPU upgrade scripts and artifacts from the upgrade ISO to a
# persistent working directory on the host. Run once per host before
# upgrading.
#
# After this script completes, run upgrade-dpu-fw.sh from the working
# directory:
#   /var/lib/dpu-upgrade/<version>/upgrade-dpu-fw.sh --ssh-key <path>
#   /var/lib/dpu-upgrade/<version>/upgrade-dpu-fw.sh --auth password
#
# Unlike initial provisioning, the host packages (libfuse2t64, rshim,
# doca-host) are usually already installed here. Each one is upgraded from the
# ISO when the ISO carries a newer version, so the host tooling matches the
# firmware being flashed; if a dependency is missing, apt-get is used to
# resolve it (the host still has network at this point — the DPU is untouched).
#
# Usage:
#   ./upgrade-install.sh [--skip-os-check] [--skip-package-upgrade]
#
# Options:
#   --skip-os-check         Skip OS version check and host package
#                           installation entirely. Use only if the host has
#                           already been validated.
#   --skip-package-upgrade  Keep already-installed host packages even when the
#                           ISO carries a newer version (missing packages are
#                           still installed). Escape hatch for hosts where the
#                           package upgrade fails or is undesirable.
#   --help                  Show this help message

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

# Save terminal as fd 3 so die() always writes to screen even after log redirect
exec 3>&2
# Print an error to the console fd and abort.
die() { echo "ERROR: $*" >&3; exit 1; }
# Timestamped console log line.
log() { echo "[$(date '+%H:%M:%S')] $*" >&3; }

# Print this script's header comment as usage text and exit.
usage() {
    grep '^#' "$0" | grep -v '#!/' | sed 's/^# \{0,1\}//'
    exit 1
}

# ── Load version ───────────────────────────────────────────────────────────────

VERSION_CFG="$SCRIPT_DIR/dpu_fw_version.cfg"
[[ -f "$VERSION_CFG" ]] || die "dpu_fw_version.cfg not found at $VERSION_CFG"
# shellcheck source=/dev/null
source "$VERSION_CFG"
[[ -z "${DOCA_VERSION:-}" ]] && die "DOCA_VERSION not set in $VERSION_CFG"
[[ -z "${HBN_VERSION:-}" ]]  && die "HBN_VERSION not set in $VERSION_CFG"

VERSION_TAG="${DOCA_VERSION}_${HBN_VERSION}"
WORK_DIR="/var/lib/dpu-upgrade/${VERSION_TAG}"
_work_dir_existed=false
[[ -d "$WORK_DIR" ]] && _work_dir_existed=true

# ── Argument parsing ───────────────────────────────────────────────────────────

SKIP_OS_CHECK=false
SKIP_PACKAGE_UPGRADE=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-os-check) SKIP_OS_CHECK=true; shift ;;
        --skip-package-upgrade) SKIP_PACKAGE_UPGRADE=true; shift ;;
        --help|-h) usage ;;
        *) die "Unknown option: $1" ;;
    esac
done

[[ "$(id -u)" -ne 0 ]] && die "must be run as root"

# Redirect trace output (set -x) to log file; terminal stays on fd 3
mkdir -p "$WORK_DIR"
LOG_FILE="$WORK_DIR/upgrade-install.log"
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

# ── Package install / upgrade helper ─────────────────────────────────────────
# Installs or upgrades a host package from the ISO .deb:
#   * not installed              → install from the ISO
#   * older than the ISO .deb    → upgrade from the ISO
#                                  (kept as-is with --skip-package-upgrade)
#   * same as or newer than ISO  → skip
# Unlike initial provisioning, the host usually still has network connectivity
# here (the DPU has not been touched yet), so a dpkg dependency failure is
# retried once with apt-get --fix-broken before giving up.
install_or_upgrade_pkg() {
    local pkg="$1" deb_glob="$2"

    local deb=""
    for f in "$SCRIPT_DIR"/$deb_glob; do
        [[ -e "$f" ]] && deb="$f" && break
    done
    [[ -z "$deb" ]] && die "$pkg .deb not found in $SCRIPT_DIR — ISO may be incomplete"

    local deb_ver
    deb_ver=$(dpkg-deb -f "$deb" Version) || die "could not read package version from $(basename "$deb")"
    [[ -z "$deb_ver" ]] && die "could not read package version from $(basename "$deb")"

    local installed_ver=""
    if dpkg -s "$pkg" &>/dev/null && dpkg -s "$pkg" | grep -q "Status: install ok installed"; then
        installed_ver=$(dpkg -s "$pkg" | awk '/^Version:/{print $2}')
    fi

    if [[ -n "$installed_ver" ]]; then
        if [[ "$SKIP_PACKAGE_UPGRADE" == true ]]; then
            log "$pkg ${installed_ver} already installed — keeping (--skip-package-upgrade; ISO has ${deb_ver})"
            return 0
        fi
        if dpkg --compare-versions "$installed_ver" ge "$deb_ver"; then
            log "$pkg ${installed_ver} already installed (ISO has ${deb_ver}) — skipping"
            return 0
        fi
        log "$pkg ${installed_ver} is older than ISO version ${deb_ver} — upgrading..."
    else
        log "$pkg not installed — installing ${deb_ver} from ISO..."
    fi

    log "Installing $(basename "$deb")..."
    if ! dpkg -i "$deb"; then
        log "dpkg -i failed for $pkg — attempting to resolve dependencies with apt-get (requires network)..."
        if ! DEBIAN_FRONTEND=noninteractive apt-get install --fix-broken -y; then
            die "Failed to install $pkg ${deb_ver}: dependency resolution failed. Fix dependencies manually and re-run, or re-run with --skip-package-upgrade to keep the existing version (${installed_ver:-not installed})."
        fi
    fi

    dpkg -s "$pkg" | grep -q "Status: install ok installed" \
        || die "$pkg installation verification failed"
    # apt-get --fix-broken may settle on a version other than the ISO's .deb
    # (e.g. by keeping the old one) — verify what actually ended up installed.
    local final_ver
    final_ver=$(dpkg -s "$pkg" | awk '/^Version:/{print $2}')
    dpkg --compare-versions "$final_ver" ge "$deb_ver" \
        || die "$pkg ended up at ${final_ver}, older than the ISO's ${deb_ver} — dependency repair did not apply the upgrade. Fix manually, or re-run with --skip-package-upgrade to accept ${final_ver}."
    log "$pkg ${final_ver} installed and verified"
}

# ── OS version check ──────────────────────────────────────────────────────────

if [[ "$SKIP_OS_CHECK" == false ]]; then
    log "Checking OS version..."
    os_id=""
    os_version=""
    if [[ -f /etc/os-release ]]; then
        os_id=$(. /etc/os-release && echo "${ID:-}")
        os_version=$(. /etc/os-release && echo "${VERSION_ID:-}")
    fi
    if [[ "$os_id" != "ubuntu" ]] || [[ "$os_version" != "24.04" ]]; then
        die "Unsupported OS: ${os_id} ${os_version}. Ubuntu 24.04 is required. Use --skip-os-check to override."
    fi
    log "OS: Ubuntu ${os_version} — OK"

    # ── Host package check / install / upgrade ───────────────────────────────

    log "Checking libfuse2t64 package (rshim dependency)..."
    install_or_upgrade_pkg libfuse2t64 "libfuse2t64_*.deb"

    log "Checking rshim package..."
    install_or_upgrade_pkg rshim "rshim_*.deb"

    log "Checking DOCA host package..."
    install_or_upgrade_pkg doca-host "doca-host_*.deb"

    log "Verifying DPU host tooling..."
    command -v bfb-install &>/dev/null \
        || die "bfb-install not found after doca-host install — package may be incomplete"
    systemctl list-unit-files rshim.service &>/dev/null && systemctl list-unit-files rshim.service | grep -q rshim \
        || die "rshim service not found after rshim/doca-host install — package may be incomplete"
    log "DPU host tooling verified (bfb-install: $(command -v bfb-install), rshim: OK)"
fi

# ── Set up working directory ───────────────────────────────────────────────────

if [[ "$_work_dir_existed" == true ]]; then
    echo "WARNING: working directory already exists: $WORK_DIR"
    echo "Re-installing will overwrite scripts and artifacts but preserve touchfiles and backups."
    read -r -p "Continue? [y/N] " _confirm
    [[ "$(echo "$_confirm" | tr '[:upper:]' '[:lower:]')" == "y" ]] || die "Aborted."
fi
log "Creating working directory: $WORK_DIR"
mkdir -p "$WORK_DIR/touchfiles"

# ── Copy scripts ───────────────────────────────────────────────────────────────

log "Copying scripts..."
cp "$SCRIPT_DIR/upgrade-dpu-fw.sh"           "$WORK_DIR/"
cp "$SCRIPT_DIR/upgrade-post-power-cycle.sh" "$WORK_DIR/"
cp "$SCRIPT_DIR/upgrade-lib.sh"              "$WORK_DIR/"
cp "$SCRIPT_DIR/dpuinstall.sh"               "$WORK_DIR/"
cp "$SCRIPT_DIR/doca_hbn_versions.cfg"       "$WORK_DIR/"
cp "$SCRIPT_DIR/dpu_fw_version.cfg"          "$WORK_DIR/"
# dpuinstall.sh looks for bf.cfg (not bf.cfg.template) in its own SCRIPTS_DIR
cp "$SCRIPT_DIR/bf.cfg.template"             "$WORK_DIR/bf.cfg"
chmod 755 "$WORK_DIR/upgrade-dpu-fw.sh" \
          "$WORK_DIR/upgrade-post-power-cycle.sh" \
          "$WORK_DIR/dpuinstall.sh"

# ── Copy embedded startup configs (DPU replacement / recovery) ────────────────

if [[ -d "$SCRIPT_DIR/startup-configs" ]]; then
    log "Copying embedded startup configs..."
    rm -rf "$WORK_DIR/startup-configs"
    cp -r "$SCRIPT_DIR/startup-configs" "$WORK_DIR/"
    log "  $(ls "$SCRIPT_DIR/startup-configs" | wc -l | tr -d ' ') config(s) copied"
fi

# ── Copy artifacts ─────────────────────────────────────────────────────────────

log "Copying artifacts..."
_needed_kb=$(du -sk "$SCRIPT_DIR" | cut -f1)
_avail_kb=$(df -Pk "$WORK_DIR" | awk 'NR==2{print $4}')
(( _avail_kb > _needed_kb )) \
    || die "Insufficient space in $WORK_DIR: need ~$(( _needed_kb / 1024 ))MiB, have $(( _avail_kb / 1024 ))MiB"
_copied=0
for pattern in "*.bfb" "*.bfb.gz" "*.deb" "*.deb.gz" "*.tar" "*.tar.gz" "*.zip" "*.zip.gz"; do
    for f in "$SCRIPT_DIR"/$pattern; do
        [[ -e "$f" ]] || continue
        # Skip the full ISO zip — it is not needed at runtime
        [[ "$(basename "$f")" == dpu_upgrade_*.zip* ]] && continue
        cp -f "$f" "$WORK_DIR/"
        log "  $(basename "$f")  ($(du -h "$f" | cut -f1))"
        _copied=$(( _copied + 1 ))
    done
done
[[ "$_copied" -eq 0 ]] && die "No artifacts found in $SCRIPT_DIR — ISO may be incomplete"

# ── Done ───────────────────────────────────────────────────────────────────────

echo
echo "============================================================"
echo "  Install complete."
echo "  Working directory: $WORK_DIR"
echo ""
echo "  To upgrade the DPU on this host, run one of:"
echo "    $WORK_DIR/upgrade-dpu-fw.sh --ssh-key <path-to-dpu-ssh-key>"
echo "    $WORK_DIR/upgrade-dpu-fw.sh --auth password --dpu-user ubuntu"
echo ""
if [[ -d "$WORK_DIR/startup-configs" ]]; then
    echo "  Embedded startup configs (for --startup-yaml-file):"
    for f in "$WORK_DIR/startup-configs"/*; do
        echo "    $f"
    done
    echo ""
fi
echo "  See upgrade-dpu-fw.sh --help for all options."
echo "============================================================"
