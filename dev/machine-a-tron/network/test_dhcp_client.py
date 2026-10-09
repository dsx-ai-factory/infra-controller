# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import ipaddress
from pathlib import Path
import runpy
import socket
import struct
import subprocess
import sys
import unittest
from unittest.mock import Mock, mock_open, patch

HERE = Path(__file__).resolve().parent
DUID = struct.pack("!HH", 3, 1) + bytes.fromhex("020000000001")
ADDRESS = "fd00:84:100::1"


def option(code, value):
    return struct.pack("!HH", code, len(value)) + value


class DhcpClientTests(unittest.TestCase):
    def test_invalid_protocol_data_never_installs_a_lease(self):
        for failure in (
            "advertise-client",
            "reply-server",
            "address",
            "lifetime",
            "dns",
        ):
            with self.subTest(failure=failure):
                sock = Mock()

                def receive(_size):
                    sent = sock.sendto.call_args.args[0]
                    advertise = sent[0] == 1
                    client = (
                        b"wrong"
                        if advertise and failure == "advertise-client"
                        else DUID
                    )
                    server = (
                        b"wrong"
                        if not advertise and failure == "reply-server"
                        else b"server"
                    )
                    address = "fd00:84:100::3" if failure == "address" else ADDRESS
                    lifetime = 0 if failure == "lifetime" else 7200
                    lease = ipaddress.IPv6Address(address).packed + struct.pack(
                        "!II", 3600, lifetime
                    )
                    ia = struct.pack("!III", 1, 0, 0) + option(5, lease)
                    dns = (
                        b"bad"
                        if failure == "dns"
                        else ipaddress.IPv6Address("fd00:84:100::").packed
                    )
                    packet = bytes([2 if advertise else 7]) + sent[1:4]
                    packet += (
                        option(1, client)
                        + option(2, server)
                        + option(3, ia)
                        + option(23, dns)
                    )
                    return packet, ("fe80::1", 547, 0, 1)

                sock.recvfrom.side_effect = receive
                # macOS does not expose Linux's SO_BINDTODEVICE constant.
                with patch.object(
                    sys, "argv", ["dhcp_client.py", "eth0", ADDRESS]
                ), patch("socket.if_nametoindex", return_value=1), patch(
                    "socket.socket", return_value=sock
                ), patch.object(
                    socket, "SO_BINDTODEVICE", 25, create=True
                ), patch(
                    "builtins.open", mock_open(read_data="02:00:00:00:00:01\n")
                ), patch(
                    "subprocess.run"
                ) as install:
                    with self.assertRaises(RuntimeError):
                        runpy.run_path(
                            str(HERE / "dhcp_client.py"), run_name="__main__"
                        )
                    install.assert_not_called()

    def test_unrelated_or_short_packets_cannot_extend_the_retry_deadline(self):
        for response in (b"\x02\x00\x00\x00", b""):
            with self.subTest(response=response):
                sock = Mock()
                clock = [0]

                def receive(_size):
                    clock[0] += 1
                    if clock[0] > 16:
                        self.fail("unrelated packets extended the DHCPv6 retry budget")
                    return response, ("fe80::1", 547, 0, 1)

                sock.recvfrom.side_effect = receive
                with patch.object(
                    sys, "argv", ["dhcp_client.py", "eth0", ADDRESS]
                ), patch("socket.if_nametoindex", return_value=1), patch(
                    "socket.socket", return_value=sock
                ), patch.object(
                    socket, "SO_BINDTODEVICE", 25, create=True
                ), patch(
                    "builtins.open", mock_open(read_data="02:00:00:00:00:01\n")
                ), patch(
                    "os.urandom", return_value=b"\x01\x02\x03"
                ), patch(
                    "time.monotonic", side_effect=lambda: clock[0]
                ), patch(
                    "subprocess.run"
                ) as install:
                    with self.assertRaisesRegex(TimeoutError, "DHCPv6 response absent"):
                        runpy.run_path(
                            str(HERE / "dhcp_client.py"), run_name="__main__"
                        )
                self.assertEqual(sock.sendto.call_count, 4)
                self.assertEqual(clock[0], 16)
                install.assert_not_called()

    def test_validation_survives_optimized_python(self):
        result = subprocess.run(
            [
                sys.executable,
                "-B",
                "-O",
                "-m",
                "unittest",
                "test_dhcp_client.DhcpClientTests.test_invalid_protocol_data_never_installs_a_lease",
            ],
            cwd=HERE,
            text=True,
            capture_output=True,
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
