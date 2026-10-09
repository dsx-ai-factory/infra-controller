#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
# Invoked by OpenRC from the apkovl on every boot, including guest reboot.
set -eu
exec >>/var/log/vflab-boot.log 2>&1
finish() {
  result=$?
  if [ "$result" -ne 0 ]; then
    echo "VFLAB_GATEWAY_FAILED $result; see /var/log/vflab-boot.log" > /dev/hvc0
    cat /var/log/vflab-boot.log > /dev/hvc0
  fi
}
trap finish EXIT
# shellcheck disable=SC1091
. /etc/vflab/lab.conf
mkdir -p /mnt/state
modprobe virtiofs
mount -t virtiofs vflab-state /mnt/state
# All other changes live in RAM and are reconstructed from the ISO.
for name in dhcpcd misc; do
  mkdir -p "/mnt/state/$name" "/var/lib/$name"
  mount --bind "/mnt/state/$name" "/var/lib/$name"
done
mkdir -p /mnt/state/ssh
if [ ! -f /mnt/state/ssh/ssh_host_ed25519_key ]; then
  ssh-keygen -q -t ed25519 -N '' -f /mnt/state/ssh/ssh_host_ed25519_key
fi
cp /mnt/state/ssh/ssh_host_ed25519_key* /etc/ssh/
chmod 600 /etc/ssh/ssh_host_ed25519_key
printf '\nHostKey /etc/ssh/ssh_host_ed25519_key\nPasswordAuthentication no\nPermitRootLogin prohibit-password\n' >> /etc/ssh/sshd_config
# The live root account is unlocked; SSH accepts only the injected public key.
sh /usr/local/lib/vflab/networking-gateway.sh "$X" "$LAB_HOSTNAME" "10.$X.0.1" "fd00:$X::1" "$WAN_MAC" "$LAN_MAC"
# Register the configured interfaces with OpenRC's net provider before DNS/RA.
# This starts one dual-stack WAN DHCP client; no preliminary IPv4-only daemon.
rc-service networking start
rc-service sshd start
sh /usr/local/lib/vflab/install-gateway.sh "$X" "$LAB_HOSTNAME" \
  "10.$X.0.1" "fd00:$X::1" "10.$X.0.10" "10.$X.0.250" "fd00:$X::10" "fd00:$X::ff" "$WAN_MAC" "$LAN_MAC"
printf '%s\n' "$X" > /run/vflab-ready
echo "VFLAB_GATEWAY_READY $X (diskless)" > /dev/hvc0
