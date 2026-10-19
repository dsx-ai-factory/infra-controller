#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -euo pipefail

artifacts="${1:?Usage: verify_scout_network.sh ARTIFACT_DIRECTORY}"
repository="$(cd "$(dirname "$0")/../.." && pwd)"
scratch="$(mktemp -d)"
# Extracted images contain read-only directories; make our copy removable.
trap 'chmod -R u+w "$scratch"; rm -rf "$scratch"' EXIT
trap 'echo "Scout network verification failed at line $LINENO" >&2' ERR

# The UKI can prepend uncompressed microcode to its compressed root archive.
# `unmkinitramfs` handles both layouts without changing the published EFI file.
objcopy -O binary --only-section=.initrd "$artifacts/scout.efi" "$scratch/loader.initrd"
unmkinitramfs "$scratch/loader.initrd" "$scratch/loader"
loader="$scratch/loader"
if [[ -d "$loader/main" ]]; then
    loader="$loader/main"
fi
unsquashfs -no-progress -d "$scratch/scout" "$artifacts/scout.squashfs"

for root in "$loader" "$scratch/scout"; do
    echo "Checking Scout networking in $root"
    test -x "$root/opt/forge/forge-scout-network.sh"
    cmp "$repository/pxe/common_files/forge-scout-network.sh" \
        "$root/opt/forge/forge-scout-network.sh"

    for unit in systemd-network-generator.service systemd-networkd.service; do
        [[ "$(systemctl --root="$root" is-enabled "$unit")" == enabled ]]
    done

    generator="$(systemd-analyze --root="$root" cat-config systemd/system/systemd-network-generator.service)"
    networkd="$(systemd-analyze --root="$root" cat-config systemd/system/systemd-networkd.service)"
    grep -Fx 'ExecStartPost=/opt/forge/forge-scout-network.sh prepare' <<< "$generator"
    # Early recovery reloads through PID1 before the system bus is available.
    grep -Fx 'Type=notify-reload' <<< "$networkd"
    for before in 'network-pre\.target' 'systemd-udevd\.service'; do
        grep -E "^Before=(.* )?$before( |$)" <<< "$generator"
        grep -E "^After=(.* )?$before( |$)" <<< "$networkd"
    done

    # Check the installed unit graph, including drop-ins, for ordering cycles.
    # These images omit man pages, so only documentation checks are disabled.
    systemd-analyze --root="$root" --man=no verify \
        systemd-network-generator.service systemd-networkd.service
done

cmp "$repository/pxe/common_files/scout-loader-rclocal" "$loader/etc/rc.local"
root="$scratch/scout"
for unit in systemd-networkd-wait-online.service forge-scout.service; do
    [[ "$(systemctl --root="$root" is-enabled "$unit")" == enabled ]]
done

wait_online="$(systemd-analyze --root="$root" cat-config systemd/system/systemd-networkd-wait-online.service)"
scout="$(systemd-analyze --root="$root" cat-config systemd/system/forge-scout.service)"
grep -Fx 'ExecStart=' <<< "$wait_online"
grep -Fx 'ExecStart=/opt/forge/forge-scout-network.sh wait-online' <<< "$wait_online"
grep -Fx 'TimeoutStartSec=370s' <<< "$wait_online"
grep -E '^After=(.* )?systemd-networkd\.service( |$)' <<< "$wait_online"
grep -E '^Before=(.* )?network-online\.target( |$)' <<< "$wait_online"
grep -E '^After=(.* )?network-online\.target( |$)' <<< "$scout"
grep -E '^Wants=(.* )?network-online\.target( |$)' <<< "$scout"
grep -Fx 'TimeoutStartSec=360s' <<< "$scout"
systemd-analyze --root="$root" --man=no verify \
    systemd-networkd-wait-online.service forge-scout.service

grep -Fx 'network: {config: disabled}' "$root/etc/cloud/cloud.cfg.d/99-nico-scout.cfg"
echo 'Scout loader and full image networking checks passed.'
