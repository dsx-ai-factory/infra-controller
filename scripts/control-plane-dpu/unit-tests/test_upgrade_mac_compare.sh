#!/bin/bash
# Tests for the MAC normalization/comparison helpers used in:
#   upgrade/on-server/upgrade-post-power-cycle.sh — pre/post upgrade MAC check

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"
source "$UNIT_TEST_DIR/../upgrade/on-server/upgrade-lib.sh"

echo "=== normalize_mac ==="
assert_eq "lowercase passthrough" "aa:bb:cc:dd:ee:ff" "$(normalize_mac 'aa:bb:cc:dd:ee:ff')"
assert_eq "uppercase lowered"     "aa:bb:cc:dd:ee:ff" "$(normalize_mac 'AA:BB:CC:DD:EE:FF')"
assert_eq "mixed case lowered"    "08:c0:eb:a1:b2:c3" "$(normalize_mac '08:C0:EB:a1:B2:c3')"
assert_eq "whitespace stripped"   "aa:bb:cc:dd:ee:ff" "$(normalize_mac '  aa:bb:cc:dd:ee:ff ')"
assert_eq "trailing newline stripped" "aa:bb:cc:dd:ee:ff" "$(normalize_mac $'aa:bb:cc:dd:ee:ff\n')"

echo ""
echo "=== macs_equal ==="
assert_true  "identical"            "macs_equal aa:bb:cc:dd:ee:ff aa:bb:cc:dd:ee:ff"
assert_true  "case-insensitive"     "macs_equal AA:BB:CC:DD:EE:FF aa:bb:cc:dd:ee:ff"
assert_true  "whitespace tolerated" "macs_equal ' aa:bb:cc:dd:ee:ff ' aa:bb:cc:dd:ee:ff"
assert_false "different MACs"       "macs_equal aa:bb:cc:dd:ee:ff aa:bb:cc:dd:ee:00"
assert_false "empty vs MAC"         "macs_equal '' aa:bb:cc:dd:ee:ff"
assert_false "single octet differs" "macs_equal 08:c0:eb:a1:b2:c3 08:c0:eb:a1:b2:c4"

summary
