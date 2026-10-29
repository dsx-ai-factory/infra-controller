#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Supervise a VM with a socketpair attachment to the private lab switch."""
import argparse
import fcntl
import os
from pathlib import Path
import select
import signal
import socket
import subprocess
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pass-fd", type=int, action="append", default=[])
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    devices = [(index, word) for index, word in enumerate(command)
               if word.startswith(("virtio-net,unixSocketPath=", "--device=virtio-net,unixSocketPath="))]
    if len(devices) != 1:
        parser.error("exactly one private lab network device is required")
    index, device = devices[0]
    path = device.split("unixSocketPath=", 1)[1].split(",", 1)[0]
    control_path = Path(path).with_suffix(".control")
    running = True

    def stop(*_):
        nonlocal running
        running = False

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    child = None
    descriptor = None
    switch, guest = socket.socketpair(socket.AF_UNIX, socket.SOCK_DGRAM)
    try:
        # VZ can permanently stall a virtio TX queue when attached directly to
        # a named datagram socket. A connected socketpair provides backpressure.
        for endpoint in (switch, guest):
            endpoint.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 1024**2)
            endpoint.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 * 1024**2)
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as control:
            control.settimeout(10)
            try:
                control.connect(str(control_path))
            except OSError as error:
                raise RuntimeError(f"lab socketpair endpoint unavailable: {control_path}; "
                                   "restart the lab with the updated switch") from error
            socket.send_fds(control, [b"P"], [switch.fileno()])
            if control.recv(1) != b"K":
                raise RuntimeError("lab switch rejected the network attachment")
            switch.close()
            # vmnet-run reserves low descriptors for the WAN attachment.
            descriptor = fcntl.fcntl(guest, fcntl.F_DUPFD_CLOEXEC, max([10, *args.pass_fd]) + 1)
            command[index] = device.replace(f"unixSocketPath={path}", f"fd={descriptor}")
            child = subprocess.Popen(command, pass_fds=(*args.pass_fd, descriptor), start_new_session=True)
            os.close(descriptor)
            descriptor = None
            guest.close()
            while running and child.poll() is None:
                if select.select([control], [], [], .2)[0]:
                    # The guest may close its NIC just before process exit.
                    try:
                        child.wait(timeout=.2)
                    except subprocess.TimeoutExpired:
                        raise RuntimeError("lab switch disconnected; stopping its VM") from None
    finally:
        switch.close()
        guest.close()
        if descriptor is not None:
            os.close(descriptor)
        if child is not None and child.poll() is None:
            try:
                os.killpg(child.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                child.wait(timeout=10)
            except subprocess.TimeoutExpired:
                os.killpg(child.pid, signal.SIGKILL)
                child.wait()
    return child.returncode if child.returncode >= 0 else 128 - child.returncode


if __name__ == "__main__":
    sys.exit(main())
