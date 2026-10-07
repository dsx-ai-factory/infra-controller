#!/bin/bash
# Tests for the IP arithmetic helpers and prefix capacity validation in
# build-dpu-install-iso.sh (get_nth_addr, ip_to_int, int_to_ip, split_in_half,
# and the loopback/control-plane prefix capacity checks added in #3796).

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

# ── Functions under test (copied verbatim from build-dpu-install-iso.sh) ──────

ip_to_int() {
    local ip="$1"
    IFS='.' read -r a b c d <<< "$ip"
    echo $(( (a << 24) | (b << 16) | (c << 8) | d ))
}

int_to_ip() {
    local n=$1
    echo "$(( (n >> 24) & 255 )).$(( (n >> 16) & 255 )).$(( (n >> 8) & 255 )).$(( n & 255 ))"
}

get_nth_addr() {
    local cidr="$1" n="$2"
    local ip="${cidr%/*}" prefix="${cidr#*/}"
    local net_int
    net_int=$(( $(ip_to_int "$ip") & ( 0xFFFFFFFF << (32 - prefix) & 0xFFFFFFFF ) ))
    int_to_ip $(( net_int + n ))
}

split_in_half() {
    local cidr="$1"
    local ip="${cidr%/*}" prefix="${cidr#*/}"
    local new_prefix=$(( prefix + 1 ))
    local net_int
    net_int=$(( $(ip_to_int "$ip") & ( 0xFFFFFFFF << (32 - prefix) & 0xFFFFFFFF ) ))
    local half_size=$(( 1 << (32 - new_prefix) ))
    echo "$(int_to_ip "$net_int")/$new_prefix $(int_to_ip $(( net_int + half_size )))/$new_prefix"
}

prefix_capacity() { local prefix="$1"; echo $(( 1 << (32 - prefix) )); }

echo "=== ip_to_int / int_to_ip ==="
assert_eq "0.0.0.0 → 0"           "0"          "$(ip_to_int 0.0.0.0)"
assert_eq "255.255.255.255 → max"  "4294967295" "$(ip_to_int 255.255.255.255)"
assert_eq "10.0.0.1 → int"        "167772161"  "$(ip_to_int 10.0.0.1)"
assert_eq "round-trip 10.1.2.3"   "10.1.2.3"   "$(int_to_ip "$(ip_to_int 10.1.2.3)")"
assert_eq "round-trip 192.168.1.100" "192.168.1.100" "$(int_to_ip "$(ip_to_int 192.168.1.100)")"

echo ""
echo "=== get_nth_addr ==="
assert_eq "offset 0 = network addr"  "10.0.0.0"   "$(get_nth_addr 10.0.0.0/24 0)"
assert_eq "offset 1"                 "10.0.0.1"   "$(get_nth_addr 10.0.0.0/24 1)"
assert_eq "offset 3"                 "10.0.0.3"   "$(get_nth_addr 10.0.0.0/24 3)"
assert_eq "non-zero base is masked"  "10.0.0.1"   "$(get_nth_addr 10.0.0.5/24 1)"
assert_eq "/31 offset 0"             "10.0.0.0"   "$(get_nth_addr 10.0.0.0/31 0)"
assert_eq "/31 offset 1"             "10.0.0.1"   "$(get_nth_addr 10.0.0.0/31 1)"
assert_eq "/26 loopback node 1"      "7.243.97.65" "$(get_nth_addr 7.243.97.64/26 1)"
assert_eq "/26 loopback node 3"      "7.243.97.67" "$(get_nth_addr 7.243.97.64/26 3)"

echo ""
echo "=== split_in_half ==="
assert_eq "/24 splits into two /25s"  \
    "10.0.0.0/25 10.0.0.128/25" \
    "$(split_in_half 10.0.0.0/24)"
assert_eq "/25 splits into two /26s"  \
    "10.0.0.0/26 10.0.0.64/26" \
    "$(split_in_half 10.0.0.0/25)"

echo ""
echo "=== prefix capacity validation (loopback: capacity > NODE_COUNT) ==="
# /26 = 64 addresses; passes for 3 nodes (64 > 3)
cap=$(prefix_capacity 26); assert_true  "/26 fits 3 nodes"     "[ $cap -gt 3 ]"
# /30 = 4 addresses; passes for 3 nodes (4 > 3)
cap=$(prefix_capacity 30); assert_true  "/30 fits 3 nodes"     "[ $cap -gt 3 ]"
# /30 = 4 addresses; fails for 4 nodes (4 > 4 is false)
cap=$(prefix_capacity 30); assert_false "/30 too small for 4"  "[ $cap -gt 4 ]"
# /32 = 1 address; fails for any nodes
cap=$(prefix_capacity 32); assert_false "/32 too small for 1"  "[ $cap -gt 1 ]"

echo ""
echo "=== prefix capacity validation (control-plane: capacity >= NODE_COUNT * 2) ==="
# /27 = 32 addresses; passes for 3 nodes (32 >= 6)
cap=$(prefix_capacity 27); assert_true  "/27 fits 3 nodes"     "[ $cap -ge $(( 3 * 2 )) ]"
# /29 = 8 addresses; passes for 3 nodes (8 >= 6)
cap=$(prefix_capacity 29); assert_true  "/29 fits 3 nodes"     "[ $cap -ge $(( 3 * 2 )) ]"
# /29 = 8 addresses; fails for 5 nodes (8 >= 10 is false)
cap=$(prefix_capacity 29); assert_false "/29 too small for 5"  "[ $cap -ge $(( 5 * 2 )) ]"
# /30 = 4 addresses; fails for 3 nodes (4 >= 6 is false)
cap=$(prefix_capacity 30); assert_false "/30 too small for 3"  "[ $cap -ge $(( 3 * 2 )) ]"

summary
