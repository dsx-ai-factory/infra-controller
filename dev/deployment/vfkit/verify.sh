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
if [[ "$2" == external ]]; then
  data_device="$(readlink -f /dev/disk/by-id/virtio-nico-data)"
  for directory in /home /var/lib/docker /var/lib/containerd; do
    [[ "$(findmnt -n -o SOURCE -T "${directory}")" == "${data_device}"* ]]
  done
fi
[[ -c /dev/kvm ]]
[[ -r /dev/kvm && -w /dev/kvm ]] || sudo test -r /dev/kvm
ip -brief addr show dev eth0
printf 'Ubuntu ARM64, KVM and guest networking verified. Network mode: %s\n' "$1"
printf 'Gateway reachability and application-level IPv6 require separate end-to-end checks.\n'
