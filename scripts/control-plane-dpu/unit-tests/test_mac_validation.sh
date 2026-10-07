#!/bin/bash
# Tests for MAC address format validation used in:
#   build-dpu-install-iso.sh  — DPU_MAC format check
#   on-server/setup_netplan.sh — captured MAC assertion

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

is_valid_mac() { [[ "$1" =~ ^([0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$ ]]; }

echo "=== valid MACs ==="
assert_true  "all lowercase"          "is_valid_mac aa:bb:cc:dd:ee:ff"
assert_true  "all uppercase"          "is_valid_mac AA:BB:CC:DD:EE:FF"
assert_true  "mixed case"             "is_valid_mac 00:1A:2b:3C:4d:5E"
assert_true  "all zeros"              "is_valid_mac 00:00:00:00:00:00"
assert_true  "all FF"                 "is_valid_mac ff:ff:ff:ff:ff:ff"
assert_true  "real BlueField MAC"     "is_valid_mac 08:c0:eb:a1:b2:c3"

echo ""
echo "=== invalid MACs ==="
assert_false "empty string"           "is_valid_mac ''"
assert_false "too short (5 groups)"   "is_valid_mac aa:bb:cc:dd:ee"
assert_false "too long (7 groups)"    "is_valid_mac aa:bb:cc:dd:ee:ff:00"
assert_false "wrong separator (dash)" "is_valid_mac aa-bb-cc-dd-ee-ff"
assert_false "no separator"           "is_valid_mac aabbccddeeff"
assert_false "non-hex chars"          "is_valid_mac gg:hh:ii:jj:kk:ll"
assert_false "group too short"        "is_valid_mac a:bb:cc:dd:ee:ff"
assert_false "group too long"         "is_valid_mac aaa:bb:cc:dd:ee:ff"
assert_false "plain text"             "is_valid_mac not-a-mac"
assert_false "whitespace"             "is_valid_mac 'aa:bb:cc:dd:ee: ff'"

summary
