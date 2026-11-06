#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -eu

. /etc/os-release
case "${ID}" in
    debian|ubuntu) ;;
    *) echo "Unsupported Kea package distribution: ${ID}" >&2; exit 1 ;;
esac

apt-get update
apt-get install -y --no-install-recommends ca-certificates
rm -rf /var/lib/apt/lists/*

# ISC's public Kea 3.0 LTS repository signing key:
# 9DA570BB192211885E4EB280B16C44CD45514C3C
install -m 0644 "$(dirname "$0")/isc-kea-3-0.asc" /usr/share/keyrings/isc-kea-3-0.asc
printf 'deb [signed-by=/usr/share/keyrings/isc-kea-3-0.asc] https://dl.cloudsmith.io/public/isc/kea-3-0/deb/%s %s main\n' \
    "${ID}" "${VERSION_CODENAME}" >/etc/apt/sources.list.d/isc-kea-3-0.list
