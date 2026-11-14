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
# Install inbound policy before exposing the tunnel's global address. Keep
# replies and related ICMPv6 errors (including packet-too-big) usable.
ip6tables -N VFLAB-TUNNEL 2>/dev/null || true
ip6tables -C VFLAB-TUNNEL -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT 2>/dev/null \
  || ip6tables -I VFLAB-TUNNEL -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
ip6tables -C VFLAB-TUNNEL -j DROP 2>/dev/null || ip6tables -A VFLAB-TUNNEL -j DROP
ip6tables -C INPUT -i route64 -j VFLAB-TUNNEL 2>/dev/null \
  || ip6tables -I INPUT -i route64 -j VFLAB-TUNNEL
wg-quick up route64
# Table=off keeps wg-quick from creating policy rules or changing IPv4 routes.
ip -6 route replace default dev route64 metric 1
