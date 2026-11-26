#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Private Ethernet learning switch over Unix datagrams; no host IP interface."""
import argparse
import errno
import fcntl
import os
import select
import signal
import socket
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("socket", type=Path)
    path = parser.parse_args().socket.resolve()
    path.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    lock = open(path.parent / "switch.lock", "w")
    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    path.unlink(missing_ok=True)
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
    # Match vfkit's packet buffers so a scheduling pause can absorb a burst
    # instead of returning ENOBUFS to a VM's network attachment.
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 1024**2)
    sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 * 1024**2)
    sock.bind(str(path))
    sock.setblocking(False)
    control_path = path.with_suffix(".control")
    control_path.unlink(missing_ok=True)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(str(control_path))
    listener.listen()
    listener.setblocking(False)
    controls = {}
    (path.parent / "switch.pid").write_text(str(os.getpid()) + "\n")
    peers = set()
    learned = {}
    running = True

    def stop(*_):
        nonlocal running
        running = False

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)

    def detach(peer):
        peers.discard(peer)
        for mac in [mac for mac, target in learned.items() if target == peer]:
            del learned[mac]
        if isinstance(peer, socket.socket):
            for owner in [owner for owner, port in controls.items() if port is peer]:
                del controls[owner]
                owner.close()
            peer.close()

    print(f"Ethernet switch ready: {path}", flush=True)
    try:
        while running:
            ports = [peer for peer in peers if isinstance(peer, socket.socket)]
            for ready in select.select([sock, listener, *controls, *ports], [], [], 1)[0]:
                if ready is listener:
                    connection, _ = listener.accept()
                    connection.setblocking(False)
                    controls[connection] = None
                    continue
                if ready in controls:
                    descriptors = []
                    try:
                        message, descriptors, flags, _ = socket.recv_fds(ready, 1, 1)
                        if controls[ready] is not None or message != b"P" or len(descriptors) != 1 or flags:
                            raise ValueError("invalid attachment or closed owner")
                        port = socket.socket(fileno=descriptors[0])
                        descriptors.clear()
                        controls[ready] = port
                        if port.family != socket.AF_UNIX or port.type != socket.SOCK_DGRAM:
                            raise ValueError("attachment is not a Unix datagram socket")
                        port.setblocking(False)
                        peers.add(port)
                        ready.sendall(b"K")
                    except (OSError, ValueError):
                        detach(controls.pop(ready))
                        ready.close()
                    finally:
                        for descriptor in descriptors:
                            os.close(descriptor)
                    continue
                if ready is not sock and ready not in peers:
                    continue
                try:
                    if ready is sock:
                        frame, sender = sock.recvfrom(65536)
                    else:
                        frame, sender = ready.recv(65536), ready
                except BlockingIOError:
                    continue
                except OSError:
                    if ready is sock:
                        raise
                    detach(ready)
                    continue
                if not sender:
                    continue
                if sender not in peers:
                    print(f"Attached {sender}", flush=True)
                    peers.add(sender)
                # Empty datagrams register peers before their first Ethernet frame.
                if len(frame) < 14:
                    continue
                destination, source = frame[:6], frame[6:12]
                if os.environ.get("VFLAB_SWITCH_TRACE") == "1":
                    print(f"FRAME {source.hex(':')} -> {destination.hex(':')} type={frame[12:14].hex()} bytes={len(frame)}", flush=True)
                if not source[0] & 1:
                    learned[source] = sender
                recipient = learned.get(destination)
                targets = {recipient} if recipient and not destination[0] & 1 else peers.copy()
                for target in targets - {sender}:
                    try:
                        if isinstance(target, socket.socket):
                            target.send(frame)
                        else:
                            sock.sendto(frame, socket.MSG_DONTWAIT, target)
                    except (BlockingIOError, socket.timeout):
                        pass
                    except OSError as error:
                        # Darwin reports a full datagram receive queue as ENOBUFS.
                        # Drop this packet without forgetting the live destination.
                        if error.errno == errno.ENOBUFS:
                            continue
                        print(f"Detached {target}: {error}", flush=True)
                        detach(target)
    finally:
        print("Switch stopped", flush=True)
        for connection, port in controls.items():
            connection.close()
            if port is not None:
                port.close()
        listener.close()
        sock.close()
        control_path.unlink(missing_ok=True)
        path.unlink(missing_ok=True)
        (path.parent / "switch.pid").unlink(missing_ok=True)


if __name__ == "__main__":
    main()
