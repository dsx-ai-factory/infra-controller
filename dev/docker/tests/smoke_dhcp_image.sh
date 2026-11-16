#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Check packaging and hook loading without a cluster, credentials or external traffic.
set -euo pipefail
if [[ $# != 1 ]]; then
    echo "Usage: $0 IMAGE" >&2
    exit 2
fi

docker run --rm --network none \
    --sysctl net.ipv6.conf.all.disable_ipv6=0 \
    --sysctl net.ipv6.conf.default.disable_ipv6=0 \
    --entrypoint sh -i "$1" -eu <<'CONTAINER'
ip -V
ss -V
curl --version

cat > /tmp/dhcp4.json <<'JSON'
{"Dhcp4":{"interfaces-config":{"interfaces":["lo"],"dhcp-socket-type":"udp"},"lease-database":{"type":"memfile","persist":false},"valid-lifetime":120,"hooks-libraries":[{"library":"/usr/lib/kea/hooks/libdhcp.so"}]}}
JSON
cat > /tmp/dhcp6.json <<'JSON'
{"Dhcp6":{"interfaces-config":{"interfaces":["lo/::1"]},"server-id":{"type":"LL","htype":1,"identifier":"000102030405","persist":false},"lease-database":{"type":"memfile","persist":false},"valid-lifetime":120,"hooks-libraries":[{"library":"/usr/lib/kea/hooks/libdhcp.so"}]}}
JSON

for family in 4 6; do
    /sbin/kea-dhcp"$family" -V
    /sbin/kea-dhcp"$family" -c /tmp/dhcp"$family".json > /tmp/kea.log 2>&1 &
    server_pid=$!
    trap 'kill "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true' EXIT
    attempts=0
    until grep -q "DHCP${family}_STARTED" /tmp/kea.log; do
        attempts=$((attempts + 1))
        if ! kill -0 "$server_pid" 2>/dev/null || [ "$attempts" -ge 40 ]; then
            cat /tmp/kea.log
            exit 1
        fi
        sleep 0.25
    done
    grep 'HOOKS_LIBRARY_LOADED.*libdhcp.so' /tmp/kea.log
    if [ "$family" = 4 ]; then port=67; else port=547; fi
    ss -H -"$family" -lun "sport = :$port" | grep -q .
    kill "$server_pid"
    wait "$server_pid"
    trap - EXIT
    printf 'DHCP%s: hook loaded and UDP listener started\n' "$family"
done
CONTAINER
