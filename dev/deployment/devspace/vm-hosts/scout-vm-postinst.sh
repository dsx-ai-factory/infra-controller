#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
[[ "$1" == final ]] || exit 0
export DEBIAN_FRONTEND=noninteractive
dpkg -i /build-output/forge-scout.deb
systemctl enable forge-scout lldpd
for module in power_state_change resizefs; do
    sed -i "/^[[:space:]]*-[[:space:]]*${module}[[:space:]]*$/d" /etc/cloud/cloud.cfg
done
mkdir -p /etc/cloud/cloud.cfg.d
printf 'ssh_quiet_keygen: true\n' > /etc/cloud/cloud.cfg.d/99-nico-scout.cfg
printf '%s\n' "$PKG_VERSION" > /etc/scout-image-version
if ldd /opt/forge/forge-scout | grep -q 'not found'; then
    echo 'Scout runtime dependencies are missing' >&2
    exit 1
fi
