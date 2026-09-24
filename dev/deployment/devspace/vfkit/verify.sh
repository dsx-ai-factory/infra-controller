#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
export PATH="${HOME}/.local/bin:${HOME}/.cargo/bin:${PATH}"
# shellcheck source=/dev/null
source /etc/os-release
[[ "${ID}" == ubuntu && "${VERSION_ID}" == 24.04 ]]
[[ "$(uname -m)" == aarch64 ]]
[[ -n "$(ip -4 -o addr show dev eth0 scope global)" ]]
ip -4 route show default | grep -q '^default '
if [[ "$1" == nat ]]; then
  [[ "$(cat /sys/class/net/eth0/mtu)" == 1280 ]]
fi
if [[ "$1" != nat ]]; then
  [[ -n "$(ip -6 -o addr show dev eth0 scope global)" ]]
  ip -6 route show default | grep -q '^default '
fi
[[ "$(sysctl -n net.ipv6.conf.all.disable_ipv6)" == 0 ]]
[[ "$(findmnt -n -o FSTYPE -T /home/nico/infra-controller)" == ext4 ]]
[[ "$(docker info --format '{{.DockerRootDir}}')" == /var/lib/docker ]]
if [[ "$2" == external ]]; then
  data_device="$(readlink -f /dev/disk/by-id/virtio-nico-data)"
  for directory in /home /var/lib/docker /var/lib/containerd; do
    [[ "$(findmnt -n -o SOURCE -T "${directory}")" == "${data_device}"* ]]
  done
fi
docker buildx version
# After a VM reboot the API can listen before its RBAC informer is ready.
deadline=$((SECONDS + 120))
until kubectl --context kind-nico-dev --request-timeout=5s get node nico-dev-control-plane >/dev/null 2>&1; do
  ((SECONDS < deadline)) || { printf 'kind API did not become ready\n' >&2; exit 1; }
  sleep 2
done
kubectl --context kind-nico-dev wait --for=condition=Ready node/nico-dev-control-plane --timeout=120s
[[ "$(kubectl --context kind-nico-dev get node nico-dev-control-plane -o jsonpath='{.spec.podCIDRs}' | jq length)" == 2 ]]
docker network inspect kind --format '{{.EnableIPv6}}' | grep -qx true
ip -brief addr show dev eth0
printf 'Ubuntu ARM64, Docker and dual-stack kind verified. Network mode: %s\n' "$1"
printf 'Gateway reachability and application-level IPv6 require separate end-to-end checks.\n'
