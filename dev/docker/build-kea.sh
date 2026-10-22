#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -eu

# Kea 2.2 must not return a collection containing a null lease when SELECT
# refuses an expired candidate. Build the pinned Debian source with that fix.
kea_version="$1"
test "$kea_version" = '2.2.0-6'
sed -i 's/Types: deb$/Types: deb deb-src/' /etc/apt/sources.list.d/debian.sources
apt-get update
apt-get install -y --no-install-recommends dpkg-dev ca-certificates
apt-get build-dep -y "isc-kea=$kea_version"
mkdir -p /build /kea-packages
cd /build
apt-get source "isc-kea=$kea_version"
cd isc-kea-2.2.0
patch -p1 < /tmp/kea-build/kea-reuse-null.patch
DEB_BUILD_OPTIONS="nocheck parallel=8" dpkg-buildpackage -b -uc -us
cp ../kea-common_*.deb /kea-packages/kea-common.deb
# Debian's development package omits this header required by LeaseMgrFactory.
cp src/lib/dhcpsrv/cfg_globals.h /kea-packages/cfg_globals.h
