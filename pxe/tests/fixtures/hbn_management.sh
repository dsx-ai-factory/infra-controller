# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

# Representative HBN 3.2.3 input, not a copy of the upstream manifest.
# Keep the patch anchors and family checks: HBN requires an address, prefix
# length, and gateway before writing either family's configuration.
      function setup_eth0_in_mgmt_vrf() {
          # Read CNI file
          CNI_FILE=/etc/cni/net.d/10-containerd-net-br-mgmt.conflist
          MASKLEN_IPV4=16
          MASKLEN_IPV6=64
          IP4=192.0.2.10/16
          IP6=2001:db8::10/64
          GATEWAYS="192.0.2.1 2001:db8::1"
          GW_IP4=$(echo $GATEWAYS | awk '{print $1}')
          GW_IP6=$(echo $GATEWAYS | awk '{print $2}')

          mkdir -p /host/var/lib/hbn/etc/network/interfaces.d/
          ENIM=/host/var/lib/hbn/etc/network/interfaces.d/mgmt.intf
          echo auto eth0 > $ENIM
          echo iface eth0 inet static >> $ENIM
          HAVE_V4=0
          HAVE_V6=0
          [ -n "$IP4" ] && [ -n "$MASKLEN_IPV4" ] && [ -n "$GW_IP4" ] && HAVE_V4=1
          [ -n "$IP6" ] && [ -n "$MASKLEN_IPV6" ] && [ -n "$GW_IP6" ] && HAVE_V6=1
          if [ "$HAVE_V4" -eq 1 ]; then
              echo "    address $IP4" >> $ENIM
          fi
          if [ "$HAVE_V6" -eq 1 ]; then
              echo "    address $IP6" >> $ENIM
          fi
          if [ "$HAVE_V4" -eq 1 ]; then
              echo "    gateway $GW_IP4" >> $ENIM
          fi
          if [ "$HAVE_V6" -eq 1 ]; then
              echo "    gateway $GW_IP6" >> $ENIM
          fi
          echo "    vrf mgmt" >> $ENIM
      }

      setup_eth0_in_mgmt_vrf

      sysctl net.ipv4.tcp_l3mdev_accept=1

      function check_sfc_journal() {
          journalctl -u sfc.service --no-pager
      }
