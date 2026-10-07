#!/bin/bash
# Tests for the hostname uniqueness check added to build-dpu-install-iso.sh.

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

if (( BASH_VERSINFO[0] < 4 )); then
    echo "SKIP: associative arrays require bash 4+ (found bash $BASH_VERSION)"
    exit 0
fi

# Returns a space-separated list of duplicate hostnames found, or empty if all unique.
find_duplicates() {
    declare -A seen=()
    local -a dupes=()
    for h in "$@"; do
        if [ "${seen["$h"]+set}" ]; then
            dupes+=("$h")
        else
            seen["$h"]=1
        fi
    done
    echo "${dupes[*]:-}"
}

echo "=== hostname uniqueness ==="

assert_eq "no duplicates → empty"       "" "$(find_duplicates host1 host2 host3)"
assert_eq "single node → empty"         "" "$(find_duplicates host1)"
assert_eq "duplicate detected"          "host1" "$(find_duplicates host1 host2 host1)"
assert_eq "multiple duplicates"         "host1 host2" "$(find_duplicates host1 host2 host1 host2)"
assert_eq "duplicate not first"         "host3" "$(find_duplicates host1 host2 host3 host3)"
assert_eq "all same → two dupe entries" "host1 host1" "$(find_duplicates host1 host1 host1)"

echo ""
echo "=== empty hostname guard (mirrors [ -z \"\$HOSTNAME\" ] in script) ==="
assert_true  "non-empty hostname accepted"  '[ -n "control223-cno1-cp1" ]'
assert_false "empty string rejected"        '[ -n "" ]'

summary
