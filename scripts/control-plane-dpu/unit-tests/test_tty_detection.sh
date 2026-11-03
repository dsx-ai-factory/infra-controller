#!/bin/bash
# have_tty() decides whether the scripts may prompt on /dev/tty: it performs the
# same open a `read < /dev/tty` would, so it is false exactly where the prompt
# would fail. The function block is sourced verbatim from both shipped scripts;
# the guard it replaced is checked for as a regression. The no-terminal case
# runs the block in a new session (setsid, or Python where setsid is missing),
# so it is exercised on macOS and Linux alike rather than skipped.
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
# Run a command in a new session, which has no controlling terminal. setsid(1)
# is util-linux or busybox, so macOS has none; there, Python's
# start_new_session forks a child that calls setsid(2) before exec. Either
# way the case runs locally instead of being skipped. No `-w`: busybox setsid
# does not know it, and the callers read the child's stdout, which stays open
# until the child exits, so nothing needs the parent to wait.
no_tty() {
    if command -v setsid >/dev/null 2>&1; then
        setsid "$@" </dev/null
    else
        python3 -c 'import subprocess, sys
sys.exit(subprocess.run(sys.argv[1:], start_new_session=True).returncode)' "$@" </dev/null
    fi
}
if command -v setsid >/dev/null 2>&1 || command -v python3 >/dev/null 2>&1; then
    # Prove the harness first where ps can report a process's terminal: it
    # prints '?' (Linux) or '??' (macOS) for a process with none. Slim images
    # ship no ps and busybox ps takes neither -p nor tty=, so the probe is
    # skipped there rather than failing the file; the have_tty check below is
    # the real assertion and runs regardless.
    if ps -o tty= -p $$ >/dev/null 2>&1; then
        child_tty="$(no_tty bash -c 'ps -o tty= -p $$' | tr -d ' ')"
        assert_true "the new-session child has no controlling terminal (ps: '$child_tty')" \
            '[[ "$child_tty" =~ ^\?+$ ]]'
    else
        echo "  SKIP: ps cannot report a process's terminal here; the have_tty check below still runs"
    fi
    # The function block ends with a comment line, so the call must start a
    # line of its own. Appended with ';' it was part of the comment, the child
    # exited 0 having run nothing, and the assertion failed on any host with
    # setsid (#7211). Echo the status so "never ran" and "ran, returned 1" differ.
    have_tty_result="$(no_tty bash -c "$build_block"$'\n''have_tty; echo "have_tty=$?"')"
    assert_eq "have_tty is false with no controlling terminal" "have_tty=1" "$have_tty_result"
else
    echo "  SKIP: neither setsid nor python3 is available to start a session without a terminal"
fi

summary
