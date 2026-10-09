#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
# Request fresh WAN advertisements before a missed renewal removes IPv6 routing.
set -eu

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ] || { [ "$#" -eq 2 ] && [ "$2" != --once ]; }; then
  echo >&2 'usage: renew-ipv6-router.sh WAN_INTERFACE [--once]'
  exit 1
fi
WAN_IF="$1"
CHECK_INTERVAL=30
RENEW_BEFORE=120
child=""

stop() {
  trap '' INT TERM
  if [ -n "$child" ]; then
    kill -TERM "$child" 2>/dev/null || true
    wait "$child" 2>/dev/null || true
  fi
  exit 0
}
trap stop INT TERM

# Waiting on a background child lets the shell handle shutdown immediately.
run_child() {
  "$@" &
  child=$!
  result=0
  wait "$child" || result=$?
  child=""
  return "$result"
}

check_router() {
  if ! routes="$(ip -6 -o route show default dev "$WAN_IF")"; then
    echo >&2 "Cannot inspect WAN IPv6 routes on $WAN_IF; retrying on the next check"
    return
  fi
  # A permanent route or any usable, sufficiently long-lived default is enough.
  # Ignore reject routes and defaults whose interface is down.
  if printf '%s\n' "$routes" | awk -v threshold="$RENEW_BEFORE" '
    $1 == "default" && $0 !~ /(^| )linkdown( |$)/ {
      lifetime = -1
      for (i = 1; i <= NF; i++) {
        if ($i == "expires") {
          value = $(i + 1)
          lifetime = value ~ /^[0-9]+sec$/ ? value + 0 : 0
        }
      }
      if (lifetime == -1 || lifetime > threshold) healthy = 1
    }
    END { exit healthy ? 0 : 1 }
  '; then
    return
  fi

  echo "Requesting WAN IPv6 router advertisements on $WAN_IF"
  # Wait for multiple responders: a local-only router can answer before the
  # Internet router. Bound both retries and wall time even if replies keep arriving.
  if ! run_child timeout -k 1 5 rdisc6 -n -m -r 1 -w 3000 "$WAN_IF" >/dev/null 2>&1; then
    echo >&2 "Router discovery failed on $WAN_IF; retrying on the next check"
  fi
}

while :; do
  check_router
  [ "${2:-}" != --once ] || break
  run_child sleep "$CHECK_INTERVAL"
done
