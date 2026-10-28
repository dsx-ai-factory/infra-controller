#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Single-link lab DHCPv6 relay with RFC 6939 client MAC metadata."""

import argparse
import errno
import ipaddress
import select
import socket
import struct
import time

RELAY_FORWARD = 12
RELAY_REPLY = 13
RELAY_MESSAGE = 9
CLIENT_LINK_LAYER = 79
CLIENT_MESSAGES = {1, 3, 4, 5, 6, 8, 9, 11}
SERVER_MESSAGES = {2, 7}
MAX_PENDING = 256
REQUEST_LIFETIME = 120
SOL_PACKET = 263
PACKET_AUXDATA = 8
TP_STATUS_CSUMNOTREADY = 1 << 3
TP_STATUS_VLAN_VALID = 1 << 4
TP_STATUS_CSUM_VALID = 1 << 7
AUXDATA_SIZE = 20


def checksum(data):
    if len(data) % 2:
        data += b"\0"
    value = sum(struct.unpack(f"!{len(data) // 2}H", data))
    while value >> 16:
        value = (value & 0xFFFF) + (value >> 16)
    return (~value) & 0xFFFF


def packet_status(ancillary):
    for level, kind, data in ancillary:
        if level == SOL_PACKET and kind == PACKET_AUXDATA and len(data) >= AUXDATA_SIZE:
            return struct.unpack_from("@I", data)[0]
    return 0


def client_packet(frame, status=0):
    """Accept unfragmented Ethernet/IPv6/UDP clients on the flat lab bridge."""
    if status & TP_STATUS_VLAN_VALID or len(frame) < 66 or frame[12:14] != b"\x86\xdd":
        return None
    packet = frame[14:]
    if packet[0] >> 4 != 6 or packet[6] != socket.IPPROTO_UDP:
        return None
    length = int.from_bytes(packet[4:6], "big")
    udp = packet[40 : 40 + length]
    if len(udp) != length or len(udp) < 12:
        return None
    source_port, destination_port, udp_length, udp_checksum = struct.unpack(
        "!HHHH", udp[:8]
    )
    if (
        (source_port, destination_port) != (546, 547)
        or udp_length != length
        or (not udp_checksum and not status & TP_STATUS_CSUMNOTREADY)
    ):
        return None
    pseudo_header = packet[8:40] + struct.pack("!I3xB", length, socket.IPPROTO_UDP)
    # TAP/virtio may deliver a partial checksum before the kernel completes it.
    # Only trust that exception when the packet socket provides offload metadata.
    verified = status & (TP_STATUS_CSUMNOTREADY | TP_STATUS_CSUM_VALID)
    if (not verified and checksum(pseudo_header + udp) != 0) or udp[
        8
    ] not in CLIENT_MESSAGES:
        return None
    peer = ipaddress.IPv6Address(packet[8:24])
    if peer.is_unspecified or peer.is_multicast:
        return None
    return frame[6:12], peer, udp[8:]


def option(code, value):
    return struct.pack("!HH", code, len(value)) + value


def forward(link, peer, mac, message):
    return (
        bytes([RELAY_FORWARD, 0])
        + link.packed
        + peer.packed
        + option(CLIENT_LINK_LAYER, b"\0\1" + mac)
        + option(RELAY_MESSAGE, message)
    )


def reply(packet, link):
    if (
        len(packet) < 34
        or packet[:2] != bytes([RELAY_REPLY, 0])
        or packet[2:18] != link.packed
    ):
        return None
    peer = ipaddress.IPv6Address(packet[18:34])
    message = None
    offset = 34
    while offset < len(packet):
        if len(packet) - offset < 4:
            return None
        code, length = struct.unpack("!HH", packet[offset : offset + 4])
        offset += 4
        if offset + length > len(packet):
            return None
        if code == RELAY_MESSAGE:
            if message is not None:
                return None
            message = packet[offset : offset + length]
        offset += length
    if message is None or len(message) < 4 or message[0] not in SERVER_MESSAGES:
        return None
    return peer, message


def bind_link(upstream, address):
    # A newly raised bridge needs time to complete IPv6 duplicate-address detection.
    for attempt in range(50):
        try:
            upstream.bind((str(address), 547))
            return
        except OSError as error:
            if error.errno != errno.EADDRNOTAVAIL or attempt == 49:
                raise
            time.sleep(0.1)


def exchange(operation, *args):
    try:
        return operation(*args)
    except OSError as error:
        # Connected UDP reports ICMP errors during server restarts or route recovery.
        # Netfilter rejection can also report EPERM/EACCES while a Service has no endpoints.
        # Clients retry DHCP; an upstream outage must not restart the target guest.
        if error.errno not in {errno.ECONNREFUSED, errno.ENETUNREACH, errno.EHOSTUNREACH,
                               errno.ENETDOWN, errno.ETIMEDOUT, errno.ENOBUFS,
                               errno.EPERM, errno.EACCES}:
            raise
        print(f"DHCPv6 packet deferred: {error}", flush=True)
        return None


def run_relay(upstream, packets, link_address, allowed, interface_index):
    pending = {}
    while True:
        now = time.monotonic()
        pending = {
            key: expires for key, expires in pending.items() if expires > now
        }
        readable, _, _ = select.select([packets, upstream], [], [], 1)
        for source in readable:
            if source is packets:
                frame, ancillary, flags, address = packets.recvmsg(
                    65535, socket.CMSG_SPACE(AUXDATA_SIZE)
                )
                if flags & (socket.MSG_TRUNC | socket.MSG_CTRUNC):
                    continue
                if address[2] == socket.PACKET_OUTGOING:
                    continue
                request = client_packet(frame, packet_status(ancillary))
                if request is None:
                    continue
                mac, peer, message = request
                if mac not in allowed:
                    continue
                key = (peer, message[1:4])
                if key not in pending and len(pending) >= MAX_PENDING:
                    continue
                pending[key] = now + REQUEST_LIFETIME
                if exchange(upstream.send, forward(link_address, peer, mac, message)) is None:
                    continue
                print(
                    f"Forwarded DHCPv6 type={message[0]} peer={peer} mac={mac.hex(':')}",
                    flush=True,
                )
            else:
                packet = exchange(upstream.recv, 65535)
                if packet is None:
                    continue
                response = reply(packet, link_address)
                if response is None:
                    continue
                peer, message = response
                if (peer, message[1:4]) not in pending:
                    continue
                if exchange(upstream.sendto, message, (str(peer), 546, 0, interface_index)) is None:
                    continue
                print(f"Returned DHCPv6 type={message[0]} peer={peer}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--interface", required=True)
    parser.add_argument("--link-address", required=True, type=ipaddress.IPv6Address)
    parser.add_argument("--server", required=True, type=ipaddress.IPv6Address)
    parser.add_argument("--mac", action="append", required=True)
    args = parser.parse_args()
    allowed = {bytes.fromhex(mac.replace(":", "")) for mac in args.mac}
    if any(len(mac) != 6 or mac[0] & 1 for mac in allowed):
        parser.error("--mac requires an Ethernet unicast address")
    interface_index = socket.if_nametoindex(args.interface)
    with socket.socket(socket.AF_INET6, socket.SOCK_DGRAM) as upstream, socket.socket(
        socket.AF_PACKET, socket.SOCK_RAW, socket.htons(0x86DD)
    ) as packets:
        upstream.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 1)
        upstream.setsockopt(
            socket.IPPROTO_IPV6,
            socket.IPV6_JOIN_GROUP,
            socket.inet_pton(socket.AF_INET6, "ff02::1:2")
            + struct.pack("@I", interface_index),
        )
        bind_link(upstream, args.link_address)
        # A connected UDP socket accepts replies only from this configured server.
        upstream.connect((str(args.server), 547))
        packets.setsockopt(SOL_PACKET, PACKET_AUXDATA, 1)
        packets.bind((args.interface, 0))
        run_relay(upstream, packets, args.link_address, allowed, interface_index)


if __name__ == "__main__":
    main()
