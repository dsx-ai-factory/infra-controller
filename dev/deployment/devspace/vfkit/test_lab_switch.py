#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Exercise real switch sockets: per-lab flooding, unicast and IPv6 multicast."""
import socket
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent / "lab"


class SwitchTests(unittest.TestCase):
    def test_congested_peer_preserves_forwarding_and_recovers_without_registration(self):
        with tempfile.TemporaryDirectory(prefix="nico-switch-congestion-", dir="/tmp") as tmp:
            root = Path(tmp)
            path = root / "switch.sock"
            process = subprocess.Popen([sys.executable, str(ROOT / "lib/switch.py"), str(path)],
                                       stdout=subprocess.DEVNULL)
            peers = []
            try:
                deadline = time.monotonic() + 5
                while not path.exists():
                    self.assertIsNone(process.poll())
                    self.assertLess(time.monotonic(), deadline)
                    time.sleep(.02)
                for name in ("sender", "congested", "healthy"):
                    peer = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
                    peer.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4096)
                    peer.bind(str(root / name))
                    peer.connect(str(path))
                    peer.settimeout(1)
                    peer.send(b"")
                    peers.append(peer)
                time.sleep(.05)
                sender, congested, healthy = peers
                macs = [bytes.fromhex("5254000001" + suffix) for suffix in ("11", "12", "13")]
                frame = macs[1] + macs[0] + b"\x08\x00" + b"x" * 1486
                sender.send(frame)
                self.assertEqual(congested.recv(2048), frame)
                self.assertEqual(healthy.recv(2048), frame)
                for peer, mac in zip((congested, healthy), macs[1:]):
                    reply = macs[0] + mac + b"\x08\x00" + b"learn"
                    peer.send(reply)
                    self.assertEqual(sender.recv(2048), reply)

                # Stop consuming B's queue while A keeps sending. Congestion
                # must neither block C nor evict B's learned MAC/attachment.
                for _ in range(64):
                    sender.send(frame)
                marker = macs[2] + macs[0] + b"\x08\x00" + b"healthy"
                sender.send(marker)
                self.assertEqual(healthy.recv(2048), marker)
                congested.setblocking(False)
                while True:
                    try:
                        congested.recv(2048)
                    except BlockingIOError:
                        break
                congested.settimeout(1)
                recovered = macs[1] + macs[0] + b"\x08\x00" + b"recovered"
                sender.send(recovered)
                self.assertEqual(congested.recv(2048), recovered)
                healthy.settimeout(.1)
                with self.assertRaises(socket.timeout):
                    healthy.recv(2048)
            finally:
                for peer in peers:
                    peer.close()
                process.terminate()
                process.wait(timeout=5)

    def test_lab_isolation_and_forwarding(self):
        with tempfile.TemporaryDirectory(prefix="nico-switch-test-", dir="/tmp") as tmp:
            root = Path(tmp)
            processes, peers = [], []
            try:
                for lab in ("a", "b"):
                    path = root / lab / "switch.sock"
                    process = subprocess.Popen([sys.executable, str(ROOT / "lib/switch.py"), str(path)],
                                               stdout=subprocess.DEVNULL)
                    processes.append(process)
                    deadline = time.monotonic() + 5
                    while not path.exists():
                        self.assertIsNone(process.poll())
                        self.assertLess(time.monotonic(), deadline)
                        time.sleep(.02)
                for name, lab in (("one", "a"), ("two", "a"), ("three", "b")):
                    peer = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
                    peer.bind(str(root / name))
                    peer.connect(str(root / lab / "switch.sock"))
                    peer.settimeout(.3)
                    peer.send(b"")
                    peers.append(peer)
                time.sleep(.05)
                mac1 = bytes.fromhex("525400000111")
                mac2 = bytes.fromhex("525400000112")
                broadcast = b"\xff" * 6 + mac1 + b"\x08\x00" + b"DHCP broadcast"
                peers[0].send(broadcast)
                self.assertEqual(peers[1].recv(1500), broadcast)
                for peer in (peers[0], peers[2]):
                    with self.assertRaises(socket.timeout):
                        peer.recv(1500)
                unicast = mac1 + mac2 + b"\x08\x00" + b"unicast reply"
                peers[1].send(unicast)
                self.assertEqual(peers[0].recv(1500), unicast)
                multicast = bytes.fromhex("333300010002") + mac1 + b"\x86\xdd" + b"DHCPv6 multicast"
                peers[0].send(multicast)
                self.assertEqual(peers[1].recv(1500), multicast)
                with self.assertRaises(socket.timeout):
                    peers[2].recv(1500)
                # A restarted VM reuses its socket path and MAC. The switch
                # must accept the replacement endpoint without a lab restart.
                peers[0].close()
                (root / "one").unlink()
                replacement = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
                replacement.bind(str(root / "one"))
                replacement.connect(str(root / "a/switch.sock"))
                replacement.settimeout(.3)
                peers[0] = replacement
                replacement.send(broadcast)
                self.assertEqual(peers[1].recv(1500), broadcast)
                peers[1].send(unicast)
                self.assertEqual(replacement.recv(1500), unicast)
                # A scheduling pause must not exhaust the default two-frame
                # macOS receive buffer and stall a VM's transmit queue.
                peers[1].setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 * 1024**2)
                processes[0].send_signal(signal.SIGSTOP)
                time.sleep(.05)
                burst = mac2 + mac1 + b"\x08\x00" + b"x" * 1486
                # Linux additionally limits the number of queued datagrams,
                # independently of buffer bytes; do not require a CI sysctl.
                burst_count = 32
                queue_limit = Path("/proc/sys/net/unix/max_dgram_qlen")
                if queue_limit.exists():
                    burst_count = min(burst_count, int(queue_limit.read_text()))
                replacement.setblocking(False)
                try:
                    for _ in range(burst_count):
                        replacement.send(burst)
                finally:
                    processes[0].send_signal(signal.SIGCONT)
                for _ in range(burst_count):
                    self.assertEqual(peers[1].recv(2048), burst)
            finally:
                for peer in peers:
                    peer.close()
                for process in processes:
                    process.terminate()
                    process.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
