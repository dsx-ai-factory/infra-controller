#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Render the same BMC lab values for Tilt and direct Helm on native Linux."""

import argparse
import copy
import json
import sys
from pathlib import Path

STACK = Path(__file__).resolve().parent.parent / 'stack'
COMPONENTS = ('nico-api', 'nico-bmc-proxy', 'nico-dhcp', 'nico-pxe')


def boot_artifact_command(architecture, ipv6):
    profile = 'ipv6' if ipv6 else 'default'
    return ['sh', '-ec',
        'for name in scout qcow-imager; do '
        f'if test -f /{architecture}/$name.efi; then '
        'actual=default; '
        f'if test -f /{architecture}/$name.network-profile; then actual=$(cat /{architecture}/$name.network-profile); fi; '
        f'test "$actual" = {profile} || {{ echo "boot artifact network profile mismatch: $name" >&2; exit 1; }}; '
        f'fi; done; cp -r /{architecture} /forge-boot-artifacts/blobs/internal/']


def render(source, site_config, architecture, namespace, boot_image,
           dhcpv6=False, ipv6_endpoints=False, ipv6_boot=False):
    if architecture not in ('aarch64', 'x86_64'):
        raise ValueError('native aarch64 or x86_64 artifacts are required')
    if not boot_image:
        raise ValueError('BMC lab requires a boot artifact image')
    if ipv6_boot and not ipv6_endpoints:
        raise ValueError('--ipv6-boot requires --ipv6-endpoints')
    if ipv6_endpoints and not dhcpv6:
        raise ValueError('--ipv6-endpoints requires --dhcpv6')
    values = copy.deepcopy(source)
    api, dhcp = values['nico-api'], values['nico-dhcp']
    api['siteConfig'] = {'enabled': True, 'nicoApiSiteConfig': site_config}
    api['extraEnv'] = []
    profile = copy.deepcopy(values['development']['bmcLab'])
    dhcp['config']['kea']['hookParameters'] = profile['dhcpHookParameters']
    if dhcpv6:
        dhcp['dhcp'] = dhcp.get('dhcp') or {}
        dhcp['dhcp']['v6Enabled'] = True
        dhcp['dhcp'].setdefault('v6ServerIdentifier', '020000000006')
    if ipv6_endpoints:
        api['externalService'] = profile['coreIPv6Service']
    pxe = profile['pxe']
    pxe['externalService'].update(enabled=ipv6_endpoints, port=8080 if ipv6_boot else 80)
    pxe['config'] = {'extraEnvData': {
        'CARBIDE_API_URL': 'https://carbide-api.forge:1079' if ipv6_boot else f'https://nico-api.{namespace}.svc.cluster.local:1079',
        'CARBIDE_PXE_URL': 'http://carbide-pxe.forge:8080' if ipv6_boot else f'http://nico-pxe.{namespace}.svc.cluster.local',
    }}
    pxe['bootArtifactContainers'][0].update(image=boot_image, command=boot_artifact_command(architecture, ipv6_boot))
    values['nico-pxe'] = pxe
    return values


def helm_values(values, images):
    if set(images) != set(COMPONENTS):
        raise ValueError('--output requires one --image component=repository:tag for each of ' + ', '.join(COMPONENTS))
    result = {}
    for name in COMPONENTS:
        repository, separator, tag = images[name].rpartition(':')
        if not separator or not repository or not tag or '/' in tag or '@' in images[name]:
            raise ValueError('use an explicit repository:tag for ' + name)
        value = copy.deepcopy({'global': values['global'], **values[name]})
        image = value.setdefault('image', {}) if name in ('nico-pxe', 'nico-bmc-proxy') else value['global']['image']
        image.update(repository=repository, tag=tag)
        result[name] = value
    for name, key in [('cert-manager', 'certManager'), ('cnpg', 'cnpgOperator'), ('postgres', 'postgres')]:
        result[name] = copy.deepcopy(values['development'][key])
    result['postgres']['cluster']['roles'] = []
    result['dependencies'] = {key: copy.deepcopy(values['development'][key])
                              for key in ('charts', 'repositories')}
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--values', default=str(STACK / 'values.yaml'), help='baseline YAML file, or - for JSON on stdin (used by Tilt)')
    parser.add_argument('--namespace', default='nico-system')
    parser.add_argument('--architecture', choices=('aarch64', 'x86_64'), required=True)
    parser.add_argument('--boot-artifact-image', required=True)
    for option in ('dhcpv6', 'ipv6-endpoints', 'ipv6-boot'):
        parser.add_argument('--' + option, action='store_true')
    parser.add_argument('--output', type=Path, help='write direct-Helm component values and images.txt into this existing directory; otherwise print combined JSON')
    parser.add_argument('--image', action='append', default=[], metavar='COMPONENT=REPOSITORY:TAG')
    args = parser.parse_args()
    if args.values == '-':
        source = json.load(sys.stdin)
    else:
        import yaml
        source = yaml.safe_load(Path(args.values).read_text())
    values = render(source, (STACK / 'bmc-lab.toml').read_text(),
                    args.architecture, args.namespace, args.boot_artifact_image,
                    args.dhcpv6, args.ipv6_endpoints, args.ipv6_boot)
    if args.output:
        images = dict(item.split('=', 1) for item in args.image)
        rendered = helm_values(values, images)
        for name, value in rendered.items():
            (args.output / (name + '.json')).write_text(json.dumps(value, indent=2) + '\n')
        (args.output / 'images.txt').write_text('\n'.join([images[name] for name in COMPONENTS] + [args.boot_artifact_image]) + '\n')
    else:
        if args.image:
            parser.error('--image requires --output')
        print(json.dumps(values))


if __name__ == '__main__':
    main()
