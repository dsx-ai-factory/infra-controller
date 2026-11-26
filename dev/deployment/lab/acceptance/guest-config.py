#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Print a Core OS configuration for a native Ubuntu networkd lab guest."""

import argparse
import json
from pathlib import Path
import re
import subprocess
import uuid

NETWORK_FILE = '/etc/systemd/network/05-nico-lab.network'
GENERATION_FILE = '/etc/nico-lab-generation'
PROFILES = {'ipv4': ('ipv4', 'no', 'no'), 'dual-stack': ('yes', 'yes', 'ipv6'),
            'ipv6-only': ('ipv6', 'yes', 'ipv6')}


def mac_address(value):
    if not re.fullmatch(r'(?:[0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}', value):
        raise argparse.ArgumentTypeError('use a colon-separated Ethernet MAC')
    address = bytes.fromhex(value.replace(':', ''))
    if address == bytes(6) or address[0] & 1:
        raise argparse.ArgumentTypeError('the guest MAC must be nonzero and unicast')
    return value.lower()


def configuration(image, mac, key, generation, profile):
    dhcp, ra, link_local = PROFILES[profile]
    network = (f'[Match]\nMACAddress={mac}\n\n[Network]\nDHCP={dhcp}\n'
               f'IPv6AcceptRA={ra}\nLinkLocalAddressing={link_local}\n')
    if profile != 'ipv4':
        network += '\n[DHCPv6]\nWithoutRA=solicit\n'
    cloud_config = {
        'users': [{'name': 'lab', 'sudo': 'ALL=(ALL) NOPASSWD:ALL', 'shell': '/bin/bash',
                   'ssh_authorized_keys': [key]}],
        'write_files': [
            {'path': GENERATION_FILE, 'permissions': '0644', 'content': generation + '\n'},
            {'path': NETWORK_FILE, 'permissions': '0644', 'content': network},
        ],
        'runcmd': [['networkctl', 'reload']],
    }
    # JSON is valid YAML, including cloud-init's cloud-config document format.
    return {'phone_home_enabled': False, 'run_provisioning_instructions_on_every_boot': False,
            'variant': {'OsImageId': {'value': str(image)}},
            'user_data': '#cloud-config\n' + json.dumps(cloud_config, indent=2, ensure_ascii=False) + '\n'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--os-image-id', required=True, type=uuid.UUID, help='registered native OS image UUID')
    parser.add_argument('--mac', required=True, type=mac_address, help='target guest NIC MAC')
    parser.add_argument('--public-key', required=True, type=Path, help='one OpenSSH public key file')
    parser.add_argument('--generation', required=True, help='nonempty printable disk generation marker')
    parser.add_argument('--network', choices=PROFILES, default='ipv4', help='installed OS networking (default: ipv4)')
    args = parser.parse_args()
    if not args.generation.strip() or not args.generation.isprintable():
        parser.error('--generation must be nonempty, not only whitespace, and contain only printable characters')
    try:
        key = args.public_key.read_text().strip()
        # Reject private-key files before invoking any key parser or writing output.
        if not key.isprintable() or not key.startswith(('ssh-', 'ecdsa-', 'sk-')):
            parser.error('--public-key must contain one printable OpenSSH public key')
        subprocess.run(['ssh-keygen', '-l', '-f', str(args.public_key)], check=True,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=10)
    except (OSError, UnicodeError, subprocess.SubprocessError):
        parser.error('could not validate --public-key with ssh-keygen')
    print(json.dumps(configuration(args.os_image_id, args.mac, key, args.generation, args.network), indent=2))


if __name__ == '__main__':
    main()
