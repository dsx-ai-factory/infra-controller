#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
set -eu

# Rebuild the allocation library from the exact ISC release used by the binary
# packages. Keep the ISC package's other libraries, service files and ABI intact.
kea_version="$1"
test "${kea_version%%-*}" = '3.0.5'
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
sh "$script_dir/setup-kea-repository.sh"
apt-get update
apt-get install -y --no-install-recommends \
    build-essential curl xz-utils patch pkg-config python3-venv ninja-build patchelf \
    libboost-system-dev liblog4cplus-dev libssl-dev libkrb5-dev libpq-dev libmariadb-dev \
    "isc-kea-common=$kea_version" "isc-kea-dev=$kea_version"

build_dir=$(mktemp -d)
trap 'rm -rf "$build_dir"' EXIT HUP INT TERM
package_dir="${KEA_PACKAGE_DIR:-/kea-packages}"
mkdir -p "$package_dir"
cd "$build_dir"
curl --fail --location --retry 3 https://downloads.isc.org/isc/kea/3.0.5/kea-3.0.5.tar.xz -o kea.tar.xz
printf '%s  kea.tar.xz\n' '03ac2443f75b56c8c58144e2929230ce430cf7a2f85f0e2c056fb14933feb5eb' | sha256sum -c -
tar -xJf kea.tar.xz
cd kea-3.0.5
patch --fuzz=0 -p1 < "$script_dir/kea-lease6-replacement.patch"
python3 -m venv "$build_dir/meson"
"$build_dir/meson"/bin/pip install --no-cache-dir meson==1.9.1
multiarch=$(dpkg-architecture -qDEB_HOST_MULTIARCH)
"$build_dir/meson"/bin/meson setup build --prefix=/usr --libdir="lib/$multiarch" \
    --libexecdir=lib/kea --sysconfdir=/etc/kea --localstatedir=/var \
    --sharedstatedir=/var/lib/kea --buildtype=release -Dcpp_std=c++17 \
    -Dnetconf=disabled -Dtests=disabled -Dmysql=enabled -Dpostgresql=enabled -Dkrb5=enabled
library=libkea-dhcpsrv.so.133.0.0
ninja -C build -j "${KEA_BUILD_JOBS:-4}" "src/lib/dhcpsrv/$library"
# An uninstalled Meson library refers to its build tree. Runtime must use only
# the matching ISC package dependencies, including in the compiler image.
patchelf --remove-rpath "build/src/lib/dhcpsrv/$library"

# Fail the build if rebuilding removed an exported strong symbol. The
# handoff contract adds symbols; it removes none from the pinned ISC library.
nm -D --defined-only "/usr/lib/$multiarch/$library" | awk '$2 ~ /^[TDBR]$/ {print $3}' | LC_ALL=C sort > "$build_dir/stock.symbols"
nm -D --defined-only "build/src/lib/dhcpsrv/$library" | awk '$2 ~ /^[TDBR]$/ {print $3}' | LC_ALL=C sort > "$build_dir/patched.symbols"
LC_ALL=C comm -23 "$build_dir/stock.symbols" "$build_dir/patched.symbols" > "$build_dir/missing.symbols"
test ! -s "$build_dir/missing.symbols" || { cat "$build_dir/missing.symbols" >&2; exit 1; }

cd "$build_dir"
apt-get download "isc-kea-common=$kea_version"
dpkg-deb --raw-extract ./isc-kea-common_*.deb package
cp "kea-3.0.5/build/src/lib/dhcpsrv/$library" "package/usr/lib/$multiarch/$library"
# Preserve the package version: all ISC components depend on this exact build.
# The hook checks the native capability, not the version string, at startup.
mkdir -p package/usr/share/doc/isc-kea-common
sha256sum "$script_dir/kea-lease6-replacement.patch" > package/usr/share/doc/isc-kea-common/nico-handoff-patch.sha256
sed -i "\|usr/lib/$multiarch/$library$|d" package/DEBIAN/md5sums
(cd package && md5sum "usr/lib/$multiarch/$library" \
    usr/share/doc/isc-kea-common/nico-handoff-patch.sha256 >> DEBIAN/md5sums)
dpkg-deb --build --root-owner-group package "$package_dir/isc-kea-common.deb"
