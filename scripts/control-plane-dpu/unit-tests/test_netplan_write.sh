#!/bin/bash
# Tests for the atomic netplan config write in on-server/setup_netplan.sh:
#   - placeholder presence check before write
#   - substitution completeness assertion
#   - temp-file write + atomic mv
#   - output file permissions (600)
#   - backup creation when destination exists

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

PLACEHOLDER_MAC="aa:aa:aa:aa:aa:aa"

# Mirrors the logic in setup_netplan.sh after the fix.
write_netplan() {
    local src="$1" dest="$2" mac="$3"
    grep -qF "$PLACEHOLDER_MAC" "$src" || { echo "ERROR: placeholder not in src" >&2; return 1; }
    local tmp; tmp="$(mktemp)"
    chmod 600 "$tmp"
    sed "s/$PLACEHOLDER_MAC/$mac/g" "$src" > "$tmp"
    grep -qF "$PLACEHOLDER_MAC" "$tmp" && { rm -f "$tmp"; echo "ERROR: substitution incomplete" >&2; return 1; }
    [[ -f "$dest" ]] && cp -a "$dest" "${dest}.bak"
    mv "$tmp" "$dest"
}

echo "=== placeholder presence check ==="

src=$(mktemp); dest=$(mktemp)
echo "mac: $PLACEHOLDER_MAC" > "$src"
assert_true  "src with placeholder succeeds" "write_netplan '$src' '$dest' '11:22:33:44:55:66'"

src_no_ph=$(mktemp)
echo "mac: 00:00:00:00:00:00" > "$src_no_ph"
assert_false "src without placeholder fails"  "write_netplan '$src_no_ph' '$dest' '11:22:33:44:55:66' 2>/dev/null"

echo ""
echo "=== substitution result ==="

echo "mac: $PLACEHOLDER_MAC" > "$src"
write_netplan "$src" "$dest" "de:ad:be:ef:00:01" 2>/dev/null
assert_eq "placeholder replaced in dest" "mac: de:ad:be:ef:00:01" "$(cat "$dest")"
assert_false "placeholder absent from dest" "grep -qF '$PLACEHOLDER_MAC' '$dest'"

echo ""
echo "=== output file permissions ==="

echo "mac: $PLACEHOLDER_MAC" > "$src"
write_netplan "$src" "$dest" "11:22:33:44:55:66" 2>/dev/null
perms=$(stat -c "%a" "$dest" 2>/dev/null || stat -f "%OLp" "$dest" 2>/dev/null)
assert_eq "dest is mode 600" "600" "$perms"

echo ""
echo "=== backup on overwrite ==="

echo "mac: $PLACEHOLDER_MAC" > "$src"
echo "old config" > "$dest"
write_netplan "$src" "$dest" "aa:aa:bb:bb:cc:cc" 2>/dev/null
assert_true  "backup file created"        "[ -f '${dest}.bak' ]"
assert_eq    "backup contains old content" "old config" "$(cat "${dest}.bak")"
assert_false "new dest still has placeholder" "grep -qF '$PLACEHOLDER_MAC' '$dest'"

echo ""
echo "=== no backup when dest does not exist ==="

rm -f "$dest" "${dest}.bak"
echo "mac: $PLACEHOLDER_MAC" > "$src"
write_netplan "$src" "$dest" "11:22:33:44:55:66" 2>/dev/null
assert_false "no spurious backup created" "[ -f '${dest}.bak' ]"

rm -f "$src" "$src_no_ph" "$dest" "${dest}.bak"

summary
