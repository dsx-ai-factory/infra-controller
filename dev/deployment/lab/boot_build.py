# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Shared mkosi staging for the lab's Scout and disk imager."""

import hashlib
import shutil


def stage_base(repo, work):
    pxe = repo / 'pxe'
    shutil.copy2(pxe / 'mkosi.conf', work / 'mkosi.conf')
    shutil.copytree(pxe / 'mkosi.extra', work / 'mkosi.extra', symlinks=True)


def stage_profile(repo, work, profile, ipv6_boot):
    destination = work / 'mkosi.profiles' / profile
    shutil.copytree(repo / 'pxe/mkosi.profiles' / profile, destination, symlinks=True)
    extra = destination / 'mkosi.extra'
    # IPv6 boot uses DHCP DNS; the default profile needs .local routing.
    if not ipv6_boot:
        resolved = extra / 'etc/systemd/resolved.conf.d'
        resolved.mkdir(parents=True, exist_ok=True)
        (resolved / '90-nico-lab.conf').write_text('[Resolve]\nDNS=10.96.0.10\nDomains=~cluster.local\n')
    return destination


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()
