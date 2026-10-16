# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import ipaddress
import socket
import struct
import unittest
from unittest.mock import Mock, mock_open, patch

import dhcpv4_client

ADDRESS = "10.84.100.1"
GATEWAY = "10.84.100.0"
PREFIX = "10.84.100.0/31"
MAC = bytes.fromhex("020000000001")
COOKIE = bytes.fromhex("63825363")


def option(code, value):
    return bytes([code, len(value)]) + value


class Dhcpv4Tests(unittest.TestCase):
    def acquire(self, failure=None, checksum_mode="complete"):
        sock = Mock()
        phases = []

        def receive(_size, _ancsize):
            sent = sock.sendto.call_args.args[0]
            # The first DHCP option in a request is its message type.
            phase = sent[242]
            phases.append(phase)
            self.assertEqual(sent[:3], bytes([1, 1, 6]))
            self.assertEqual(sent[28:34], MAC)
            self.assertEqual(sent[236:240], COOKIE)
            if phase == 3:
                self.assertIn(option(50, ipaddress.IPv4Address(ADDRESS).packed), sent)
                self.assertIn(option(54, ipaddress.IPv4Address(GATEWAY).packed), sent)
            reply = bytearray(sent[:240])
            reply[0] = 2
            reply[16:20] = ipaddress.IPv4Address(ADDRESS).packed
            values = {
                53: bytes([2 if phase == 1 else 5]),
                54: ipaddress.IPv4Address(GATEWAY).packed,
                1: ipaddress.IPv4Address("255.255.255.254").packed,
                3: ipaddress.IPv4Address(GATEWAY).packed,
                6: ipaddress.IPv4Address(GATEWAY).packed,
                51: struct.pack("!I", 7200),
            }
            if phase == 3:
                if failure == "address":
                    reply[16:20] = ipaddress.IPv4Address("10.84.100.3").packed
                elif failure in ("server", "gateway", "dns"):
                    code = {"server": 54, "gateway": 3, "dns": 6}[failure]
                    values[code] = ipaddress.IPv4Address("10.84.100.2").packed
                elif failure == "mask":
                    values[1] = ipaddress.IPv4Address("255.255.255.0").packed
                elif failure == "lifetime":
                    values[51] = struct.pack("!I", 0)
            payload = (
                bytes(reply)
                + b"".join(option(k, v) for k, v in values.items())
                + b"\xff"
            )

            def checksum(data):
                data += b"\x00" * (len(data) % 2)
                total = sum(struct.unpack(f"!{len(data) // 2}H", data))
                while total > 65535:
                    total = (total & 65535) + (total >> 16)
                return struct.pack("!H", total ^ 65535)

            header = bytearray(20)
            header[0], header[9] = 0x45, 17
            header[2:4] = struct.pack("!H", 28 + len(payload))
            header[12:16] = ipaddress.IPv4Address(GATEWAY).packed
            header[16:20] = b"\xff" * 4
            header[10:12] = checksum(bytes(header))
            udp = bytearray(struct.pack("!HHHH", 67, 68, 8 + len(payload), 0))
            pseudo = header[12:20] + bytes([0, 17]) + udp[4:6]
            udp[6:8] = checksum(bytes(pseudo + udp) + payload)
            ancillary = []
            if checksum_mode == "omitted":
                udp[6:8] = b"\0\0"
            elif checksum_mode == "offloaded":
                udp[6] ^= 1
                ancillary = [
                    (263, 8, struct.pack("=IIIHHHH", 1 << 3, 0, 0, 0, 0, 0, 0))
                ]
            if failure == "ip-checksum":
                header[8] ^= 1
            if failure == "udp-checksum":
                udp[6] ^= 1
            return bytes(header + udp) + payload, ancillary, 0, ("eth0", 0)

        sock.recvmsg.side_effect = receive
        with patch("socket.socket", return_value=sock), patch.object(
            socket, "AF_PACKET", 17, create=True
        ), patch.object(socket, "SO_BINDTODEVICE", 25, create=True), patch(
            "builtins.open", mock_open(read_data="02:00:00:00:00:01\n")
        ), patch(
            "subprocess.run"
        ) as install:
            if failure:
                with self.assertRaises(RuntimeError):
                    dhcpv4_client.acquire("eth0", ADDRESS, PREFIX, GATEWAY, GATEWAY)
                install.assert_not_called()
            else:
                lease = dhcpv4_client.acquire("eth0", ADDRESS, PREFIX, GATEWAY, GATEWAY)
                self.assertEqual(lease["address"], ADDRESS)
                self.assertEqual(lease["prefix_length"], 31)
                self.assertEqual(lease["gateway"], GATEWAY)
                self.assertEqual(lease["dns_servers"], [GATEWAY])
                self.assertEqual(phases, [1, 3])
                self.assertEqual(
                    install.call_args_list[0].args[0][-4:],
                    ["valid_lft", "7200", "preferred_lft", "7200"],
                )
                self.assertEqual(
                    install.call_args_list[-1].args[0],
                    [
                        "ip",
                        "-4",
                        "route",
                        "add",
                        "default",
                        "via",
                        GATEWAY,
                        "dev",
                        "eth0",
                    ],
                )

    def test_dora_installs_the_validated_lease_and_gateway(self):
        for mode in ("complete", "omitted", "offloaded"):
            with self.subTest(checksum_mode=mode):
                self.acquire(checksum_mode=mode)

    def test_invalid_ack_cannot_install_address_or_routes(self):
        for failure in (
            "server",
            "address",
            "mask",
            "gateway",
            "dns",
            "lifetime",
            "ip-checksum",
            "udp-checksum",
        ):
            with self.subTest(failure=failure):
                self.acquire(failure)
