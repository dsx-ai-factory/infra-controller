#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
# Disposable live VM: resolve and verify the offline package closure.
set -eu
exec > /dev/hvc0 2>&1
mkdir -p /mnt/build
modprobe virtiofs
mount -t virtiofs vflab-build /mnt/build
finish() {
  result=$?
  [ "$result" -eq 0 ] || echo "$result" > /mnt/build/failed
  sync
  poweroff
}
trap finish EXIT
ip link set lo up
ip link set eth0 mtu 1280
printf 'interface eth0\n  nooption interface_mtu\n' >> /etc/dhcpcd.conf
dhcpcd -w -4 -t 60 eth0
cp /mnt/build/repositories /etc/apk/repositories
apk update
mkdir -p /mnt/build/apks/aarch64
# Package names are controlled by the checked-in world file.
# shellcheck disable=SC2046
apk fetch --recursive --output /mnt/build/apks/aarch64 $(cat /mnt/build/world)
apk verify /mnt/build/apks/aarch64/*.apk
# The repository layout is aarch64 even for architecture-independent packages.
apk index --rewrite-arch aarch64 -o /mnt/build/apks/aarch64/APKINDEX.tar.gz /mnt/build/apks/aarch64/*.apk
touch /mnt/build/success
