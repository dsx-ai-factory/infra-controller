#!/bin/bash
# Run all unit tests and report an overall pass/fail summary.

set -euo pipefail
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

PASS_FILES=0
FAIL_FILES=0

run_test() {
    local f="$1"
    echo "━━━ $(basename "$f") ━━━"
    if bash "$f"; then
        PASS_FILES=$(( PASS_FILES + 1 ))
    else
        FAIL_FILES=$(( FAIL_FILES + 1 ))
    fi
    echo ""
}

# Syntax-check all scripts before running tests
echo "━━━ syntax check ━━━"
SYNTAX_FAIL=0
for f in \
    "$UNIT_TEST_DIR/../build-dpu-install-iso.sh" \
    "$UNIT_TEST_DIR/../download-build-dpu-artifacts.sh" \
    "$UNIT_TEST_DIR/../on-server/dpuinstall.sh" \
    "$UNIT_TEST_DIR/../on-server/install.sh" \
    "$UNIT_TEST_DIR/../on-server/provision-dpu.sh" \
    "$UNIT_TEST_DIR/../on-server/post-power-cycle.sh" \
    "$UNIT_TEST_DIR/../on-server/setup_netplan.sh" \
    "$UNIT_TEST_DIR/../upgrade/build-dpu-upgrade-iso.sh" \
    "$UNIT_TEST_DIR/../upgrade/on-server/upgrade-install.sh" \
    "$UNIT_TEST_DIR/../upgrade/on-server/upgrade-dpu-fw.sh" \
    "$UNIT_TEST_DIR/../upgrade/on-server/upgrade-post-power-cycle.sh" \
    "$UNIT_TEST_DIR/../upgrade/on-server/upgrade-lib.sh"; do
    printf "  %-55s" "$(basename "$f")"
    if bash -n "$f" 2>&1; then
        echo "OK"
    else
        echo "SYNTAX ERROR"
        SYNTAX_FAIL=$(( SYNTAX_FAIL + 1 ))
    fi
done
echo ""
[ "$SYNTAX_FAIL" -gt 0 ] && { echo "Syntax errors found — fix before running unit tests."; exit 1; }
PASS_FILES=$(( PASS_FILES + 1 ))

for f in "$UNIT_TEST_DIR"/test_*.sh; do
    run_test "$f"
done

echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Test files passed: $PASS_FILES"
echo "Test files failed: $FAIL_FILES"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

[ "$FAIL_FILES" -eq 0 ]
