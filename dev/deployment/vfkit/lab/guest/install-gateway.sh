#!/bin/sh
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
# install-gateway.sh <lab_num> <hostname> <ipv4_gw> <ipv6_gw>
#                    <pool4_start> <pool4_end> <pool6_start> <pool6_end>
#                    <wan_mac> <lan_mac>
#
# Step 2 of gateway configuration (idempotent): dnsmasq (DHCPv4+DHCPv6+DNS),
# radvd (RA), NAT for v4 and v6, persisted for reboots.
#
# Interfaces are taken from /etc/vxlab (written by networking-gateway.sh) and
# cross-checked by MAC, so nothing depends on virtio device ordering.
set -e

if [ $# -ne 10 ]; then
  echo >&2 "# usage: install-gateway.sh <lab_num> <hostname> <ipv4_gw> <ipv6_gw> <pool4_start> <pool4_end> <pool6_start> <pool6_end> <wan_mac> <lan_mac>"
  exit 1
fi

LAB_NUM="$1"
HOSTNAME_="$2"
IPV4_GW="$3"
IPV6_GW="$4"
POOL4_START="$5"
POOL4_END="$6"
POOL6_START="$7"
POOL6_END="$8"
WAN_MAC="${9}"
LAN_MAC="${10}"
LAN_MODE="${VXLAB_LAN_MODE:-dual}"
LAN_MTU=1500
[ "${VXLAB_UPLINK:-bridged}" != nat ] || LAN_MTU=1280
case "$LAN_MODE" in dual|ipv4|ipv6) ;; *) echo >&2 'invalid LAN mode'; exit 1;; esac

# the /24 for this lab, derived from the gateway address (10.X.0.1 -> 10.X.0.0)
NET4="$(echo "${IPV4_GW}" | sed 's/\.[0-9]*$/\.0/')"
# the ULA prefix for this lab, derived from fd00:X::1
case "${IPV6_GW}" in
  *::1) IPV6_PREFIX="${IPV6_GW%::1}::/64" ;;
  *) echo >&2 "# expected an IPv6 gateway ending in ::1, got ${IPV6_GW}"; exit 1 ;;
esac

if [ "$(hostname)" != "${HOSTNAME_}" ]; then
  echo "${HOSTNAME_}" > /etc/hostname
  hostname "${HOSTNAME_}"
fi

WAN_IF="$(cat /etc/vxlab/wan_if 2>/dev/null || echo eth0)"
LAN_IF="$(cat /etc/vxlab/lan_if 2>/dev/null || echo eth1)"

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
if [ -n "$(ifname_for_mac "${WAN_MAC}" 2>/dev/null)" ]; then
  WAN_IF="$(ifname_for_mac "${WAN_MAC}")"
fi
if [ -n "$(ifname_for_mac "${LAN_MAC}" 2>/dev/null)" ]; then
  LAN_IF="$(ifname_for_mac "${LAN_MAC}")"
fi
echo >&2 "# using WAN_IF=${WAN_IF} LAN_IF=${LAN_IF}"

apk info -e dhcpcd dhcpcd-openrc dnsmasq dnsmasq-openrc radvd radvd-openrc ndisc6 iptables ip6tables curl iproute2 acpid acpid-openrc >/dev/null
rc-update add acpid default >/dev/null
rc-service acpid start >/dev/null
# Alpine disables TCP forwarding by default. The host reaches isolated clients
# using SSH's direct-tcpip channel through this gateway.
sed -i 's/^[# ]*AllowTcpForwarding .*/AllowTcpForwarding local/' /etc/ssh/sshd_config
sshd -t
rc-service sshd reload >/dev/null

# --------------------------------------------------------------- dnsmasq
mkdir -p /etc/dnsmasq.d
cat > /etc/dnsmasq.d/lab.conf <<EOF
# Lab ${LAB_NUM} segment.
interface=${LAN_IF}
bind-interfaces
port=53
dhcp-authoritative
log-dhcp
EOF
if [ "$LAN_MODE" != ipv6 ]; then
  cat >> /etc/dnsmasq.d/lab.conf <<EOF
# IPv4 pool (the gateway owns ${IPV4_GW})
dhcp-range=${POOL4_START},${POOL4_END},24h
dhcp-option=option:router,${IPV4_GW}
dhcp-option=option:dns-server,${IPV4_GW}
dhcp-option=option:mtu,${LAN_MTU}
EOF
fi
if [ "$LAN_MODE" != ipv4 ]; then
cat >> /etc/dnsmasq.d/lab.conf <<EOF
# IPv6: stateful DHCPv6; radvd advertises the managed-address flag.
dhcp-range=${POOL6_START},${POOL6_END},64,12h
dhcp-option=option6:dns-server,[${IPV6_GW}]
EOF
fi

rc-update add dnsmasq default >/dev/null
rc-service dnsmasq restart

# --------------------------------------------------------------- radvd
if [ "$LAN_MODE" != ipv4 ]; then
cat > /etc/radvd.conf <<EOF
# Router advertisements for lab ${LAB_NUM}.
interface ${LAN_IF} {
    AdvSendAdvert on;
    AdvLinkMTU ${LAN_MTU};
    AdvManagedFlag on;
    AdvOtherConfigFlag on;
    MaxRtrAdvInterval 120;
    MinRtrAdvInterval 15;
    AdvDefaultLifetime 600;
    prefix ${IPV6_PREFIX} {
        # we assign addresses via DHCPv6, not SLAAC
        AdvOnLink on;
        AdvAutonomous off;
        AdvValidLifetime 21600;
        AdvPreferredLifetime 10800;
    };
    RDNSS ${IPV6_GW} {
        AdvRDNSSLifetime 600;
    };
};
EOF

rc-update add radvd default >/dev/null
rc-service radvd restart
rc-update add vflab-ipv6-router default >/dev/null
rc-service vflab-ipv6-router restart
else
  rc-service radvd stop 2>/dev/null || true
  rc-update del radvd default 2>/dev/null || true
  rc-service vflab-ipv6-router stop 2>/dev/null || true
  rc-update del vflab-ipv6-router default 2>/dev/null || true
fi

# ------------------------------------------------------------------ NAT
# Install the rules now and reapply them through OpenRC after guest reboot.
mkdir -p /etc/local.d
cat > /etc/local.d/nat.start <<EOF
#!/bin/sh
write_nat() {
  if [ "$LAN_MODE" != ipv6 ]; then
  iptables -t nat -C POSTROUTING -s "${NET4}/24" -o "${WAN_IF}" -j MASQUERADE 2>/dev/null \\
    || iptables -t nat -A POSTROUTING -s "${NET4}/24" -o "${WAN_IF}" -j MASQUERADE
  fi
  if [ "$LAN_MODE" != ipv4 ]; then
  ip6tables -t nat -C POSTROUTING -s "${IPV6_PREFIX}" -o "${WAN_IF}" -j MASQUERADE 2>/dev/null \\
    || ip6tables -t nat -A POSTROUTING -s "${IPV6_PREFIX}" -o "${WAN_IF}" -j MASQUERADE
  fi
}
write_nat
EOF
# Only lab-originated traffic may start forwarding flows. Blocking lab address
# ranges on WAN prevents routing from one lab through the host into another.
cat >> /etc/local.d/nat.start <<EOF
for tool in iptables ip6tables; do
  \$tool -N VFLAB-FORWARD 2>/dev/null || true
  \$tool -F VFLAB-FORWARD
  \$tool -C FORWARD -j VFLAB-FORWARD 2>/dev/null || \$tool -I FORWARD -j VFLAB-FORWARD
  \$tool -A VFLAB-FORWARD -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
done
iptables -A VFLAB-FORWARD -i ${LAN_IF} -d 10.0.0.0/8 -j REJECT
ip6tables -A VFLAB-FORWARD -i ${LAN_IF} -d fd00::/8 -j REJECT
[ "$LAN_MODE" = ipv6 ] || iptables -A VFLAB-FORWARD -i ${LAN_IF} -o ${WAN_IF} -s ${NET4}/24 -j ACCEPT
[ "$LAN_MODE" = ipv4 ] || ip6tables -A VFLAB-FORWARD -i ${LAN_IF} -o ${WAN_IF} -s ${IPV6_PREFIX} -j ACCEPT
iptables -A VFLAB-FORWARD -j DROP
ip6tables -A VFLAB-FORWARD -j DROP
EOF
chmod +x /etc/local.d/nat.start
/etc/local.d/nat.start
rc-update add local default >/dev/null

echo "# install-gateway done: v4=${NET4}/24 pool=${POOL4_START}-${POOL4_END} v6=${IPV6_PREFIX} pool=${POOL6_START}-${POOL6_END}"
