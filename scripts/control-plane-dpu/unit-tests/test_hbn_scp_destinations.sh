#!/bin/bash
# Tests for SCP destinations and SSH hosts in on-server/dpuinstall.sh.

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

_tmpdir="$(mktemp -d)"
trap 'if [[ $? -ne 0 && -f "$_tmpdir/setup.log" ]]; then cat "$_tmpdir/setup.log" >&2; fi; rm -rf "$_tmpdir"' EXIT

# The installed script loads this generated config beside itself.
cp "$UNIT_TEST_DIR/../on-server/dpuinstall.sh" "$_tmpdir/"
printf '%s\n' 'HBN_CONFIG_SRC_DIR=configs' 'HBN_SCRIPT_DIR=scripts' > "$_tmpdir/doca_hbn_versions.cfg"
# shellcheck source=../on-server/dpuinstall.sh
source "$_tmpdir/dpuinstall.sh"

cd "$_tmpdir"
mkdir dpucfg touchfiles
touch dpucfg/startup.yaml provision_key
DPU_SSH_KEY="$_tmpdir/provision_key"
DPU_SSH_USER=root
DPU_SSH_OPTS=(-o BatchMode=yes)

# Exercise the real setup and transfer functions without contacting a DPU.
ssh() {
    while [[ "$1" == -o ]]; do shift 2; done
    printf '%s\n' "$1" >> "$_tmpdir/ssh_hosts"
    case "${*: -1}" in
        ls\ *) printf '%s\n' '/root/dpucfg/doca_hbn.tar' ;;
    esac
}

scp() {
    _scp_args+=("$@")
}

sleep() { :; }

while IFS='|' read -r host scp_host; do
    DPU_SSH_HOST="$host"
    _scp_args=()
    : > "$_tmpdir/ssh_hosts"
    rm -f "$TOUCHFILE_HBN_SETUP_A" "$TOUCHFILE_HBN_DEPLOYED"

    setup_hbn > "$_tmpdir/setup.log" 2>&1 3>&1

    assert_eq "$host: both SCP destinations" \
        "$(printf '%s\n' -o BatchMode=yes -r dpucfg "root@${scp_host}:/root/" \
            -o BatchMode=yes doca_hbn.tar.gz "root@${scp_host}:/root/dpucfg/")" \
        "$(printf '%s\n' "${_scp_args[@]}")"
    assert_eq "$host: SSH host unchanged" "root@${host}" "$(sort -u "$_tmpdir/ssh_hosts")"
done <<'CASES'
2001:db8::2|[2001:db8::2]
192.168.100.2|192.168.100.2
[2001:db8::2]|[2001:db8::2]
CASES

summary
