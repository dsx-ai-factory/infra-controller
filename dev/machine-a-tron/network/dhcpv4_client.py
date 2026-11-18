# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Minimal initial DHCPv4 lease acquisition for the isolated packet-test endpoint."""

import ipaddress
import json
import os
import socket
import struct
import subprocess
import sys
import time

COOKIE = bytes.fromhex("63825363")
# Linux packet(7) / linux/if_packet.h; Python does not expose these constants.
SOL_PACKET = 263
PACKET_AUXDATA = 8
TP_STATUS_CSUMNOTREADY = 1 << 3


def checksum(data):
    data += b"\0" * (len(data) % 2)
    total = sum(int.from_bytes(data[i : i + 2], "big") for i in range(0, len(data), 2))
    while total >> 16:
        total = (total & 0xFFFF) + (total >> 16)
    return (~total) & 0xFFFF


def option(code, value):
    return bytes([code, len(value)]) + value


def options(data):
    result = {}
    while data:
        code, data = data[0], data[1:]
        if code == 255:
            return result
        if code == 0:
            continue
        if not data or len(data) < data[0] + 1:
            raise RuntimeError("truncated DHCPv4 option")
        size = data[0]
        if code in result:
            raise RuntimeError("duplicate DHCPv4 option")
        result[code], data = data[1 : size + 1], data[size + 1 :]
    raise RuntimeError("missing DHCPv4 end option")


def acquire(iface, expected, prefix, gateway, dns):
    with open("/sys/class/net/" + iface + "/address") as stream:
        mac = bytes.fromhex(stream.read().strip().replace(":", ""))
    xid = os.urandom(4)
    header = bytearray(236)
    header[:3] = bytes([1, 1, 6])
    header[4:8] = xid
    header[10:12] = struct.pack("!H", 0x8000)
    header[28:34] = mac
    sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    receiver = None
    try:
        # Receive before IPv4 is configured: the IP stack may discard broadcast
        # replies on an addressless interface. Packet sockets bypass that check.
        receiver = socket.socket(
            socket.AF_PACKET, socket.SOCK_DGRAM, socket.htons(0x0800)
        )
        receiver.setsockopt(SOL_PACKET, PACKET_AUXDATA, 1)
        receiver.bind((iface, 0))
        sock.setsockopt(
            socket.SOL_SOCKET, socket.SO_BINDTODEVICE, iface.encode() + b"\0"
        )
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        sock.bind(("0.0.0.0", 68))

        def exchange(kind, extra, expected_kind):
            packet = bytes(header) + COOKIE + option(53, bytes([kind]))
            packet += (
                option(61, b"\x01" + mac)
                + option(55, bytes([1, 3, 6, 51, 54]))
                + extra
                + b"\xff"
            )
            for _ in range(4):
                sock.sendto(packet, ("255.255.255.255", 67))
                deadline = time.monotonic() + 4
                while time.monotonic() < deadline:
                    receiver.settimeout(max(0.001, deadline - time.monotonic()))
                    try:
                        received, ancillary, flags, _ = receiver.recvmsg(
                            8192, socket.CMSG_SPACE(20)
                        )
                        if flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC):
                            raise RuntimeError(
                                "truncated DHCPv4 packet or checksum metadata"
                            )
                        if (
                            len(received) < 28
                            or received[0] >> 4 != 4
                            or received[9] != 17
                        ):
                            continue
                        ihl = (received[0] & 15) * 4
                        total = struct.unpack("!H", received[2:4])[0]
                        if (
                            ihl < 20
                            or total > len(received)
                            or total < ihl + 8
                            or struct.unpack("!H", received[6:8])[0] & 0x3FFF
                        ):
                            continue
                        source, destination, size = struct.unpack(
                            "!HHH", received[ihl : ihl + 6]
                        )
                        if (
                            (source, destination) != (67, 68)
                            or size < 8
                            or ihl + size != total
                        ):
                            continue
                        data = received[ihl + 8 : total]
                    except socket.timeout:
                        break
                    if len(data) < 240 or data[4:8] != xid or data[28:34] != mac:
                        continue
                    if checksum(received[:ihl]):
                        raise RuntimeError("invalid IPv4 header checksum")
                    pending_checksum = any(
                        level == SOL_PACKET
                        and kind == PACKET_AUXDATA
                        and len(value) >= 4
                        and struct.unpack_from("=I", value)[0] & TP_STATUS_CSUMNOTREADY
                        for level, kind, value in ancillary
                    )
                    # Locally offloaded packets can arrive before their UDP checksum
                    # is completed; only kernel metadata can authorize this exception.
                    if received[ihl + 6 : ihl + 8] != b"\0\0" and not pending_checksum:
                        pseudo = (
                            received[12:20] + bytes([0, 17]) + struct.pack("!H", size)
                        )
                        if checksum(pseudo + received[ihl:total]):
                            raise RuntimeError("invalid DHCPv4 UDP checksum")
                    if data[:3] != bytes([2, 1, 6]) or data[236:240] != COOKIE:
                        raise RuntimeError("invalid DHCPv4 response header")
                    parsed = options(data[240:])
                    if parsed.get(53) == b"\x06":
                        raise RuntimeError("DHCPv4 server rejected the request")
                    if parsed.get(53) == bytes([expected_kind]):
                        return str(ipaddress.IPv4Address(data[16:20])), parsed
            raise TimeoutError("DHCPv4 response absent")

        offered, offer = exchange(1, b"", 2)
        server = offer.get(54)
        if offered != expected or server != ipaddress.IPv4Address(gateway).packed:
            raise RuntimeError("unexpected DHCPv4 offer address or server")
        address, reply = exchange(
            3, option(50, ipaddress.IPv4Address(offered).packed) + option(54, server), 5
        )
        network = ipaddress.IPv4Network(prefix)
        required = {
            54: server,
            1: network.netmask.packed,
            3: ipaddress.IPv4Address(gateway).packed,
            6: ipaddress.IPv4Address(dns).packed,
        }
        if address != expected or any(
            reply.get(k) != value for k, value in required.items()
        ):
            raise RuntimeError("DHCPv4 lease, server, subnet, gateway or DNS mismatch")
        if len(reply.get(51, b"")) != 4:
            raise RuntimeError("missing or malformed DHCPv4 lease lifetime")
        lifetime = struct.unpack("!I", reply[51])[0]
        if not lifetime:
            raise RuntimeError("zero DHCPv4 lease lifetime")
        # Install only values validated from the ACK, including its subnet and router.
        mask = str(ipaddress.IPv4Address(reply[1]))
        prefix_length = ipaddress.IPv4Network(f"0.0.0.0/{mask}").prefixlen
        router = str(ipaddress.IPv4Address(reply[3]))
        subprocess.run(
            [
                "ip",
                "-4",
                "addr",
                "add",
                f"{address}/{prefix_length}",
                "dev",
                iface,
                "valid_lft",
                str(lifetime),
                "preferred_lft",
                str(lifetime),
            ],
            check=True,
        )
        subprocess.run(
            ["ip", "-4", "route", "add", "default", "via", router, "dev", iface],
            check=True,
        )
        return {
            "exchange": "Discover/Offer/Request/Ack",
            "address": address,
            "prefix_length": prefix_length,
            "gateway": router,
            "dns_servers": [str(ipaddress.IPv4Address(reply[6]))],
            "server": str(ipaddress.IPv4Address(reply[54])),
            "valid_lifetime": lifetime,
        }
    finally:
        if receiver is not None:
            receiver.close()
        sock.close()


if __name__ == "__main__":
    print(json.dumps(acquire(*sys.argv[1:])))
