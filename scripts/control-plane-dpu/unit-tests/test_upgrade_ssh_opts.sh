#!/bin/bash
# Tests for build_upgrade_ssh_opts and validate_saved_startup_yaml in
# upgrade/on-server/upgrade-lib.sh — the DPU login options used by
# upgrade-dpu-fw.sh for the pre-upgrade backup.

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"
source "$UNIT_TEST_DIR/../upgrade/on-server/upgrade-lib.sh"

_tmpdir="$(mktemp -d)"
trap 'rm -rf "$_tmpdir"' EXIT

# A known-hosts path is mandatory (the lib fails closed without one).
UPGRADE_KNOWN_HOSTS="$_tmpdir/known_hosts"

echo "=== key mode ==="
_key="$_tmpdir/id_test"
touch "$_key"
build_upgrade_ssh_opts key "$_key"
_opts=" ${UPGRADE_SSH_OPTS[*]} "
assert_true  "includes -i <key>"           "[[ \"\$_opts\" == *' -i $_key '* ]]"
assert_true  "includes BatchMode=yes"      "[[ \"\$_opts\" == *'BatchMode=yes'* ]]"
assert_true  "includes IdentitiesOnly=yes" "[[ \"\$_opts\" == *'IdentitiesOnly=yes'* ]]"
assert_false "no password-auth forcing"    "[[ \"\$_opts\" == *'PubkeyAuthentication=no'* ]]"

echo ""
echo "=== password mode ==="
build_upgrade_ssh_opts password
_opts=" ${UPGRADE_SSH_OPTS[*]} "
assert_true  "disables pubkey auth"     "[[ \"\$_opts\" == *'PubkeyAuthentication=no'* ]]"
assert_true  "prefers password auth"    "[[ \"\$_opts\" == *'PreferredAuthentications=password,keyboard-interactive'* ]]"
assert_false "no BatchMode (would block the prompt)" "[[ \"\$_opts\" == *'BatchMode=yes'* ]]"
assert_false "no -i option"             "[[ \"\$_opts\" == *' -i '* ]]"

echo ""
echo "=== known-hosts handling ==="
build_upgrade_ssh_opts password
_opts=" ${UPGRADE_SSH_OPTS[*]} "
assert_true  "uses UPGRADE_KNOWN_HOSTS"    "[[ \"\$_opts\" == *'UserKnownHostsFile=$_tmpdir/known_hosts'* ]]"
assert_false "never falls back to /dev/null" "[[ \"\$_opts\" == *'UserKnownHostsFile=/dev/null'* ]]"
assert_false "fails closed when unset"     "(unset UPGRADE_KNOWN_HOSTS; build_upgrade_ssh_opts password)"
assert_false "fails closed when empty"     "(UPGRADE_KNOWN_HOSTS=''; build_upgrade_ssh_opts key '$_key')"

echo ""
echo "=== error cases ==="
assert_false "key mode without a key path fails"   "build_upgrade_ssh_opts key ''"
assert_false "key mode with missing key fails"     "build_upgrade_ssh_opts key '$_tmpdir/no-such-key'"
assert_false "unknown auth mode fails"             "build_upgrade_ssh_opts agent"

echo ""
echo "=== validate_saved_startup_yaml ==="
_yaml="$_tmpdir/startup.yaml"
assert_false "missing file fails" "validate_saved_startup_yaml '$_yaml'"
touch "$_yaml"
assert_false "empty file fails"   "validate_saved_startup_yaml '$_yaml'"
echo "- set:" > "$_yaml"
assert_true  "non-empty file passes" "validate_saved_startup_yaml '$_yaml'"

summary
