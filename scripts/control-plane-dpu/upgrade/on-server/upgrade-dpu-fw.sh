#!/bin/bash
# upgrade-dpu-fw.sh
# Upgrades the DPU firmware (BFB) and HBN stack on this host while preserving
# the DPU's existing HBN startup.yaml and the host's existing netplan.
# Run from the working directory created by upgrade-install.sh. Can be re-run
# after a failure — completed steps are skipped via touchfiles in the
# touchfiles/ subdirectory.
#
# Unlike initial provisioning, this flow:
#   * backs up the running DPU's startup.yaml before flashing and redeploys
#     the same config on the upgraded DPU
#   * never generates or applies netplan — the existing host config is kept
#   * records the BlueField p0 MAC before flashing so
#     upgrade-post-power-cycle.sh can verify it did not change
#
# Usage:
#   ./upgrade-dpu-fw.sh --ssh-key <path> [options]
#   ./upgrade-dpu-fw.sh --auth password [options]
#   ./upgrade-dpu-fw.sh --startup-yaml-file <path> [options]
#
# Startup config source (exactly one of):
#   --ssh-key PATH       Fetch the live startup.yaml from the running DPU
#                        over SSH with this private key. On a host provisioned
#                        by this toolchain, the provisioning key at
#                        /root/.dpu_provision/dpu_provision_ed25519 works.
#   --auth password      Fetch the live startup.yaml from the running DPU
#                        over SSH, prompting interactively for the DPU user's
#                        password on the console (the password is never passed
#                        on a command line or written to disk).
#   --startup-yaml-file PATH
#                        Do not log in to the DPU; deploy this local file as
#                        the startup.yaml instead. For DPUs whose credentials
#                        are lost or whose live startup.yaml is corrupted.
#
# Credentials on the upgraded DPU: an existing host-side provisioning SSH key
# is kept (its public key is re-injected into a bf.cfg re-rendered from this
# ISO's template); a fresh key is created when none exists. The DPU's existing
# ubuntu password is KEPT by default when the host has credentials from a
# previous install; pass --replace-ubuntu-password to install this ISO's
# password hash instead. Hosts without previous credentials always receive
# this ISO's password. Partial or corrupt host credentials fail fast with
# remediation instructions before anything destructive happens.
#
# Options:
#   --dpu-user USER      User to log in as on the running DPU (default: root).
#                        With --auth password on a stock BlueField Ubuntu
#                        image, use --dpu-user ubuntu (root password login is
#                        disabled there).
#   --dpu-host HOST      Address of the running DPU for the backup step
#                        (default: 192.168.100.2, the tmfifo address; rshim
#                        and the tmfifo interface are brought up
#                        automatically). After flashing, the DPU is always
#                        reached at 192.168.100.2 over tmfifo.
#   --startup-yaml-path PATH
#                        Path of the live HBN startup.yaml on the DPU
#                        (default: /var/lib/hbn/etc/nvue.d/startup.yaml)
#   --replace-ubuntu-password
#                        Install this upgrade ISO's ubuntu password hash on
#                        the DPU instead of keeping the DPU's existing
#                        password. Without this flag, a host with credentials
#                        from a previous install keeps the existing password;
#                        hosts without previous credentials always receive the
#                        ISO's password.
#   --regenerate-dpu-credentials
#                        Rotate the provisioning SSH key: discard the existing
#                        /root/.dpu_provision key and prepared bf.cfg and
#                        generate fresh ones from this ISO (this implies the
#                        ISO's password hash). Also the remedy for partial or
#                        corrupt credential state.
#   --help               Show this help message

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

# Parse --help before requiring root so anyone can read the usage text.
for _arg in "$@"; do [[ "$_arg" == "--help" || "$_arg" == "-h" ]] && usage; done

[[ "$(id -u)" -ne 0 ]] && die "must be run as root"
LOG_FILE="$SCRIPT_DIR/upgrade.log"
# The log can carry sensitive material (e.g. traced credential handling) —
# keep it root-only regardless of the ambient umask.
touch "$LOG_FILE" && chmod 600 "$LOG_FILE"
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

# Timestamped console log line.
log() { echo "[$(date '+%H:%M:%S')] $*" >&3; }

# ── Validate working directory ─────────────────────────────────────────────────

VERSION_CFG="$SCRIPT_DIR/dpu_fw_version.cfg"
[[ -f "$VERSION_CFG" ]] || die "dpu_fw_version.cfg not found — run upgrade-install.sh first"
# shellcheck source=/dev/null
source "$VERSION_CFG"
[[ -z "${DOCA_VERSION:-}" ]] && die "DOCA_VERSION not set in $VERSION_CFG"
[[ -z "${HBN_VERSION:-}" ]]  && die "HBN_VERSION not set in $VERSION_CFG"

for _f in dpuinstall.sh upgrade-lib.sh bf.cfg doca_hbn_versions.cfg; do
    [[ -f "$SCRIPT_DIR/$_f" ]] || die "$_f not found in $SCRIPT_DIR — run upgrade-install.sh first"
done

# ── Argument parsing ───────────────────────────────────────────────────────────

SSH_KEY=""
AUTH_MODE=""
STARTUP_YAML_FILE=""
DPU_LOGIN_USER="root"
DPU_LOGIN_HOST="192.168.100.2"
STARTUP_YAML_PATH="/var/lib/hbn/etc/nvue.d/startup.yaml"
REGEN_CREDENTIALS=false
REPLACE_UBUNTU_PASSWORD=false

while [[ $# -gt 0 ]]; do
    case "$1" in
        --ssh-key)           [[ -z "${2:-}" ]] && die "--ssh-key requires a value"; [[ -n "$AUTH_MODE" ]] && die "--ssh-key and --auth are mutually exclusive"; SSH_KEY="$2"; AUTH_MODE="key"; shift 2 ;;
        --auth)              [[ "${2:-}" != "password" ]] && die "--auth only supports 'password' (use --ssh-key for key auth)"; [[ -n "$AUTH_MODE" ]] && die "--auth and --ssh-key are mutually exclusive"; AUTH_MODE="password"; shift 2 ;;
        --startup-yaml-file) [[ -z "${2:-}" ]] && die "--startup-yaml-file requires a value"; STARTUP_YAML_FILE="$2"; shift 2 ;;
        --dpu-user)          [[ -z "${2:-}" ]] && die "--dpu-user requires a value"; DPU_LOGIN_USER="$2"; shift 2 ;;
        --dpu-host)          [[ -z "${2:-}" ]] && die "--dpu-host requires a value"; DPU_LOGIN_HOST="$2"; shift 2 ;;
        --startup-yaml-path) [[ -z "${2:-}" ]] && die "--startup-yaml-path requires a value"; STARTUP_YAML_PATH="$2"; shift 2 ;;
        --regenerate-dpu-credentials) REGEN_CREDENTIALS=true; shift ;;
        --replace-ubuntu-password)    REPLACE_UBUNTU_PASSWORD=true; shift ;;
        --help|-h) usage ;;
        *) die "Unknown option: $1" ;;
    esac
done

# Exactly one startup config source: fetch over SSH (key or password) or an
# operator-provided local file.
if [[ -n "$STARTUP_YAML_FILE" ]]; then
    [[ -n "$AUTH_MODE" ]] && die "--startup-yaml-file cannot be combined with --ssh-key/--auth (the DPU login is only used to fetch the config)"
    [[ -f "$STARTUP_YAML_FILE" ]] || die "startup.yaml file not found: $STARTUP_YAML_FILE"
else
    [[ -z "$AUTH_MODE" ]] && die "one of --ssh-key <path>, --auth password, or --startup-yaml-file <path> is required"
    if [[ "$AUTH_MODE" == "key" ]]; then
        [[ -r "$SSH_KEY" ]] || die "SSH key not readable: $SSH_KEY — if the provisioning key is lost but the DPU password works, recover with: $SCRIPT_DIR/upgrade-dpu-fw.sh --auth password --dpu-user ubuntu --regenerate-dpu-credentials"
    fi
fi

# ── Load helpers and the (unchanged) provisioning functions ────────────────────

# shellcheck source=upgrade-lib.sh
source "$SCRIPT_DIR/upgrade-lib.sh"
# The BASH_SOURCE guard in dpuinstall.sh keeps its main install block from
# executing when sourced; only its functions are loaded.
# shellcheck source=/dev/null
source "$SCRIPT_DIR/dpuinstall.sh"

# ── Existing credential sanity check (fail fast, with remediation) ─────────────
# A previous install leaves four credential files; a partial or corrupt set
# would make dpu_ssh_prepare fail mid-flow. Catch it up front, before anything
# destructive, and tell the operator how to recover.

if [[ "$REGEN_CREDENTIALS" != true ]]; then
    _cred_files=("$DPU_SSH_TOUCHFILE" "$DPU_SSH_KEY_SECURE" "${DPU_SSH_KEY_SECURE}.pub" "$_dpu_ssh_bf_prepared")
    _cred_n=0
    for _f in "${_cred_files[@]}"; do
        [[ -f "$_f" ]] && _cred_n=$(( _cred_n + 1 ))
    done
    # Print the prescriptive recovery commands for unusable credentials.
    _cred_recovery_help() {
        echo "" >&3
        echo "RECOVERY — the provisioning SSH key is unusable, but the DPU password still works:" >&3
        echo "  $SCRIPT_DIR/upgrade-dpu-fw.sh --auth password --dpu-user ubuntu --regenerate-dpu-credentials" >&3
        echo "This one command fetches the live startup.yaml using the password (you are" >&3
        echo "prompted on the console), discards the broken credentials, and generates fresh" >&3
        echo "ones. NOTE: the upgraded DPU receives THIS ISO's ubuntu password." >&3
        echo "" >&3
        echo "If the DPU password does not work either, no login is possible — reflash with a" >&3
        echo "saved config instead (add --regenerate-dpu-credentials here too):" >&3
        echo "  $SCRIPT_DIR/upgrade-dpu-fw.sh --startup-yaml-file <saved-startup.yaml> --regenerate-dpu-credentials" >&3
    }
    if (( _cred_n > 0 && _cred_n < 4 )); then
        echo "ERROR: partial DPU provisioning credentials on this host — missing:" >&3
        for _f in "${_cred_files[@]}"; do
            [[ -f "$_f" ]] || echo "  $_f" >&3
        done
        _cred_recovery_help
        exit 1
    fi
    if (( _cred_n == 4 )) && ! ssh-keygen -y -f "$DPU_SSH_KEY_SECURE" >/dev/null 2>&1; then
        echo "ERROR: existing provisioning SSH key at $DPU_SSH_KEY_SECURE is unreadable or corrupt." >&3
        _cred_recovery_help
        exit 1
    fi
fi

trap cleanup EXIT
cd "$SCRIPT_DIR"
mkdir -p "$TOUCHFILES_DIR"
# backup/ holds the DPU config, its live password hash, and the pinned host
# key — root-only, regardless of the ambient umask.
install -d -m 700 "$SCRIPT_DIR/backup"

TOUCHFILE_UPGRADE_BACKUP="$TOUCHFILES_DIR/upgradebackup"
BACKUP_STARTUP_YAML="$SCRIPT_DIR/backup/startup.yaml"
BACKUP_P0_MAC="$SCRIPT_DIR/backup/p0_mac"
BACKUP_UBUNTU_SHADOW="$SCRIPT_DIR/backup/ubuntu_shadow"

# ── Pre-upgrade backup ─────────────────────────────────────────────────────────

# Save the DPU's live startup.yaml (or stage the operator-provided file),
# capture the live ubuntu password hash when readable, and record the p0 MAC.
# Idempotent via the upgradebackup touchfile; nothing destructive runs before
# this completes.
upgrade_backup() {
    if [ -f "$TOUCHFILE_UPGRADE_BACKUP" ]; then
        echo "Pre-upgrade backup already completed, skipping. Remove $TOUCHFILE_UPGRADE_BACKUP to force." >&3
        validate_saved_startup_yaml "$BACKUP_STARTUP_YAML" \
            || die "backup touchfile exists but saved startup.yaml is missing or empty: $BACKUP_STARTUP_YAML"
        [ -s "$BACKUP_P0_MAC" ] \
            || die "backup touchfile exists but saved p0 MAC is missing: $BACKUP_P0_MAC"
        return 0
    fi

    echo "" >&3
    echo "==== Pre-upgrade backup ====" >&3

    if [ -n "$STARTUP_YAML_FILE" ]; then
        # Replacement mode: no DPU login — deploy the operator-provided config
        # (DPU credentials lost, or the live startup.yaml is corrupted).
        echo "Using operator-provided startup.yaml: $STARTUP_YAML_FILE (skipping DPU fetch)" >&3
        validate_saved_startup_yaml "$STARTUP_YAML_FILE" \
            || die "provided startup.yaml is missing or empty: $STARTUP_YAML_FILE"
        if ! grep -q "set" "$STARTUP_YAML_FILE"; then
            echo "WARNING: provided startup.yaml has no 'set' entries — it may not be an NVUE config. Continuing anyway." >&3
        fi
        install -m 600 "$STARTUP_YAML_FILE" "$BACKUP_STARTUP_YAML"
        echo "Staged replacement startup.yaml at $BACKUP_STARTUP_YAML" >&3
    else
        # Reach the running DPU. The default tmfifo address needs the interface
        # up first; any other address must already be reachable from this host.
        if [ "$DPU_LOGIN_HOST" = "192.168.100.2" ]; then
            setup_tmfifo
        fi

        # Persist the DPU host key across retries/resume: first contact is
        # trusted (accept-new), later connections must present the same key.
        UPGRADE_KNOWN_HOSTS="$SCRIPT_DIR/backup/known_hosts"
        build_upgrade_ssh_opts "$AUTH_MODE" "$SSH_KEY" \
            || die "could not set up SSH options for the DPU login"

        # One SSH session fetches both the live startup.yaml and the DPU's
        # LIVE ubuntu password hash (it may have been rotated since initial
        # provisioning, in which case the prepared bf.cfg's copy is stale).
        # A single session means password auth prompts only once. The shadow
        # read is best-effort (`|| true`): it needs root or passwordless sudo.
        local remote_cmd _shadow_marker="__DPU_UPGRADE_SHADOW__"
        if [ "$DPU_LOGIN_USER" = "root" ]; then
            remote_cmd="{ cat '$STARTUP_YAML_PATH'; echo '$_shadow_marker'; getent shadow ubuntu || true; }"
        else
            remote_cmd="{ sudo -n cat '$STARTUP_YAML_PATH' 2>/dev/null || cat '$STARTUP_YAML_PATH'; echo '$_shadow_marker'; sudo -n getent shadow ubuntu 2>/dev/null || true; }"
        fi

        echo "Fetching $STARTUP_YAML_PATH from ${DPU_LOGIN_USER}@${DPU_LOGIN_HOST}..." >&3
        if [ "$AUTH_MODE" = "password" ]; then
            echo "You will be prompted for the password of '${DPU_LOGIN_USER}' on the DPU." >&3
        fi

        local _raw _tmp
        _raw="$(mktemp "$SCRIPT_DIR/backup/fetch.XXXXXX")"
        _tmp="$(mktemp "$SCRIPT_DIR/backup/startup.yaml.XXXXXX")"
        chmod 600 "$_raw" "$_tmp"
        if ! ssh "${UPGRADE_SSH_OPTS[@]}" "${DPU_LOGIN_USER}@${DPU_LOGIN_HOST}" "$remote_cmd" > "$_raw"; then
            rm -f "$_raw" "$_tmp"
            echo "ERROR: failed to fetch $STARTUP_YAML_PATH from ${DPU_LOGIN_USER}@${DPU_LOGIN_HOST} — check credentials and that the DPU is reachable (see $LOG_FILE)." >&3
            if [ "$AUTH_MODE" = "key" ]; then
                echo "" >&3
                echo "RECOVERY — if the DPU no longer accepts this key but its password works:" >&3
                echo "  $SCRIPT_DIR/upgrade-dpu-fw.sh --auth password --dpu-user ubuntu" >&3
                echo "(no --regenerate-dpu-credentials needed: the host key is intact and is" >&3
                echo "re-injected during the flash, so key login works again after the upgrade)" >&3
            fi
            echo "" >&3
            echo "If no login works at all, reflash with a saved config:" >&3
            echo "  $SCRIPT_DIR/upgrade-dpu-fw.sh --startup-yaml-file <saved-startup.yaml>" >&3
            exit 1
        fi
        # Split the combined fetch: startup.yaml before the marker, the shadow
        # entry after it. Suppress xtrace while hash material is handled so it
        # never reaches the log.
        local _xt=false
        case "$-" in *x*) _xt=true; set +x ;; esac
        # Pre-create the shadow file 0600 so the hash is never on disk with
        # permissive modes, even briefly (awk's > truncates, keeping the mode).
        install -m 600 /dev/null "$BACKUP_UBUNTU_SHADOW"
        awk -v marker="$_shadow_marker" -v yaml="$_tmp" -v shadow="$BACKUP_UBUNTU_SHADOW" \
            '$0 == marker {found=1; next} !found {print > yaml} found {print > shadow}' "$_raw"
        rm -f "$_raw"
        if [ -s "$BACKUP_UBUNTU_SHADOW" ]; then
            local _live_hash
            _live_hash="$(cut -d: -f2 "$BACKUP_UBUNTU_SHADOW")"
            if [ "${_live_hash#\$}" != "$_live_hash" ]; then
                [ "$_xt" = true ] && set -x
                echo "Captured the DPU's live ubuntu password hash — 'keep password' will preserve it." >&3
            else
                rm -f "$BACKUP_UBUNTU_SHADOW"
                [ "$_xt" = true ] && set -x
                echo "NOTE: the DPU's live password hash is unavailable or not a crypt hash — 'keep password' will use the hash recorded at initial provisioning." >&3
            fi
        else
            rm -f "$BACKUP_UBUNTU_SHADOW"
            [ "$_xt" = true ] && set -x
            echo "NOTE: could not read the DPU's live password hash (needs root or passwordless sudo) — 'keep password' will use the hash recorded at initial provisioning." >&3
        fi

        validate_saved_startup_yaml "$_tmp" || { rm -f "$_tmp"; die "fetched startup.yaml is empty — refusing to continue"; }
        if ! grep -q "set" "$_tmp"; then
            echo "WARNING: saved startup.yaml has no 'set' entries — it may not be an NVUE config. Continuing anyway." >&3
        fi
        mv "$_tmp" "$BACKUP_STARTUP_YAML"
        echo "Saved DPU startup.yaml to $BACKUP_STARTUP_YAML" >&3
    fi

    echo "Recording BlueField p0 MAC address..." >&3
    local mac
    mac="$(detect_bluefield_p0_mac 3 10)" \
        || die "could not detect the BlueField p0 MAC — cannot validate it after the upgrade (see $LOG_FILE)"
    echo "$mac" > "$BACKUP_P0_MAC"
    echo "Saved p0 MAC: $mac" >&3

    touch "$TOUCHFILE_UPGRADE_BACKUP"
    echo "Pre-upgrade backup complete." >&3
    echo "" >&3
}

# ── bf.cfg refresh (fleet credential convergence) ──────────────────────────────
# When this host already has provisioning credentials from an earlier install,
# dpu_ssh_prepare would reuse the OLD prepared bf.cfg — flashing the DPU with
# the password hash and settings of the ORIGINAL install ISO instead of this
# upgrade ISO's. Re-render the prepared bf.cfg from this ISO's template,
# re-injecting the EXISTING key's public key, so every DPU converges on this
# ISO's credentials while keeping SSH-key continuity (no lockout on resume).
# Uses the path variables defined by the sourced (unchanged) dpuinstall.sh.

# Re-render the prepared bf.cfg from this ISO's template, keeping the existing
# SSH key and (by default) the DPU's current ubuntu password. No-op after the
# flash or when no complete credential set exists.
refresh_prepared_bf_cfg() {
    # After a successful flash the injected config is already on the DPU;
    # leave the resume path untouched.
    if [ -f "$TOUCHFILE_BFB_UPDATED" ]; then
        return 0
    fi

    local key="$DPU_SSH_KEY_SECURE"
    local -a files=("$DPU_SSH_TOUCHFILE" "$key" "${key}.pub" "$_dpu_ssh_bf_prepared")
    for f in "${files[@]}"; do
        # Anything missing: let dpu_ssh_prepare handle fresh generation (none
        # exist) or report partial state (some exist).
        [ -f "$f" ] || return 0
    done

    echo "Existing DPU provisioning credentials found — refreshing bf.cfg from this ISO (its settings will apply; the existing SSH key is kept)." >&3
    local pub_line _tmp
    pub_line="$(ssh-keygen -y -f "$key")" \
        || die "could not derive public key from $key — use --regenerate-dpu-credentials to replace corrupt credentials"
    _tmp="$(mktemp /root/.dpu_provision/bf.cfg.XXXXXX)"
    chmod 600 "$_tmp"
    sed "s|${_dpu_ssh_pubkey_placeholder}|${pub_line}|g" "$_dpu_ssh_bf_src" > "$_tmp"
    if ! grep -qF "$pub_line" "$_tmp"; then
        rm -f "$_tmp"
        die "bf.cfg refresh failed: could not inject the provisioning public key"
    fi

    # Password policy: keep the DPU's existing ubuntu password by default (it
    # lives in the old prepared bf.cfg); install this ISO's hash only when
    # asked. Crypt hashes contain only [./$A-Za-z0-9], so the line is safe to
    # carry through awk verbatim.
    if [ "$REPLACE_UBUNTU_PASSWORD" = true ]; then
        echo "--replace-ubuntu-password: the DPU will receive this ISO's ubuntu password hash." >&3
    else
        # Suppress xtrace while the password hash is in shell variables so it
        # never lands in the log via the trace output.
        local old_pw_line="" _live_hash _xtrace_was_on=false
        case "$-" in *x*) _xtrace_was_on=true; set +x ;; esac
        # Prefer the LIVE hash captured from the DPU during the backup — the
        # prepared bf.cfg's copy is the provisioning-time hash and is stale if
        # the password was rotated on the DPU since.
        if [ -s "$BACKUP_UBUNTU_SHADOW" ]; then
            _live_hash="$(cut -d: -f2 "$BACKUP_UBUNTU_SHADOW")"
            [ "${_live_hash#\$}" != "$_live_hash" ] && old_pw_line="ubuntu_PASSWORD='${_live_hash}'"
        fi
        [ -z "$old_pw_line" ] && old_pw_line="$(grep -m1 '^ubuntu_PASSWORD=' "$_dpu_ssh_bf_prepared" || true)"
        if [ -n "$old_pw_line" ]; then
            awk -v repl="$old_pw_line" '/^ubuntu_PASSWORD=/ {print repl; next} {print}' "$_tmp" > "${_tmp}.pw"
            chmod 600 "${_tmp}.pw"
            mv -f "${_tmp}.pw" "$_tmp"
            [ "$_xtrace_was_on" = true ] && set -x
            echo "Keeping the DPU's existing ubuntu password (pass --replace-ubuntu-password to install this ISO's instead)." >&3
        else
            [ "$_xtrace_was_on" = true ] && set -x
            echo "WARNING: existing bf.cfg has no ubuntu_PASSWORD line — the DPU will receive this ISO's password hash." >&3
        fi
    fi

    mv "$_tmp" "$_dpu_ssh_bf_prepared"
    echo "Prepared bf.cfg refreshed at $_dpu_ssh_bf_prepared" >&3
}

# ── Run the upgrade ────────────────────────────────────────────────────────────

log "Upgrading DPU — DOCA: $DOCA_VERSION  HBN: $HBN_VERSION"
if [[ -n "$STARTUP_YAML_FILE" ]]; then
    log "Startup config source: operator-provided file $STARTUP_YAML_FILE (no DPU login)"
else
    log "Startup config source: fetch from ${DPU_LOGIN_USER}@${DPU_LOGIN_HOST} (auth: $AUTH_MODE)"
fi

# The provisioning functions in dpuinstall.sh were QA'd running under
# `set -eux` without nounset/pipefail (see its main block). Match that exact
# runtime before invoking them.
set +o pipefail
set +u
set -eux

copy_files
start_rshim

# Back up BEFORE any credential mutation: a fetch that relies on the old
# provisioning key (--ssh-key /root/.dpu_provision/...) must complete before
# --regenerate-dpu-credentials or the bf.cfg refresh can touch anything.
upgrade_backup

if [ "$REGEN_CREDENTIALS" = true ]; then
    # Never wipe after a completed flash: the flashed DPU authorizes the
    # CURRENT host key and install_bfb is skipped on resume, so a fresh key
    # could never be injected — the host would be locked out of the DPU.
    if [ -f "$TOUCHFILE_BFB_UPDATED" ]; then
        echo "--regenerate-dpu-credentials ignored: the DPU was already flashed in an earlier run and authorizes the current host key. Continuing the resume; regenerate on the next upgrade instead." >&3
    else
        echo "Regenerating DPU provisioning credentials (--regenerate-dpu-credentials)..." >&3
        rm -rf /root/.dpu_provision
        rm -f "$DPU_SSH_TOUCHFILE"
    fi
fi

refresh_prepared_bf_cfg
dpu_ssh_prepare

# stage_hbn_config reads the STARTUP_YAML global — point it at the backup so
# the upgraded DPU comes back with its previous configuration.
STARTUP_YAML="$BACKUP_STARTUP_YAML"
stage_hbn_config
install_bfb
setup_tmfifo
setup_hbn

# The netplan step of the install flow is intentionally skipped: the existing
# host netplan stays in place and is validated by upgrade-post-power-cycle.sh.

echo "" >&3
echo "After the power cycle completes and the host is back up, run:" >&3
echo "  $SCRIPT_DIR/upgrade-post-power-cycle.sh" >&3
echo "" >&3

power_cycle
