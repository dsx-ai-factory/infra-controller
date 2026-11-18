# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Dual-stack DNS fixture for peer.mat.test; forwards other names to an explicit VM resolver."""

import ipaddress
import json
import socket
import struct
import sys

bind, peer4, peer6, resolver = sys.argv[1:]
upstream_ip = ipaddress.ip_address(resolver)
sock = socket.socket(
    socket.AF_INET6 if ipaddress.ip_address(bind).version == 6 else socket.AF_INET,
    socket.SOCK_DGRAM,
)
sock.bind((bind, 53))
while True:
    data, client = sock.recvfrom(65535)
    try:
        if len(data) < 12 or struct.unpack("!H", data[4:6])[0] != 1:
            continue
        pos = 12
        labels = []
        while data[pos]:
            size = data[pos]
            if size > 63:
                raise ValueError("compressed or invalid question")
            labels.append(data[pos + 1 : pos + 1 + size].decode("ascii"))
            pos += size + 1
        qtype, qclass = struct.unpack("!HH", data[pos + 1 : pos + 5])
        name = ".".join(labels).lower()
        if name == "peer.mat.test":
            answer = b""
            if qtype in (1, 28) and qclass == 1:
                address = ipaddress.ip_address(peer4 if qtype == 1 else peer6).packed
                answer = (
                    b"\xc0\x0c"
                    + struct.pack("!HHIH", qtype, 1, 0, len(address))
                    + address
                )
            response = (
                data[:2]
                + struct.pack("!HHHHH", 0x8180, 1, int(bool(answer)), 0, 0)
                + data[12 : pos + 5]
                + answer
            )
        else:
            family = socket.AF_INET6 if upstream_ip.version == 6 else socket.AF_INET
            with socket.socket(family, socket.SOCK_DGRAM) as upstream:
                upstream.settimeout(3)
                upstream.connect((resolver, 53))
                upstream.send(data)
                response = upstream.recv(65535)
        sock.sendto(response, client)
        print(
            json.dumps(
                {
                    "client": client[0],
                    "name": name,
                    "qtype": qtype,
                    "rcode": response[3] & 15,
                }
            ),
            flush=True,
        )
    except (ValueError, IndexError, struct.error, OSError) as error:
        print(json.dumps({"client": client[0], "error": str(error)}), flush=True)
