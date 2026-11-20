# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
from deployment_test_support import load_script
import ipaddress
import errno
import struct
import unittest
from unittest.mock import Mock, patch

from pathlib import Path

relay = load_script('dhcp6_relay', Path(__file__).resolve().parents[1] / 'lab/dhcp6_relay.py')


class RelayTests(unittest.TestCase):
    def setUp(self):
        outgoing = patch.object(relay.socket, 'PACKET_OUTGOING', 4, create=True)
        outgoing.start()
        self.addCleanup(outgoing.stop)
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

    def run_events(self, events, upstream=None, allowed=None):
        upstream = upstream or Mock()
        packets = Mock()
        selected = []
        frames = []
        responses = []
        times = []
        for now, source, value in events:
            times.append(now)
            selected.append(([packets if source == 'client' else upstream], [], []))
            (frames if source == 'client' else responses).append(value)
        packets.recvmsg.side_effect = frames
        upstream.recv.side_effect = responses
        with patch.object(relay.time, 'monotonic', side_effect=times + [999]), \
                patch.object(relay.select, 'select', side_effect=selected + [StopIteration]), \
                self.assertRaises(StopIteration):
            relay.run_relay(upstream, packets, self.link, {self.mac} if allowed is None else allowed, 7)
        return upstream

    def client_event(self, now=0, outgoing=False):
        return now, 'client', (self.frame, [], 0, ('bridge', 0, relay.socket.PACKET_OUTGOING if outgoing else 0))

    def server_event(self, now=0, transaction=b"\x12\x34\x56"):
        message = b"\x07" + transaction
        return now, 'server', b"\x0d\0" + self.link.packed + self.peer.packed + relay.option(9, message)

    def test_transient_upstream_failure_keeps_relay_available_for_client_retry(self):
        upstream = Mock()
        upstream.send.side_effect = [PermissionError(errno.EPERM, 'Service rejected packet'), 60]
        self.run_events([self.client_event(), (1, 'server', ConnectionRefusedError(errno.ECONNREFUSED, 'restarting')),
                         self.client_event(2), self.server_event(3)], upstream)
        self.assertEqual(upstream.send.call_count, 2)
        upstream.sendto.assert_called_once_with(b"\x07\x12\x34\x56", (str(self.peer), 546, 0, 7))
        upstream.send.side_effect = OSError(errno.EBADF, 'invalid socket')
        with self.assertRaises(OSError):
            self.run_events([self.client_event()], upstream)

    def test_loop_requires_allowed_incoming_client_and_live_matching_transaction(self):
        for name, events, allowed in (
            ('foreign MAC', [self.client_event(), self.server_event()], {b'foreign'}),
            ('outgoing', [self.client_event(outgoing=True), self.server_event()], None),
            ('unsolicited', [self.server_event()], None),
            ('wrong transaction', [self.client_event(), self.server_event(transaction=b'bad')], None),
            ('expired', [self.client_event(), self.server_event(relay.REQUEST_LIFETIME)], None),
        ):
            with self.subTest(name=name):
                upstream = self.run_events(events, allowed=allowed)
                upstream.sendto.assert_not_called()
                if name in ('foreign MAC', 'outgoing'):
                    upstream.send.assert_not_called()

    def test_pending_bound_allows_retry_and_recovers_after_expiration(self):
        # Two slots distinguish admission, retry of an admitted request, and expiration.
        other = (self.mac, self.peer, b'\x01abc')
        rejected = (self.mac, self.peer, b'\x01def')
        original = (self.mac, self.peer, self.message)
        with patch.object(relay, 'MAX_PENDING', 2), \
                patch.object(relay, 'client_packet', side_effect=[original, other, rejected, original, rejected]):
            upstream = self.run_events([self.client_event(t) for t in (0, 1, 2, 3, 124)])
        self.assertEqual(upstream.send.call_count, 4)
        self.assertEqual(upstream.send.call_args.args[0], relay.forward(self.link, self.peer, self.mac, rejected[2]))

    def test_bind_waits_for_dad_but_does_not_hide_a_port_conflict(self):
        upstream = Mock()
        upstream.bind.side_effect = [OSError(errno.EADDRNOTAVAIL, 'tentative'), None]
        with patch.object(relay.time, 'sleep') as sleep:
            relay.bind_link(upstream, self.link)
        self.assertEqual(upstream.bind.call_count, 2)
        upstream.bind.assert_called_with((str(self.link), 547))
        sleep.assert_called_once_with(0.1)
        upstream.bind.side_effect = OSError(errno.EADDRINUSE, 'port conflict')
        with patch.object(relay.time, 'sleep') as sleep, self.assertRaises(OSError):
            relay.bind_link(upstream, self.link)
        sleep.assert_not_called()

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
