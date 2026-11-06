#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

set -eu

# Keep packaged public hooks available through the architecture-independent path.
install -d -m 0755 /usr/lib/kea/hooks
for library in /usr/lib/*-linux-gnu/kea/hooks/*.so; do
    test -f "${library}"
    ln -sf "${library}" /usr/lib/kea/hooks/
done

# NICo's containers run Kea as root. ISC's _kea-owned directories otherwise
# require a DAC override that the host's native AppArmor profile disallows.
# Preserve the package's restricted modes; native host installs retain _kea.
chown root:root /run/kea /var/lib/kea
# The confined daemon must also be able to execute scheduled lease cleanup.
chown root:_kea /usr/sbin/kea-lfc
