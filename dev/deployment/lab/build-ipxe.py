#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Build the pinned native iPXE bootloader with NICo's ordinary boot script."""

import argparse
import platform
import shutil
import subprocess
from pathlib import Path

REPO = Path(__file__).resolve().parents[3]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True, help='new or empty output directory')
    args = parser.parse_args()
    architecture = platform.machine()
    if platform.system() != 'Linux' or architecture not in ('aarch64', 'x86_64'):
        parser.error('build on native ARM64 or AMD64 Linux')
    source = REPO / 'pxe/ipxe/upstream'
    if not (source / 'src/Makefile').is_file():
        parser.error('initialize the pinned pxe/ipxe/upstream submodule first')
    output = args.output.resolve()
    if output.exists() and any(output.iterdir()):
        parser.error('--output must be new or empty; prior artifacts are never overwritten')
    work = output / 'build'
    work.mkdir(parents=True)
    # Copy source rather than patching the user's submodule or sharing build products.
    shutil.copytree(source / 'src', work / 'src', ignore=shutil.ignore_patterns('bin*'))
    local = REPO / 'pxe/ipxe/local'
    for header in local.glob('*.h'):
        shutil.copy2(header, work / 'src/config/local' / header.name)
    patch = local / '0004-http-ipv6-source.patch'
    applied = subprocess.run(['patch', '--batch', '--dry-run', '--reverse', '-p1', '-i', str(patch)],
                             cwd=work, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    if applied.returncode:
        subprocess.run(['patch', '--batch', '-p1', '-i', str(patch)], cwd=work, check=True)
    target = 'arm64' if architecture == 'aarch64' else 'x86_64'
    subprocess.run(['make', '-j2', f'bin-{target}-efi/snponly.efi',
                    f'EMBED={local / "embed.ipxe"}'], cwd=work / 'src', check=True)
    destination = output / architecture
    destination.mkdir()
    shutil.copy2(work / f'src/bin-{target}-efi/snponly.efi', destination / 'ipxe.efi')
    (destination / 'ipxe.efi').chmod(0o644)
    print(f'iPXE payload: {destination / "ipxe.efi"}')


if __name__ == '__main__':
    main()
