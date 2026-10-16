# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Test RA sender only; does not run production FRR/HBN."""

import ipaddress
import pathlib
import socket
import struct
import sys
import time

iface, prefix, control = sys.argv[1:]
idx = socket.if_nametoindex(iface)
s = socket.socket(socket.AF_INET6, socket.SOCK_RAW, socket.IPPROTO_ICMPV6)
s.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, iface.encode() + b"\0")
s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_IF, idx)
s.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_HOPS, 255)
s.bind(("fe80::1", 0, 0, idx))
p = ipaddress.IPv6Network(prefix)
mac = bytes.fromhex(
    pathlib.Path("/sys/class/net/" + iface + "/address")
    .read_text()
    .strip()
    .replace(":", "")
)
while True:
    lifetime = int(pathlib.Path(control).read_text())
    header = struct.pack("!BBHBBHII", 134, 0, 0, 64, 0xC0, lifetime, 0, 0)
    pio = struct.pack(
        "!BBBBIII16s", 3, 4, p.prefixlen, 0x80, 7200, 3600, 0, p.network_address.packed
    )
    mtu = struct.pack("!BBHI", 5, 1, 0, 1280)
    s.sendto(header + pio + mtu + bytes([1, 1]) + mac, ("ff02::1", 0, 0, idx))
    print("RA", iface, prefix, "M=1 O=1 A=0", "lifetime", lifetime, flush=True)
    time.sleep(2)
