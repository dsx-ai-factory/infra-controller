#!/bin/bash
# Tests for parse_lshw_bluefield_p0_mac in upgrade/on-server/upgrade-lib.sh.
# The extraction must match the behavior of on-server/setup_netplan.sh: pick
# the first Ethernet interface whose product mentions BlueField and whose
# logical name contains p0.

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"
source "$UNIT_TEST_DIR/../upgrade/on-server/upgrade-lib.sh"

# Wrapper: run the extraction, mapping no-match to empty output.
parse() { parse_lshw_bluefield_p0_mac || true; }

LSHW_BF_BOTH_PORTS='  *-network:0
       description: Ethernet interface
       product: MT43244 BlueField-3 integrated ConnectX-7 network controller
       vendor: Mellanox Technologies
       logical name: enp3s0f0np0
       serial: 08:c0:eb:a1:b2:c3
  *-network:1
       description: Ethernet interface
       product: MT43244 BlueField-3 integrated ConnectX-7 network controller
       vendor: Mellanox Technologies
       logical name: enp3s0f1np1
       serial: 08:c0:eb:a1:b2:c4'

LSHW_NON_BLUEFIELD='  *-network:0
       description: Ethernet interface
       product: Ethernet Controller X710 for 10GbE SFP+
       vendor: Intel Corporation
       logical name: ens1f0np0
       serial: 3c:fd:fe:aa:bb:cc'

LSHW_NON_ETHERNET='  *-network:0
       description: Wireless interface
       product: BlueField imaginary wireless p0
       logical name: wlp0s1
       serial: aa:bb:cc:dd:ee:ff'

LSHW_MIXED='  *-network:0
       description: Ethernet interface
       product: Ethernet Controller X710 for 10GbE SFP+
       vendor: Intel Corporation
       logical name: ens1f0np0
       serial: 3c:fd:fe:aa:bb:cc
  *-network:1
       description: Ethernet interface
       product: MT43244 BlueField-3 integrated ConnectX-7 network controller
       vendor: Mellanox Technologies
       logical name: enp3s0f0np0
       serial: 08:c0:eb:11:22:33'

LSHW_EMPTY=''

echo "=== BlueField p0 extraction ==="
assert_eq "picks p0 of a two-port BlueField" \
    "08:c0:eb:a1:b2:c3" "$(echo "$LSHW_BF_BOTH_PORTS" | parse)"
assert_eq "ignores non-BlueField NICs even with p0 in the name" \
    "" "$(echo "$LSHW_NON_BLUEFIELD" | parse)"
assert_eq "ignores non-Ethernet interfaces" \
    "" "$(echo "$LSHW_NON_ETHERNET" | parse)"
assert_eq "finds the BlueField among mixed NICs" \
    "08:c0:eb:11:22:33" "$(echo "$LSHW_MIXED" | parse)"
assert_eq "empty input yields empty output" \
    "" "$(echo "$LSHW_EMPTY" | parse)"

echo ""
echo "=== extracted value is a valid MAC ==="
_mac="$(echo "$LSHW_BF_BOTH_PORTS" | parse)"
assert_true "extracted MAC passes is_valid_mac" "is_valid_mac '$_mac'"

summary
