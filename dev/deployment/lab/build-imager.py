#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Build the native NICo disk imager for the isolated kind guest lab."""

import argparse
import hashlib
import json
import os
import platform
import shutil
import subprocess
from pathlib import Path

from boot_build import sha256, stage_base, stage_profile as stage_mkosi_profile

REPO = Path(__file__).resolve().parents[3]


def stage_profile(repo, work, architecture, ipv6_boot=False):
    pxe = repo / 'pxe'
    profile = 'qcow-imager-aarch64' if architecture == 'aarch64' else 'qcow-imager'
    stage_base(repo, work)
    destination = stage_mkosi_profile(repo, work, profile, ipv6_boot)
    extra = destination / 'mkosi.extra'
    (extra / 'opt/forge').mkdir(parents=True, exist_ok=True)
    shutil.copy2(pxe / 'common_files/disk_imaging.sh', extra / 'opt/forge')
    return profile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True, help='new or empty output directory')
    parser.add_argument('--ipv6-boot', action='store_true',
                        help='use DHCP-provided DNS with the lab IPv6 provisioning endpoints')
    args = parser.parse_args()
    architecture = platform.machine()
    if platform.system() != 'Linux' or os.geteuid() != 0 or architecture not in ('aarch64', 'x86_64'):
        parser.error('run as root on native ARM64 or AMD64 Linux in the Scout builder container')
    mkosi = REPO / 'pxe/mkosi/bin/mkosi'
    if not mkosi.is_file():
        parser.error('initialize the pinned pxe/mkosi submodule first')
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error('--output must be new or empty; prior artifacts are never overwritten')
    work = output / 'build'
    work.mkdir(parents=True)
    profile = stage_profile(REPO, work, architecture, args.ipv6_boot)
    subprocess.run([str(mkosi), '--profile', profile, f'--output-dir={work / "output"}',
                    f'--package-cache-directory={work / "package-cache"}', 'build'], cwd=work, check=True)
    source = work / 'output/qcow-imager.efi'
    if source.stat().st_size == 0:
        raise ValueError('mkosi produced an empty imager')
    payload = output / architecture
    payload.mkdir()
    image = payload / source.name
    shutil.copy2(source, image)
    image.chmod(0o644)
    (payload / 'qcow-imager.network-profile').write_text('ipv6\n' if args.ipv6_boot else 'default\n')
    (output / 'build-info.json').write_text(json.dumps({
        'architecture': architecture, 'profile': profile, 'ipv6_boot': args.ipv6_boot, 'payload_sha256': {image.name: sha256(image)},
        'installer_sha256': hashlib.sha256((REPO / 'pxe/common_files/disk_imaging.sh').read_bytes()).hexdigest(),
    }, indent=2) + '\n')
    print(f'Imager payload: {image}')


if __name__ == '__main__':
    main()
