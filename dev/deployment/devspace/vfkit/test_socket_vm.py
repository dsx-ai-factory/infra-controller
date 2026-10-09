#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Exercise descriptor transfer, VM supervision and real lab socket forwarding."""
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parent / "lab/lib"
GUEST = """
import os,socket,sys
fd=int(sys.argv[1].split('fd=',1)[1].split(',',1)[0])
nic=socket.socket(fileno=fd)
assert nic.getsockname() == '' and nic.getpeername() == ''
assert os.get_blocking(fd)
if len(sys.argv)>2:
    wan=socket.socket(fileno=int(sys.argv[2]))
    wan.send(b'WAN preserved')
print(os.getpid(),flush=True)
while True:
    frame=nic.recv(65536)
    nic.send(frame[6:12]+frame[:6]+frame[12:])
    if frame.endswith(b'exit'): break
"""


class SocketVMTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="nico-pair-", dir="/tmp")
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name) / "switch.sock"
        self.switch = subprocess.Popen([sys.executable, str(ROOT / "switch.py"), str(self.path)],
                                       stdout=subprocess.DEVNULL)
        self.addCleanup(self.stop, self.switch)
        deadline = time.monotonic() + 5
        while not self.path.with_suffix(".control").exists():
            self.assertIsNone(self.switch.poll())
            self.assertLess(time.monotonic(), deadline)
            time.sleep(.02)

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.terminate()
        process.wait(timeout=15)

    def guest(self, pass_fd=None):
        extra = [] if pass_fd is None else ["--pass-fd", str(pass_fd)]
        command = [sys.executable, str(ROOT / "socket_vm.py"), *extra, "--", sys.executable,
                   "-u", "-c", GUEST, f"virtio-net,unixSocketPath={self.path},mac=02:00:00:00:00:01"]
        if pass_fd is not None:
            command.append(str(pass_fd))
        process = subprocess.Popen(command, pass_fds=() if pass_fd is None else (pass_fd,),
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        self.addCleanup(process.stdout.close)
        self.addCleanup(process.stderr.close)
        self.addCleanup(self.stop, process)
        self.assertTrue(select.select([process.stdout], [], [], 5)[0])
        pid = int(process.stdout.readline())
        return process, pid

    def test_socketpair_roundtrip_preserves_wan_and_child_exit(self):
        wan, inherited = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
        self.addCleanup(wan.close)
        self.addCleanup(inherited.close)
        wan.settimeout(5)
        process, _ = self.guest(inherited.fileno())
        self.assertEqual(wan.recv(100), b"WAN preserved")
        with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as peer:
            peer.bind(str(self.path.parent / "peer"))
            peer.connect(str(self.path))
            peer.settimeout(5)
            frame = bytes.fromhex("0200000000010200000000020800") + b"exit"
            peer.send(frame)
            self.assertEqual(peer.recv(1500), frame[6:12] + frame[:6] + frame[12:])
        output, error = process.communicate(timeout=5)
        self.assertEqual(process.returncode, 0, output + error)
        self.assertIsNone(self.switch.poll())

    def test_supervisor_stops_child_on_signal_or_switch_loss(self):
        for cause in ("signal", "switch-loss"):
            with self.subTest(cause=cause):
                process, pid = self.guest()
                if cause == "signal":
                    process.send_signal(signal.SIGTERM)
                else:
                    self.stop(self.switch)
                process.communicate(timeout=5)
                with self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)

    def test_invalid_descriptor_does_not_kill_switch(self):
        read_fd, write_fd = os.pipe()
        try:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
                control.settimeout(5)
                control.connect(str(self.path.with_suffix(".control")))
                socket.send_fds(control, [b"P"], [read_fd])
                self.assertEqual(control.recv(1), b"")
            process, _ = self.guest()
            self.assertIsNone(process.poll())
        finally:
            os.close(read_fd)
            os.close(write_fd)

    def test_failed_port_disconnects_owner_without_stopping_switch(self):
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control, \
                socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as peer:
            control.settimeout(5)
            control.connect(str(self.path.with_suffix(".control")))
            switch, guest = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
            try:
                socket.send_fds(control, [b"P"], [switch.fileno()])
                self.assertEqual(control.recv(1), b"K")
            finally:
                switch.close()
                guest.close()
            peer.bind(str(self.path.parent / "peer"))
            peer.connect(str(self.path))
            peer.send(b"\xff" * 6 + bytes.fromhex("0200000000020800") + b"probe")
            self.assertEqual(control.recv(1), b"")
            self.assertIsNone(self.switch.poll())


if __name__ == "__main__":
    unittest.main()
