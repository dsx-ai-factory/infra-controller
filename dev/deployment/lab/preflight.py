#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Check native KVM access and an explicit incremental Linux host budget."""

import argparse
import fcntl
import json
import os
from pathlib import Path
import platform
import sys

# Linux UAPI: include/uapi/linux/kvm.h. These ioctls are shared by both architectures.
KVM_GET_API_VERSION = 0xAE00
KVM_CREATE_VM = 0xAE01
KVM_CHECK_EXTENSION = 0xAE03
KVM_CAP_ARM_VM_IPA_SIZE = 165
KVM_API_VERSION = 12
ARCHITECTURES = {'x86_64': 'amd64', 'aarch64': 'arm64'}


def check_kvm(architecture):
    device = os.open('/dev/kvm', os.O_RDWR | os.O_CLOEXEC)
    try:
        version = fcntl.ioctl(device, KVM_GET_API_VERSION, 0)
        if version != KVM_API_VERSION:
            raise ValueError(f'unsupported KVM API version: {version}')
        vm_type = 0
        if architecture == 'arm64':
            ipa_bits = fcntl.ioctl(device, KVM_CHECK_EXTENSION, KVM_CAP_ARM_VM_IPA_SIZE)
            # Nested ARM64 hosts can support fewer than the default 40 IPA bits.
            if ipa_bits:
                vm_type = min(40, ipa_bits)
        vm = fcntl.ioctl(device, KVM_CREATE_VM, vm_type)
        os.close(vm)
        return version
    finally:
        os.close(device)


def host_capacity(storage_path, meminfo=Path('/proc/meminfo')):
    if not storage_path.is_dir():
        raise ValueError('--storage-path must name an existing directory on the target filesystem')
    fields = dict(line.split(':', 1) for line in meminfo.read_text().splitlines())
    memory = fields.get('MemAvailable', '').split()
    if len(memory) != 2 or memory[1] != 'kB' or not memory[0].isdigit():
        raise ValueError('/proc/meminfo has no valid MemAvailable value')
    storage = os.statvfs(storage_path)
    return {
        'memory_mib': int(memory[0]) // 1024,
        'cpus': len(os.sched_getaffinity(0)),
        'disk_gib': storage.f_bavail * storage.f_frsize // (1024 ** 3),
    }


def hugepage_capacity(root=Path('/sys/kernel/mm/hugepages/hugepages-2048kB')):
    """Unreserved 2 MiB pages, expressed in MiB; this does not reserve them."""
    try:
        free = int((root / 'free_hugepages').read_text())
        reserved = int((root / 'resv_hugepages').read_text())
    except (OSError, ValueError) as error:
        raise ValueError('cannot read the 2 MiB huge-page pool; configure it on the Linux host') from error
    if free < 0 or reserved < 0:
        raise ValueError('invalid 2 MiB huge-page pool counters')
    return max(0, free - reserved) * 2


def capacity_errors(available, requested):
    return [f'insufficient {resource}: requested {amount}, available {available[resource]}'
            for resource, amount in requested.items() if available[resource] < amount]


def positive_integer(value):
    try:
        number = int(value)
    except ValueError:
        raise argparse.ArgumentTypeError('must be a positive integer') from None
    if number <= 0:
        raise argparse.ArgumentTypeError('must be a positive integer')
    return number


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest='scope', required=True)
    commands.add_parser('kvm', help='probe KVM in the current execution namespace; no capacity check')
    host = commands.add_parser('host', help='probe KVM and additional capacity on the Linux host')
    host.add_argument('--storage-path', required=True, type=Path,
                      help='existing directory on the filesystem that will hold the lab data')
    host.add_argument('--memory-mib', required=True, type=positive_integer,
                      help='additional available memory required, in MiB; include runtime overhead')
    host.add_argument('--cpus', required=True, type=positive_integer,
                      help='required CPUs in the invoking process affinity mask; not idle CPUs')
    host.add_argument('--disk-gib', required=True, type=positive_integer,
                      help='additional available disk space required, in GiB')
    host.add_argument('--hugepages-mib', type=positive_integer,
                      help='additional unreserved 2 MiB HugeTLB capacity; ordinary RAM is budgeted separately')
    args = parser.parse_args(argv)
    report = {'scope': args.scope, 'architecture': None, 'ready': False, 'errors': []}
    try:
        if platform.system() != 'Linux' or platform.machine() not in ARCHITECTURES:
            raise ValueError('requires native Linux AMD64 or ARM64; run on the lab runtime host')
        report['architecture'] = ARCHITECTURES[platform.machine()]
        report['kvm_api_version'] = check_kvm(report['architecture'])
        if args.scope == 'host':
            try:
                storage_path = args.storage_path.resolve(strict=True)
            except RuntimeError as error:
                # Python before 3.13 reports a symlink loop as RuntimeError.
                raise ValueError(str(error)) from error
            available = host_capacity(storage_path)
            requested = {key: getattr(args, key) for key in ('memory_mib', 'cpus', 'disk_gib')}
            if args.hugepages_mib is not None:
                available['hugepages_mib'] = hugepage_capacity()
                requested['hugepages_mib'] = args.hugepages_mib
            report.update(storage_path=str(storage_path), available=available, requested=requested)
            report['errors'].extend(capacity_errors(report['available'], report['requested']))
    except (OSError, ValueError) as error:
        report['errors'].append(str(error))
    report['ready'] = not report['errors']
    print(json.dumps(report, indent=2))
    return 0 if report['ready'] else 1


if __name__ == '__main__':
    sys.exit(main())
