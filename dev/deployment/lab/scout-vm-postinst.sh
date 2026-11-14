#!/usr/bin/env bash
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -euo pipefail
[[ "$1" == final ]] || exit 0
export DEBIAN_FRONTEND=noninteractive
dpkg -i /build-output/forge-scout.deb
printf 'DAEMON_ARGS="-M 1"\n' > /etc/default/lldpd
systemctl enable forge-scout lldpd
for module in power_state_change resizefs; do
    sed -i "/^[[:space:]]*-[[:space:]]*${module}[[:space:]]*$/d" /etc/cloud/cloud.cfg
    if grep -q "${module}" /etc/cloud/cloud.cfg; then
        echo "ERROR: cloud-init module ${module} is still enabled" >&2
        exit 1
    fi
done
mkdir -p /etc/cloud/cloud.cfg.d
cat > /etc/cloud/cloud.cfg.d/99-nico-scout.cfg <<'CLOUD_CFG'
ssh_quiet_keygen: true
# Scout's networkd configuration manages its interfaces.
network: {config: disabled}
CLOUD_CFG
printf '%s\n' "$PKG_VERSION" > /etc/scout-image-version
test -x /opt/forge/mlx-firmware-supervisor
scout_deps="$(ldd /opt/forge/forge-scout /opt/forge/mlx-firmware-supervisor)"
if grep -q 'not found' <<<"$scout_deps"; then
    echo 'Scout runtime dependencies are missing' >&2
    exit 1
fi
