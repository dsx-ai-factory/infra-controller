#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
# networking-gateway.sh <lab_num> <hostname> <ipv4_gw> <ipv6_gw> <wan_mac> <lan_mac>
#
# Step 1 of gateway configuration (idempotent, SSH-safe):
# - hostname
# - /etc/network/interfaces: WAN (DHCP v4+v6) and LAN (static v4+v6),
#   interfaces matched by MAC so it does not depend on device order
# - forwarding / RA sysctls, resolv.conf
# - brings up the LAN and enables WAN IPv6 without bouncing IPv4.
set -e

if [ $# -ne 6 ]; then
  echo >&2 "# usage: networking-gateway.sh <lab_num> <hostname> <ipv4_gw> <ipv6_gw> <wan_mac> <lan_mac>"
  exit 1
fi

LAB_NUM="$1"
HOSTNAME="$2"
IPV4_GW="$3"
IPV6_GW="$4"
WAN_MAC="$5"
LAN_MAC="$6"
LAN_MODE="${VXLAB_LAN_MODE:-dual}"
case "$LAN_MODE" in dual|ipv4|ipv6) ;; *) echo >&2 'invalid LAN mode'; exit 1;; esac
IPV4_FORWARD=1
[ "$LAN_MODE" != ipv6 ] || IPV4_FORWARD=0
IPV6_FORWARD=1
LAN_DISABLE_IPV6=0
if [ "$LAN_MODE" = ipv4 ]; then
  IPV6_FORWARD=0
  LAN_DISABLE_IPV6=1
fi

echo "${HOSTNAME}" > /etc/hostname
hostname "${HOSTNAME}"

# Map MACs to the kernel interface names (robust against device ordering).
ifname_for_mac() {
  for d in /sys/class/net/*; do
    d="${d##*/}"
    [ "$d" = "lo" ] && continue
    if [ "$(cat "/sys/class/net/${d}/address" 2>/dev/null)" = "$(echo "$1" | tr 'A-F' 'a-f')" ]; then
      echo "$d"
      return 0
    fi
  done
  return 1
}

WAN_IF="$(ifname_for_mac "${WAN_MAC}")" || { echo >&2 "# WAN interface (mac ${WAN_MAC}) not found"; exit 1; }
LAN_IF="$(ifname_for_mac "${LAN_MAC}")" || { echo >&2 "# LAN interface (mac ${LAN_MAC}) not found"; exit 1; }
echo >&2 "# using WAN_IF=${WAN_IF} LAN_IF=${LAN_IF}"

if [ "${VXLAB_UPLINK:-bridged}" = nat ]; then
  # Apple's NAT/VPN path can stall large packets. Keep the WAN MTU stable
  # across both DHCP renewals and router advertisements.
  ip link set dev "$WAN_IF" mtu 1280
  printf 'interface %s\n  nooption interface_mtu\n' "$WAN_IF" >> /etc/dhcpcd.conf
  sysctl -w "net.ipv6.conf.${WAN_IF}.accept_ra_mtu=0"
fi

mkdir -p /etc/vxlab
printf '%s\n' "${WAN_IF}" > /etc/vxlab/wan_if
printf '%s\n' "${LAN_IF}" > /etc/vxlab/lan_if
printf '%s\n' "$LAN_MODE" > /etc/vxlab/lan_mode
cat > /etc/dhcpcd.exit-hook <<'HOOK'
case "$reason" in
  BOUND|RENEW|REBIND|REBOOT)
    printf '%s: leased %s for DHCP\n' "$interface" "$new_ip_address" > /dev/hvc0;;
esac
HOOK

cat > /etc/network/interfaces <<EOF
auto lo
iface lo inet loopback

# WAN: hypervisor (vfkit NAT or vmnet shared). DHCP for v4 and v6.
auto ${WAN_IF}
iface ${WAN_IF} inet dhcp
  dhcp-opts -b

# LAN: the isolated lab segment we serve (v4 + v6, static).
auto ${LAN_IF}
iface ${LAN_IF} inet static
EOF
if [ "$LAN_MODE" != ipv4 ]; then
  printf '  address %s/64\n' "$IPV6_GW" >> /etc/network/interfaces
fi
if [ "$LAN_MODE" != ipv6 ]; then
  printf '  address %s/24\n' "$IPV4_GW" >> /etc/network/interfaces
else
  ip -4 addr flush dev "$LAN_IF"
fi

mkdir -p /etc/sysctl.d
# v4/v6 forwarding: we are the router of the lab segment.
echo "net.ipv4.conf.all.forwarding=$IPV4_FORWARD" > /etc/sysctl.d/20-enable-ipv4-routing.conf
echo "net.ipv6.conf.all.forwarding=$IPV6_FORWARD" > /etc/sysctl.d/20-enable-ipv6-routing.conf
echo "net.ipv6.conf.${LAN_IF}.disable_ipv6=$LAN_DISABLE_IPV6" > /etc/sysctl.d/20-lan-ipv6.conf
# WAN: accept router advertisements even with forwarding enabled, so we
# learn the hypervisor's IPv6 default route (needed for IPv6 NAT).
# Value 2 is required when global IPv6 forwarding is enabled; value 1 only
# accepts RAs while the interface is acting as a host.
echo "net.ipv6.conf.${WAN_IF}.accept_ra=2" > /etc/sysctl.d/20-wan-accept-ra.conf
sysctl -w "net.ipv4.conf.all.forwarding=$IPV4_FORWARD"
sysctl -w "net.ipv6.conf.all.forwarding=$IPV6_FORWARD"
sysctl -w "net.ipv6.conf.${LAN_IF}.disable_ipv6=$LAN_DISABLE_IPV6"
sysctl -w "net.ipv6.conf.${WAN_IF}.accept_ra=2"

# bring the LAN up (new interface: no risk to the SSH session)
ifdown "${LAN_IF}" 2>/dev/null || true
ifup "${LAN_IF}"

echo "# networking-gateway done (lab ${LAB_NUM})."
