# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import ipaddress
import struct
import unittest

import dhcp6_relay as relay


class RelayTests(unittest.TestCase):
    def setUp(self):
        self.link = ipaddress.IPv6Address("fd42:84:30::1")
        self.peer = ipaddress.IPv6Address("fe80::1234")
        self.mac = bytes.fromhex("020000010002")
        self.message = b"\x01\x12\x34\x56" + relay.option(
            1, b"\x00\x04" + bytes(range(16))
        )
        destination = ipaddress.IPv6Address("ff02::1:2")
        length = 8 + len(self.message)
        udp = struct.pack("!HHHH", 546, 547, length, 0) + self.message
        pseudo = (
            self.peer.packed + destination.packed + struct.pack("!I3xB", length, 17)
        )
        udp = udp[:6] + struct.pack("!H", relay.checksum(pseudo + udp)) + udp[8:]
        ip = (
            struct.pack("!IHBB", 6 << 28, length, 17, 1)
            + self.peer.packed
            + destination.packed
        )
        self.frame = bytes.fromhex("333300010002") + self.mac + b"\x86\xdd" + ip + udp

    def test_uuid_duid_keeps_real_ethernet_identity_and_original_payload(self):
        mac, peer, message = relay.client_packet(self.frame)
        self.assertEqual((mac, peer, message), (self.mac, self.peer, self.message))
        forwarded = relay.forward(self.link, peer, mac, message)
        self.assertEqual(forwarded[:34], b"\x0c\0" + self.link.packed + peer.packed)
        self.assertEqual(forwarded[34:46], b"\0\x4f\0\x08\0\x01" + self.mac)
        self.assertEqual(forwarded[46:], relay.option(9, self.message))

    def test_invalid_or_unsupported_client_frames_are_not_forwarded(self):
        for name, frame in (
            ("truncated", self.frame[:-1]),
            ("corrupt checksum", self.frame[:-1] + b"\xff"),
            ("IPv6 extension header", self.frame[:20] + b"\x00" + self.frame[21:]),
        ):
            with self.subTest(name=name):
                self.assertIsNone(relay.client_packet(frame))

    def test_checksum_offload_requires_kernel_metadata_and_rejects_stripped_vlan(self):
        partial = self.frame[:60] + b"\x12\x34" + self.frame[62:]
        self.assertIsNone(relay.client_packet(partial))
        metadata = [
            (
                relay.SOL_PACKET,
                relay.PACKET_AUXDATA,
                struct.pack(
                    "@IIIHHHH",
                    relay.TP_STATUS_CSUMNOTREADY,
                    len(partial),
                    len(partial),
                    0,
                    14,
                    0,
                    0,
                ),
            )
        ]
        self.assertEqual(
            relay.client_packet(partial, relay.packet_status(metadata)),
            (self.mac, self.peer, self.message),
        )
        self.assertIsNone(
            relay.client_packet(
                partial, relay.TP_STATUS_CSUMNOTREADY | relay.TP_STATUS_VLAN_VALID
            )
        )

    def test_reply_preserves_payload_and_rejects_wrong_link_or_ambiguous_options(self):
        message = b"\x07\x12\x34\x56" + relay.option(13, b"\0\0")
        header = b"\x0d\0" + self.link.packed + self.peer.packed
        option = relay.option(9, message)
        self.assertEqual(relay.reply(header + option, self.link), (self.peer, message))
        for name, packet in (
            (
                "wrong link",
                b"\x0d\0"
                + ipaddress.IPv6Address("fd42:84:31::1").packed
                + self.peer.packed
                + option,
            ),
            ("truncated option", header + option[:-1]),
            ("duplicate relay message", header + option + option),
        ):
            with self.subTest(name=name):
                self.assertIsNone(relay.reply(packet, self.link))


if __name__ == "__main__":
    unittest.main()
