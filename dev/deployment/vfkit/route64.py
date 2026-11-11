# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Import the data-only subset of a Route64 WireGuard export."""

import base64
import configparser
import ipaddress
from pathlib import Path
import re


def configuration(path):
    """Normalize an IPv6 tunnel; never execute hooks from an imported config."""
    config = configparser.ConfigParser(interpolation=None, delimiters=("=",), inline_comment_prefixes=("#", ";"))
    try:
        config.read_string(Path(path).expanduser().read_text())
        if config.defaults() or set(config.sections()) != {"Interface", "Peer"}:
            raise ValueError()
        interface, peer = config["Interface"], config["Peer"]
        if set(interface) - {"address", "privatekey", "mtu", "dns"} or set(peer) - {
                "publickey", "presharedkey", "endpoint", "allowedips", "persistentkeepalive"}:
            raise ValueError()
        address = ipaddress.IPv6Interface(interface["address"])
        if not address.ip.is_global or address.ip.is_multicast:
            raise ValueError()
        mtu = int(interface.get("mtu", "1420"))
        if not 1280 <= mtu <= 1420:
            raise ValueError()
        # IPv4 transport is explicit: a hostname may resolve to unusable IPv6.
        host, port = peer["endpoint"].rsplit(":", 1)
        endpoint = ipaddress.IPv4Address(host)
        networks = [ipaddress.IPv6Network(value.strip()) for value in peer["allowedips"].split(",")]
        if not 1 <= int(port) <= 65535 or list(ipaddress.collapse_addresses(networks)) != [ipaddress.IPv6Network("::/0")]:
            raise ValueError()
        keepalive = int(peer.get("persistentkeepalive", "25"))
        if not 1 <= keepalive <= 65535:
            raise ValueError()
        for section, field in ((interface, "privatekey"), (peer, "publickey"), (peer, "presharedkey")):
            if field == "presharedkey" and field not in section:
                continue
            value = section[field]
            if not re.fullmatch(r"[A-Za-z0-9+/]{43}=", value) or len(base64.b64decode(value, validate=True)) != 32:
                raise ValueError()
        # DNS stays on WAN so tunnel bootstrap cannot depend on the tunnel.
        result = (f"[Interface]\nAddress = {address}\nPrivateKey = {interface['privatekey']}\n"
                  f"MTU = {mtu}\nTable = off\n\n[Peer]\nPublicKey = {peer['publickey']}\n"
                  f"Endpoint = {endpoint}:{int(port)}\nAllowedIPs = ::/0\nPersistentKeepalive = {keepalive}\n")
        if "presharedkey" in peer:
            result += f"PresharedKey = {peer['presharedkey']}\n"
        return result
    except (ValueError, KeyError, configparser.Error):
        # ConfigParser errors can include the private key's source line.
        raise ValueError("invalid Route64 export: require one IPv6 Interface, one Peer, IPv4 endpoint, "
                         "AllowedIPs ::/0, MTU 1280..1420 and no hooks; see vfkit README") from None
