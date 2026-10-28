#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Export the dedicated lab's IPv6 DNS and PXE Services as DHCP Helm values."""

import argparse
import ipaddress
import json
from pathlib import Path
import subprocess
import sys
import tempfile

DNS_SERVICE = 'nico-lab-dns'
PXE_SERVICE = 'nico-pxe-external'


def service_address(service, namespace, name, release, ports):
    metadata, spec = service['metadata'], service['spec']
    annotations = metadata.get('annotations', {})
    address = ipaddress.ip_address(spec['clusterIP'])
    if (metadata['name'] != name or metadata['namespace'] != namespace or metadata.get('deletionTimestamp')
            or annotations.get('meta.helm.sh/release-name') != release
            or annotations.get('meta.helm.sh/release-namespace') != namespace
            or spec.get('type') != 'ClusterIP' or spec.get('ipFamilies') != ['IPv6']
            or spec.get('ipFamilyPolicy') != 'SingleStack' or address.version != 6
            or address.is_unspecified or address.is_multicast or address.is_loopback or address.is_link_local
            or not ports.issubset({(p.get('protocol', 'TCP'), p['port']) for p in spec['ports']})):
        raise ValueError(name + ' must be the Helm-owned single-stack IPv6 lab Service with its expected ports')
    return str(address)


def export(context, namespace):
    services = json.loads(subprocess.check_output([
        'kubectl', '--context', context, '--request-timeout=30s', '-n', namespace,
        'get', 'service', DNS_SERVICE, PXE_SERVICE, '-o', 'json',
    ], text=True, timeout=45))['items']
    if len(services) != 2 or {s['metadata']['name'] for s in services} != {DNS_SERVICE, PXE_SERVICE}:
        raise ValueError('expected exactly the DNS and PXE lab Services')
    by_name = {s['metadata']['name']: s for s in services}
    dns = service_address(by_name[DNS_SERVICE], namespace, DNS_SERVICE, DNS_SERVICE, {('UDP', 53), ('TCP', 53)})
    pxe = service_address(by_name[PXE_SERVICE], namespace, PXE_SERVICE, 'nico-pxe', {('TCP', 8080)})
    if dns == pxe:
        raise ValueError('DNS and PXE Services must have distinct addresses')
    return {'dhcp': {'v6DnsServers': dns, 'v6ProvisioningServer': pxe}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--context', required=True)
    parser.add_argument('--namespace', default='nico-system', help='Core Service namespace; default nico-system')
    parser.add_argument('--output', type=Path, required=True, help='Helm values JSON file; parent directories are created')
    args = parser.parse_args()
    if not args.context.startswith('kind-'):
        parser.error('IPv6 lab endpoint export requires a kind- context')
    try:
        values = json.dumps(export(args.context, args.namespace), indent=2) + '\n'
        output = args.output.expanduser().absolute()
        if output.exists() and output.read_text() == values:
            return 0
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(mode='w', dir=output.parent, delete=False) as stream:
            temporary = Path(stream.name)
            try:
                stream.write(values)
                stream.close()
                temporary.replace(output)
            finally:
                temporary.unlink(missing_ok=True)
        return 0
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        print(f'Could not export IPv6 lab endpoints: {error}; output was not changed', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
