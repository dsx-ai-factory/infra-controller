# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

curl -X POST "https://api.example.com/v2/org/{tenant-org-name}/nico/operating-system" \
  -H "Content-Type: application/json" -H "Accept: application/json" \
  -H "Authorization: Bearer ${TOKEN}" \
  -d '{
        "name": "demo-ipxe-os",
        "description": "Demo tenant operating system",
        "tenantId": "7306ff7d-f2b4-472f-ba1c-3ec9c24967be",
        "allowOverride": true,
        "isCloudInit": true,
        "ipxeScript": "#!ipxe\nkernel https://github.com/netbootxyz/ubuntu-squash/releases/download/20.04.6-a1b16d57/vmlinuz ip=dhcp fb=false interface=ens5f0 url=https://releases.ubuntu.com/20.04.6/ubuntu-20.04.6-live-server-amd64.iso autoinstall ds=nocloud-net;s=${tenant-cloudinit-url} initrd=initrd.magic\ninitrd https://github.com/netbootxyz/ubuntu-squash/releases/download/20.04.6-a1b16d57/initrd\nboot\n",
        "userData": "#cloud-config\nusers:\n  - default\n  - name: demo-user\n    gecos: Demo User\n    sudo: ALL=(ALL) NOPASSWD:ALL\n    groups: root\n    lock_passwd: true\n    ssh_authorized_keys:\n      - ssh-ed25519 ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAcV/3oxRllEji0wl9F6icRk+Kme0H2MMAPFizKB5yv8 demo@example.com\n\nautoinstall:\n  version: 1\n\n  identity:\n    hostname: demo-host\n    password: $6$jCfWFbdxh1lK09sY$pxFnrW/yXewYFmgoaywu3WKhdPQg0e8DR8jvedAV.udXM0.i5M6wr4Up2S7ZCN9kNDmg.s7fmrOaXE6nEyzPb/ # Welcome123\n    username: ubuntu\n\n  ntp:\n    enabled: true\n    ntp_client: chrony\n    servers:\n      - 129.6.15.32\n\n  keyboard:\n    layout: us\n    toggle: null\n    variant: \"\"\n  locale: en_US\n  ssh:\n    allow-pw: true\n    authorized-keys: []\n    install-server: true\n"
      }'
