#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
# Restore the privately imported tunnel on every diskless boot.
set -eu
umask 077
mkdir -p /etc/wireguard
cp /mnt/state/route64.conf /etc/wireguard/route64.conf
chmod 600 /etc/wireguard/route64.conf
modprobe wireguard
wg-quick up route64
# Table=off keeps wg-quick from creating policy rules or changing IPv4 routes.
ip -6 route replace default dev route64 metric 1
