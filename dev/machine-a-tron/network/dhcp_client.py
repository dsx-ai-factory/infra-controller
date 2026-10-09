# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import ipaddress
import json
import os
import socket
import struct
import subprocess
import sys
import time

IAID = 1

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


def checked_ia(data):
    if len(data) < 12 or struct.unpack("!I", data[:4])[0] != IAID:
        raise RuntimeError("DHCPv6 IA_NA identity mismatch")
    return data


idx = socket.if_nametoindex(iface)
mac = open("/sys/class/net/" + iface + "/address").read().strip()
duid = struct.pack("!HH", 3, 1) + bytes.fromhex(mac.replace(":", ""))
ia = struct.pack("!III", IAID, 0, 0)
sock = socket.socket(socket.AF_INET6, socket.SOCK_DGRAM)
sock.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, iface.encode() + b"\0")
sock.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_MULTICAST_IF, idx)
sock.bind(("::", 546))


def exchange(kind, payload, expected_kind):
    xid = os.urandom(3)
    packet = bytes([kind]) + xid + payload
    for attempt in range(4):
        sock.sendto(packet, ("ff02::1:2", 547, 0, idx))
        deadline = time.monotonic() + 4
        while (remaining := deadline - time.monotonic()) > 0:
            sock.settimeout(remaining)
            try:
                response, peer = sock.recvfrom(8192)
            except socket.timeout:
                break
            if (
                len(response) >= 4
                and response[0] == expected_kind
                and response[1:4] == xid
            ):
                return opts(response[4:]), peer
    raise TimeoutError("DHCPv6 response absent")


adv, peer = exchange(1, opt(1, duid) + opt(3, ia) + opt(6, struct.pack("!H", 23)), 2)
if adv.get(1) != duid:
    raise RuntimeError("Advertise client ID mismatch")
advertised_ia = checked_ia(adv.get(3, b""))
reply, peer = exchange(
    3,
    opt(1, duid)
    + opt(2, adv[2])
    + opt(3, advertised_ia)
    + opt(6, struct.pack("!H", 23)),
    7,
)
if reply.get(1) != duid or reply.get(2) != adv[2]:
    raise RuntimeError("Reply identity mismatch")
iaopts = opts(checked_ia(reply.get(3, b""))[12:])
address = str(ipaddress.IPv6Address(iaopts[5][:16]))
preferred, valid = struct.unpack("!II", iaopts[5][16:24])
if address != expected or not valid >= preferred > 0:
    raise RuntimeError("unexpected DHCPv6 lease")
if 23 not in reply or len(reply[23]) % 16:
    raise RuntimeError("missing or malformed DNS option")
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
