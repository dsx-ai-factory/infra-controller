# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import ipaddress
import json
import os
import socket
import struct
import subprocess
import sys

iface, expected = sys.argv[1:]


def opt(code, data):
    return struct.pack("!HH", code, len(data)) + data


def opts(data):
    result = {}
    while data:
        if len(data) < 4:
            raise ValueError("truncated option")
        code, size = struct.unpack("!HH", data[:4])
        if len(data) < 4 + size:
            raise ValueError("truncated option value")
        result[code] = data[4 : 4 + size]
        data = data[4 + size :]
    return result


idx = socket.if_nametoindex(iface)
mac = open("/sys/class/net/" + iface + "/address").read().strip()
duid = struct.pack("!HH", 3, 1) + bytes.fromhex(mac.replace(":", ""))
ia = struct.pack("!III", 1, 0, 0)
sock = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, iface.encode() + b"\0")
sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_IF, idx)
sock.bind(("::", 546))
sock.settimeout(4)


def exchange(kind, payload, expected_kind):
    xid = os.urandom(3)
    packet = bytes([kind]) + xid + payload
    for attempt in range(4):
        sock.sendto(packet, ("ff02::1:2", 547, 0, idx))
        try:
            while True:
                response, peer = sock.recvfrom(8192)
                if response[0] == expected_kind and response[1:4] == xid:
                    return opts(response[4:]), peer
        except socket.timeout:
            pass
    raise TimeoutError("DHCPv6 response absent")


adv, peer = exchange(1, opt(1, duid) + opt(3, ia) + opt(6, struct.pack("!H", 23)), 2)
assert adv[1] == duid
reply, peer = exchange(
    3, opt(1, duid) + opt(2, adv[2]) + opt(3, adv[3]) + opt(6, struct.pack("!H", 23)), 7
)
assert reply[1] == duid and reply[2] == adv[2]
iaopts = opts(reply[3][12:])
address = str(ipaddress.IPv6Address(iaopts[5][:16]))
preferred, valid = struct.unpack("!II", iaopts[5][16:24])
assert address == expected and valid >= preferred > 0
assert 23 in reply and len(reply[23]) % 16 == 0
dns = [
    str(ipaddress.IPv6Address(reply[23][i : i + 16]))
    for i in range(0, len(reply[23]), 16)
]
subprocess.run(
    [
        "ip",
        "-6",
        "addr",
        "add",
        address + "/128",
        "dev",
        iface,
        "nodad",
        "valid_lft",
        str(valid),
        "preferred_lft",
        str(preferred),
    ],
    check=True,
)
print(
    json.dumps(
        {
            "dns_servers": dns,
            "exchange": "Solicit/Advertise/Request/Reply",
            "address": address,
            "server": peer[0],
            "preferred_lifetime": preferred,
            "valid_lifetime": valid,
        }
    )
)
