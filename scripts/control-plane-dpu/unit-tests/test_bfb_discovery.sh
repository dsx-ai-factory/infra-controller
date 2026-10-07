#!/bin/bash
# Tests for the BFB glob-array discovery pattern in
# on-server/dpuinstall.sh (copy_files function).

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

# Run each case in a temp dir so globs don't pick up unrelated files.
# Uses a ( ) subshell, so all functions defined in this script are inherited.
with_tmpdir() {
    local fn="$1"; shift
    local dir; dir=$(mktemp -d)
    ( cd "$dir" && "$fn" "$@" )
    local rc=$?
    rm -rf "$dir"
    return $rc
}

# The shared implementation under test — mirrors copy_files() in dpuinstall.sh.
discover_bfb() {
    local -a _bfbs=(bf-bundle*.bfb)
    if [ ! -f "${_bfbs[0]}" ]; then
        echo "ERROR: BFB file not found" >&2
        return 1
    fi
    if [ "${#_bfbs[@]}" -gt 1 ]; then
        echo "ERROR: multiple BFB files staged: ${_bfbs[*]}" >&2
        return 1
    fi
    echo "${_bfbs[0]}"
}

# ── Per-case setup functions ──────────────────────────────────────────────────
# Each runs inside with_tmpdir, which provides an isolated temp directory.

_case_single_bfb() {
    touch bf-bundle-3.2.2.bfb
    discover_bfb
}

_case_no_bfb() {
    discover_bfb >/dev/null 2>&1
}

_case_multi_bfb() {
    touch bf-bundle-3.2.2.bfb bf-bundle-3.2.3.bfb
    discover_bfb >/dev/null 2>&1
}

_case_gz_not_matched() {
    touch bf-bundle-3.2.2.bfb.gz
    # local -a is valid inside a function; verify the glob *.bfb does not match *.bfb.gz
    local -a _bfbs=(bf-bundle*.bfb)
    [ ! -f "${_bfbs[0]:-}" ]
}

# ── Tests ─────────────────────────────────────────────────────────────────────

echo "=== BFB discovery ==="

assert_eq    "single BFB found"          "bf-bundle-3.2.2.bfb" "$(with_tmpdir _case_single_bfb)"
assert_false "no BFB returns error"      "with_tmpdir _case_no_bfb"
assert_false "multiple BFBs returns error" "with_tmpdir _case_multi_bfb"
assert_true  "glob does not match .bfb.gz" "with_tmpdir _case_gz_not_matched"

summary
