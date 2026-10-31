#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
# Run by cloud-init on the first boot of a VM with its own blank data disk.
set -euo pipefail
disk=/dev/disk/by-id/virtio-nico-data
data_root=/mnt/nico-data
[[ -b "${disk}" ]]
if ! blkid -p "${disk}" >/dev/null 2>&1; then
  mkfs.ext4 -L nico-data "${disk}"
fi
[[ "$(blkid -s TYPE -o value "${disk}")" == ext4 ]]
install -d -m 0755 "${data_root}"
mountpoint -q "${data_root}" || mount "${disk}" "${data_root}"
if ! grep -Fq "${disk} ${data_root} " /etc/fstab; then
  printf '%s %s ext4 defaults 0 2\n' "${disk}" "${data_root}" >>/etc/fstab
fi
for destination in /home /var/lib/docker /var/lib/containerd; do
  source_dir="${data_root}${destination}"
  install -d -m 0755 "${destination}"
  if [[ ! -d "${source_dir}" ]]; then
    install -d -m 0755 "$(dirname "${source_dir}")"
    cp -a "${destination}" "${source_dir}"
  fi
  if ! grep -Fq "${source_dir} ${destination} " /etc/fstab; then
    printf '%s %s none bind,x-systemd.requires-mounts-for=%s 0 0\n' \
      "${source_dir}" "${destination}" "${data_root}" >>/etc/fstab
  fi
  mountpoint -q "${destination}" || mount --bind "${source_dir}" "${destination}"
done
