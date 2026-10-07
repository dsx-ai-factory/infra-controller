#!/bin/bash
# Shared test helpers — source this at the top of every test file.

_PASS=0
_FAIL=0
_TEST_FILE="${BASH_SOURCE[1]##*/}"

assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [ "$actual" = "$expected" ]; then
        echo "  PASS: $desc"
        _PASS=$(( _PASS + 1 ))
    else
        echo "  FAIL: $desc"
        echo "        expected: $(printf '%q' "$expected")"
        echo "        actual:   $(printf '%q' "$actual")"
        _FAIL=$(( _FAIL + 1 ))
    fi
}

assert_true() {
    local desc="$1"; shift
    if eval "$@" 2>/dev/null; then
        echo "  PASS: $desc"
        _PASS=$(( _PASS + 1 ))
    else
        echo "  FAIL: $desc (condition was false)"
        _FAIL=$(( _FAIL + 1 ))
    fi
}

assert_false() {
    local desc="$1"; shift
    if ! eval "$@" 2>/dev/null; then
        echo "  PASS: $desc"
        _PASS=$(( _PASS + 1 ))
    else
        echo "  FAIL: $desc (condition was true)"
        _FAIL=$(( _FAIL + 1 ))
    fi
}

summary() {
    echo ""
    echo "Results: $_PASS passed, $_FAIL failed"
    [ "$_FAIL" -eq 0 ]
}
