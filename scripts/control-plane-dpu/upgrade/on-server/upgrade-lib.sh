#!/bin/bash
# upgrade-lib.sh
# Shared helpers for the DPU firmware upgrade scripts (upgrade-dpu-fw.sh and
# upgrade-post-power-cycle.sh). Sourced, never executed directly.
#
# These helpers intentionally duplicate small pieces of logic from the
# provisioning scripts (e.g. the lshw BlueField p0 MAC extraction in
# setup_netplan.sh) so the QA'd provisioning scripts stay untouched.

# ── MAC helpers ────────────────────────────────────────────────────────────────

# True when the argument is a colon-separated 6-octet MAC address.
is_valid_mac() { [[ "$1" =~ ^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$ ]]; }

# Lowercase a MAC and strip surrounding whitespace for comparison.
normalize_mac() { echo "$1" | tr -d '[:space:]' | tr '[:upper:]' '[:lower:]'; }

# Case- and whitespace-insensitive MAC equality.
macs_equal() { [ "$(normalize_mac "$1")" = "$(normalize_mac "$2")" ]; }

# Reads `lshw -c network -quiet` output on stdin and prints the MAC address of
# the first BlueField Ethernet interface whose logical name contains "p0".
# Same extraction as setup_netplan.sh. Prints nothing if no match.
parse_lshw_bluefield_p0_mac() {
    awk '$1 == "description:" {desc=$2}
         $1 == "product:"     {product=$0}
         $1 == "logical"      {logical_name=$3}
         $1 == "serial:"      {print logical_name, $2, desc, product}' \
        | grep "Ethernet" | grep "BlueField" | grep p0 | sed -n '1p' | awk '{print $2}'
}

# Detects the BlueField p0 MAC address on this host, retrying while the
# interface appears. Prints the MAC on stdout. Diagnostics go to stderr.
#   detect_bluefield_p0_mac [attempts] [delay-seconds]
detect_bluefield_p0_mac() {
    local attempts="${1:-3}" delay="${2:-10}" mac="" _attempt
    if ! command -v lshw >/dev/null 2>&1; then
        echo "ERROR: lshw not found — required to detect the BlueField p0 MAC address" >&2
        return 1
    fi
    for (( _attempt=1; _attempt<=attempts; _attempt++ )); do
        mac=$(lshw -c network -quiet 2>/dev/null | parse_lshw_bluefield_p0_mac || true)
        [ -n "$mac" ] && break
        if (( _attempt < attempts )); then
            echo "BlueField p0 not found (attempt ${_attempt}/${attempts}), retrying in ${delay}s..." >&2
            sleep "$delay"
        fi
    done
    if [ -z "$mac" ]; then
        echo "ERROR: BlueField p0 network interface not found after ${attempts} attempts." >&2
        return 1
    fi
    if ! is_valid_mac "$mac"; then
        echo "ERROR: detected value is not a MAC address: '$mac' — lshw output format may have changed" >&2
        return 1
    fi
    echo "$mac"
}

# ── DPU login for the pre-upgrade backup ──────────────────────────────────────

UPGRADE_SSH_OPTS=()

# Builds UPGRADE_SSH_OPTS (global array) for logging in to the *running* DPU
# before the upgrade. Two modes:
#   build_upgrade_ssh_opts key <path-to-private-key>
#   build_upgrade_ssh_opts password
# Password mode relies on ssh's own interactive prompt on the console — the
# password is never passed on a command line or stored on disk.
# The caller must set UPGRADE_KNOWN_HOSTS to a file path so the DPU host key
# accepted on first contact is persisted and verified on later connections
# (retries, resume) instead of being re-trusted every time. There is
# deliberately no /dev/null fallback: discarding host keys would let every
# reconnection trust a fresh, unverified endpoint.
build_upgrade_ssh_opts() {
    local mode="$1" key="${2:-}"
    if [ -z "${UPGRADE_KNOWN_HOSTS:-}" ]; then
        echo "ERROR: UPGRADE_KNOWN_HOSTS must be set to a known-hosts file path before building SSH options" >&2
        return 1
    fi
    local -a common=(
        -o StrictHostKeyChecking=accept-new
        -o "UserKnownHostsFile=${UPGRADE_KNOWN_HOSTS}"
        -o ConnectTimeout=10
        -o ServerAliveInterval=30
        -o ServerAliveCountMax=3
    )
    case "$mode" in
        key)
            if [ -z "$key" ]; then
                echo "ERROR: SSH key path required for key auth" >&2
                return 1
            fi
            if [ ! -r "$key" ]; then
                echo "ERROR: SSH key not readable: $key" >&2
                return 1
            fi
            UPGRADE_SSH_OPTS=(-i "$key" -o IdentitiesOnly=yes -o BatchMode=yes "${common[@]}")
            ;;
        password)
            UPGRADE_SSH_OPTS=(
                -o PubkeyAuthentication=no
                -o PreferredAuthentications=password,keyboard-interactive
                -o NumberOfPasswordPrompts=3
                "${common[@]}"
            )
            ;;
        *)
            echo "ERROR: unknown auth mode: $mode" >&2
            return 1
            ;;
    esac
}

# Saved startup.yaml sanity check: must exist and be non-empty.
validate_saved_startup_yaml() {
    local f="$1"
    if [ ! -f "$f" ]; then
        echo "ERROR: saved startup.yaml not found: $f" >&2
        return 1
    fi
    if [ ! -s "$f" ]; then
        echo "ERROR: saved startup.yaml is empty: $f" >&2
        return 1
    fi
}
