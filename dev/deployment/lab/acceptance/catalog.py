#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Register an owned native OS image and flat VPC in an isolated kind lab."""

import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import urllib.parse
import uuid

from lab_io import run
from core_journal import CoreJournal

HERE = Path(__file__).resolve().parent
FLAT = 6
IMAGE_READY = 3


def origin(value):
    try:
        parsed = urllib.parse.urlsplit(value)
        port = parsed.port
        if (parsed.scheme not in ('http', 'https') or not parsed.hostname or parsed.username is not None
                or parsed.password is not None or parsed.path not in ('', '/')
                or '?' in value or '#' in value or any(c.isspace() for c in value)
                or port == 0):
            raise ValueError('invalid origin')
    except ValueError as error:
        raise argparse.ArgumentTypeError('use an HTTP(S) origin with a valid port and without path, credentials, query or fragment') from error
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, '', '', ''))


def artifact(value):
    architecture = value.get('architecture')
    directory = {'arm64': 'aarch64', 'amd64': 'x86_64'}.get(architecture)
    if (not directory or value.get('artifact_directory') != directory
            or value.get('pxe_path') != '/public/blobs/internal/' + directory + '/ubuntu.img'
            or not re.fullmatch('[0-9a-f]{64}', value.get('sha256', ''))):
        raise ValueError('expected a native OS builder manifest with its exact PXE path and SHA256')
    return {key: value[key] for key in ('architecture', 'artifact_directory', 'pxe_path', 'sha256', 'image_id', 'boot_artifact_image_id')}


class Catalog(CoreJournal):
    def find_os(self):
        images = self.admin('os-image', 'show', '--tenant-org-id', self.state['owner'], read=True)
        matches = [i for i in images if i['attributes']['id']['value'] == self.state['os_id']]
        if not matches:
            return None
        if len(matches) != 1:
            raise ValueError('OS catalog returned duplicate IDs')
        image = matches[0]
        expected = dict(source_url=self.state['url'], digest=self.state['artifact']['sha256'],
                        tenant_organization_id=self.state['owner'], name=self.state['owner'] + '-os',
                        description=self.state['owner'], boot_disk='/dev/vda', create_volume=False,
                        auth_type=None, auth_token=None, rootfs_id=None, rootfs_label=None,
                        bootfs_id=None, efifs_id=None, capacity=None)
        if any(image['attributes'].get(key) != value for key, value in expected.items()) or image['status'] != IMAGE_READY:
            raise ValueError('owned OS image changed or is not Ready; refusing adoption')
        return self.state['os_id']

    def find_vpc(self):
        vpcs = self.admin('vpc', 'show', read=True)['vpcs']
        matches = [v for v in vpcs if v['metadata']['name'] == self.state['owner'] + '-vpc'
                   or v['id'] == self.state.get('vpc_id')]
        if not matches:
            return None
        if len(matches) != 1:
            raise ValueError('multiple VPCs match the recorded catalog intent')
        vpc = matches[0]
        expected = dict(tenant_organization_id=self.state['owner'], network_virtualization_type=FLAT,
                        tenant_keyset_id=None, network_security_group_id=None, slaac_enabled=False)
        if (vpc['metadata']['name'] != self.state['owner'] + '-vpc' or vpc['metadata']['description'] != self.state['owner']
                or vpc.get('deleted') or any(vpc['config'].get(key) != value for key, value in expected.items())
                or self.state.get('vpc_id') not in (None, vpc['id'])):
            raise ValueError('VPC identity, owner or networking changed; refusing adoption')
        return str(uuid.UUID(vpc['id']))

    def apply(self):
        self.verify_cluster()
        self.ensure('os', self.find_os, ['os-image', 'create', '--id', self.state['os_id'], '--url', self.state['url'],
                                      '--digest', self.state['artifact']['sha256'], '--tenant-org-id', self.state['owner'],
                                      '--name', self.state['owner'] + '-os', '--description', self.state['owner'], '--boot-disk', '/dev/vda'])
        # Core generates the VPC ID. Never retry an ambiguous create: its name is
        # not a unique idempotency key, so recover only a unique matching result.
        self.state['vpc_id'] = self.ensure('vpc', self.find_vpc,
                                          ['vpc', 'create', '--name', self.state['owner'] + '-vpc',
                                           '--description', self.state['owner'], '--org-id', self.state['owner'],
                                           '--virtualization-type', 'flat'])
        self.state['complete'] = True
        self.checkpoint()
        print(json.dumps({key: self.state[key] for key in ('context', 'cluster', 'owner', 'os_id', 'vpc_id')}, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state-dir', required=True, type=Path, help='new private journal directory, or existing directory with --resume')
    parser.add_argument('--resume', action='store_true', help='verify/recover only the recorded creation intents in the same cluster')
    parser.add_argument('--context', help='explicit kind context for a new catalog')
    parser.add_argument('--core-namespace', '--namespace', dest='core_namespace',
                        help='Core namespace (default nico-system; --namespace is a compatibility alias)')
    parser.add_argument('--artifact-manifest', type=Path, help='build-os-artifact.py build-info.json, already served by PXE')
    parser.add_argument('--pxe-origin', type=origin, help='guest-reachable HTTP(S) origin for the selected PXE profile')
    args = parser.parse_args()
    if args.resume:
        if args.context or args.core_namespace or args.artifact_manifest or args.pxe_origin:
            parser.error('--resume uses all configuration from its retained journal')
    elif not args.context or not args.context.startswith('kind-') or not args.artifact_manifest or not args.pxe_origin:
        parser.error('new catalogs require --context kind-..., --artifact-manifest and --pxe-origin')
    manifest = artifact(json.loads(args.artifact_manifest.read_text())) if not args.resume else None
    os.umask(0o077)
    directory = args.state_dir.expanduser().resolve()
    if not args.resume:
        cluster = json.loads(run(['kubectl', '--context', args.context, 'get', 'namespace', 'kube-system', '-o', 'json']))['metadata']['uid']
        state = dict(version=1, context=args.context, namespace=args.core_namespace or 'nico-system', cluster=cluster,
                     owner='lab-' + uuid.uuid4().hex, os_id=str(uuid.uuid4()), artifact=manifest,
                     url=args.pxe_origin + manifest['pxe_path'])
        Catalog(directory, state).verify_cluster()
        directory.mkdir(mode=0o700, parents=False, exist_ok=False)
    with (directory / 'catalog.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.resume:
            state = json.loads((directory / 'state.json').read_text())
        catalog = Catalog(directory, state)
        if not args.resume:
            catalog.checkpoint()
        catalog.apply()


if __name__ == '__main__':
    main()
