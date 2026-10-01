#!/bin/bash
# have_tty() decides whether the scripts may prompt on /dev/tty: it performs the
# same open a `read < /dev/tty` would, so it is false exactly where the prompt
# would fail. The function block is sourced verbatim from both shipped scripts;
# the guard it replaced is checked for as a regression.
set -u
UNIT_TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
source "$UNIT_TEST_DIR/lib.sh"

BUILD_SH="$UNIT_TEST_DIR/../build-dpu-install-iso.sh"
INSTALL_SH="$UNIT_TEST_DIR/../on-server/install.sh"

build_block="$(sed -n '/^# >>> tty-detection function/,/^# <<< tty-detection function/p' "$BUILD_SH")"
install_block="$(sed -n '/^# >>> tty-detection function/,/^# <<< tty-detection function/p' "$INSTALL_SH")"

echo "=== the function is shipped identically in both scripts ==="
assert_true "defined in build-dpu-install-iso.sh" '[ -n "$build_block" ]'
assert_eq   "identical in install.sh" "$build_block" "$install_block"
eval "$build_block"
assert_true "have_tty defined" "declare -F have_tty >/dev/null"

echo "=== every prompt guards on have_tty (regression check for the replaced guard) ==="
guards() { grep -v '^[[:space:]]*#' "$1" | grep -c -- '-r /dev/tty'; }
assert_eq "no '-r /dev/tty' left in build-dpu-install-iso.sh" "0" "$(guards "$BUILD_SH")"
assert_eq "no '-r /dev/tty' left in install.sh" "0" "$(guards "$INSTALL_SH")"
assert_eq "every /dev/tty read in install.sh sits under have_tty" "0" \
    "$(awk '/^[[:space:]]*#/{next} /have_tty/{guard=NR} /< \/dev\/tty/ && NR-guard>6 {c++} END{print c+0}' "$INSTALL_SH")"

echo "=== without a controlling terminal have_tty is false ==="
if command -v setsid >/dev/null 2>&1; then
    # setsid starts a new session with no controlling terminal; -w waits for the child.
    assert_false "have_tty is false with no controlling terminal" \
        "setsid -w bash -c '$(printf '%s\n' "$build_block" | sed "s/'/'\\\\''/g"); have_tty' </dev/null"
else
    echo "  SKIP: setsid not available (macOS); the no-terminal case runs in CI (Linux)"
fi

summary
