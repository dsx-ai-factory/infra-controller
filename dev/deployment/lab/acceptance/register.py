#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Register two owned native BMC targets and their dedicated provisioning segments."""

import argparse
import fcntl
import ipaddress
import json
import os
from pathlib import Path
import re
import time
import uuid

from core_journal import CoreJournal
from lab_io import run

HOST_INBAND = 3
UNDERLAY = 2
DPU_IGNORE = 3
RESERVE_FIRST = 10


def initial_password(secret):
    if secret.stat().st_uid != os.getuid() or secret.stat().st_mode & 0o077:
        raise ValueError('BMC password file must be owned by this user and private')
    password = secret.read_text().rstrip('\n')
    if not password or '\n' in password:
        raise ValueError('BMC password file must contain one nonempty line')
    return password


def target_inventory(context, namespace, release, cluster):
    kube = ['kubectl', '--context', context, '-n', namespace]
    deployment = json.loads(run([*kube, 'get', 'deployment', release, '-o', 'json']))
    if deployment['metadata']['labels'].get('app.kubernetes.io/name') != 'nico-bmc-target':
        raise ValueError('selected deployment is not a native BMC target')
    inventory = json.loads(run([*kube, 'exec', 'deployment/' + release, '--', 'cat', '/data/inventory.json']))
    if inventory['clusterUID'] != cluster or inventory['owner'] != namespace + '/' + release:
        raise ValueError('target inventory belongs to another cluster or release')
    return inventory


def plan_targets(inventories, profile, bmc_prefixes, owner, domain):
    families = {'ipv4': (4,), 'dual-stack': (4, 6), 'ipv6-only': (6,)}[profile]
    prefixes = [ipaddress.ip_network(p, strict=True) for p in bmc_prefixes]
    if len({p.version for p in prefixes}) != len(prefixes):
        raise ValueError('provide at most one BMC prefix per address family')
    segments = [dict(id=str(uuid.uuid4()), name=owner + '-bmc', segment_type=UNDERLAY, subdomain_id=None,
                     prefixes=[dict(prefix=str(p), gateway=None, reserve_first=0) for p in prefixes])]
    targets = []
    for release, inv in inventories.items():
        bmc = ipaddress.ip_address(inv['bmcAddress'])
        if not any(bmc in p and bmc != p.network_address and bmc != p.broadcast_address for p in prefixes if p.version == bmc.version):
            raise ValueError('BMC address must be inside the supplied dedicated prefix, excluding its endpoints')
        networks = []
        for family in families:
            address = inv.get('bridgeAddress' if family == 4 else 'bridgeAddressV6')
            if not address:
                raise ValueError(f'target {release} has no IPv{family} bridge address')
            interface = ipaddress.ip_interface(address)
            if interface.version != family or not 0 < int(interface.ip) - int(interface.network.network_address) < RESERVE_FIRST:
                raise ValueError('bridge gateway must be among the first nine reserved addresses')
            networks.append(dict(prefix=str(interface.network), gateway=str(interface.ip) if family == 4 else None,
                                 reserve_first=RESERVE_FIRST))
        segment = dict(id=str(uuid.uuid4()), name=owner + '-' + release, segment_type=HOST_INBAND,
                       subdomain_id=domain, prefixes=networks)
        segments.append(segment)
        targets.append(dict(release=release, inventory=inv, expected_id=str(uuid.uuid4()),
                            bmc_mac=f'02:00:00:{inv["instanceIndex"]:02x}:00:01', name=owner + '-' + release))
    all_prefixes = [ipaddress.ip_network(p['prefix']) for s in segments for p in s['prefixes']]
    if any(a.version == b.version and a.overlaps(b) for i, a in enumerate(all_prefixes) for b in all_prefixes[i + 1:]):
        raise ValueError('BMC and guest prefixes must be distinct and nonoverlapping')
    for field in ('bmcAddress', 'mac', 'serial', 'instanceIndex'):
        if len({t['inventory'][field] for t in targets}) != 2:
            raise ValueError('target identities must be distinct: ' + field)
    return segments, targets


class Registration(CoreJournal):
    def verify_targets(self):
        self.verify_cluster()
        for target in self.state['targets']:
            current = target_inventory(self.state['context'], self.state['target_namespace'], target['release'], self.state['cluster'])
            if current != target['inventory']:
                raise ValueError('target identity or configuration changed; refusing registration')

    def find_segment(self, wanted):
        segments = self.admin('network-segment', 'show', read=True)['network_segments']
        found = None
        desired = [ipaddress.ip_network(p['prefix']) for p in wanted['prefixes']]
        for segment in segments:
            if segment.get('deleted'):
                continue
            config = segment['config']
            if segment['id'] != wanted['id']:
                if any(ipaddress.ip_network(p['prefix']).overlaps(prefix) for p in config['prefixes']
                       for prefix in desired if ipaddress.ip_network(p['prefix']).version == prefix.version):
                    raise ValueError('requested prefix overlaps a segment outside this registration')
                continue
            actual = [{key: p.get(key) for key in ('prefix', 'gateway', 'reserve_first')} for p in config['prefixes']]
            if (segment['metadata']['name'] != wanted['name'] or config['segment_type'] != wanted['segment_type']
                    or config.get('vpc_id') or config.get('subdomain_id') != wanted['subdomain_id']
                    or config['mtu'] != 1500 or config.get('infer_slaac_eui64_addresses', False)
                    or sorted(actual, key=lambda p: p['prefix']) != sorted(wanted['prefixes'], key=lambda p: p['prefix'])):
                raise ValueError('owned segment configuration changed')
            found = segment
        return found

    def find_expected(self, target):
        expected = self.admin('em', 'show', read=True)['expected_machines']
        inv = target['inventory']
        matches = [e for e in expected if e['id']['value'] == target['expected_id']
                   or e['bmc_mac_address'].lower() == target['bmc_mac'] or e['chassis_serial_number'] == inv['serial']
                   or e.get('bmc_ip_address') == inv['bmcAddress']
                   or any(i['mac_address'].lower() == inv['mac'].lower() for i in e['interfaces'])]
        if not matches:
            return None
        if len(matches) != 1:
            raise ValueError('conflicting expected-machine identities')
        e = matches[0]
        interfaces = e['interfaces']
        if (e['id']['value'] != target['expected_id'] or e['bmc_mac_address'].lower() != target['bmc_mac']
                or e['chassis_serial_number'] != inv['serial'] or e.get('bmc_ip_address') != inv['bmcAddress']
                or e['metadata']['name'] != target['name'] or e['metadata']['description'] != self.state['owner']
                or e['dpu_mode'] != DPU_IGNORE or e.get('is_dpf_enabled', e.get('dpf_enabled')) is not False
                or e.get('default_pause_ingestion_and_poweron') or e.get('sku_id') or e.get('rack_id')
                or e.get('host_lifecycle_profile') or e.get('fallback_dpu_serial_numbers') or len(interfaces) != 1):
            raise ValueError('expected machine is unowned or changed; credentials will not be overwritten')
        interface = interfaces[0]
        if (interface['mac_address'].lower() != inv['mac'].lower() or interface['primary'] is not True
                or interface['network_segment_type'] != HOST_INBAND
                or any(interface.get(k) for k in ('fixed_ip', 'fixed_mask', 'fixed_gateway'))
                or interface.get('role') not in (None, 'host') or interface.get('ip_allocation') not in (None, 'dynamic')):
            raise ValueError('expected host interface policy changed')
        return e

    def segments_ready(self):
        for segment in self.state['segments']:
            current = self.find_segment(segment)
            if current is None:
                raise ValueError('owned segment disappeared; inspect Core before resuming')
            if json.loads(current['status']['lifecycle']['state'])['state'] != 'ready':
                return False
        return True

    def apply(self):
        self.verify_targets()
        # Validate all recorded identities and existing prefixes before any write.
        for segment in self.state['segments']:
            self.find_segment(segment)
        for target in self.state['targets']:
            self.find_expected(target)
        for segment in self.state['segments']:
            command = ['network-segment', 'create', '--id', segment['id'], '--name', segment['name'],
                       '--segment-type', 'host-inband' if segment['segment_type'] == HOST_INBAND else 'underlay']
            if segment['subdomain_id']:
                command += ['--subdomain-id', segment['subdomain_id'], '--reserve-first', str(RESERVE_FIRST)]
            for prefix in segment['prefixes']:
                command += ['--prefix', prefix['prefix']]
                if prefix['gateway']:
                    command += ['--gateway', prefix['gateway']]
            self.ensure('segment-' + segment['id'], lambda: self.find_segment(segment), command)
        # Wait for segment controllers before static BMC preallocation.
        deadline = time.monotonic() + self.state['timeout']
        while not self.segments_ready():
            if time.monotonic() >= deadline:
                raise TimeoutError('segments are not Ready; resume after inspecting Core')
            time.sleep(5)
        for target in self.state['targets']:
            inv = target['inventory']
            key = 'expected-' + target['expected_id']
            command = []
            if not self.find_expected(target) and not self.state.get(key + '_intent'):
                password = initial_password(Path(self.state['bmc_password_file']))
                command = ['em', 'add', '--id', target['expected_id'], '--bmc-mac-address', target['bmc_mac'],
                           '--bmc-username', self.state['bmc_username'], '--bmc-password', password,
                           '--chassis-serial-number', inv['serial'], '--bmc-ip-address', inv['bmcAddress'],
                           '--dpu-policy', 'ignore', '--dpf-enabled', 'false', '--meta-name', target['name'],
                           '--meta-description', self.state['owner'], '--interfaces', json.dumps([dict(
                               mac_address=inv['mac'], role='host', primary=True, network_segment_type=HOST_INBAND)])]
            self.ensure(key, lambda: self.find_expected(target), command)
        self.state['complete'] = True
        self.checkpoint()
        print(json.dumps(dict(context=self.state['context'], owner=self.state['owner'],
                              targets=[dict(release=t['release'], expected_id=t['expected_id']) for t in self.state['targets']]), indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state-dir', type=Path, required=True)
    parser.add_argument('--resume', action='store_true')
    parser.add_argument('--context')
    parser.add_argument('--core-namespace', help='Core namespace; default nico-system')
    parser.add_argument('--namespace', help='target namespace; default bmc-lab')
    parser.add_argument('--target', action='append', help='exactly two native target Helm releases')
    parser.add_argument('--network', choices=('ipv4', 'dual-stack', 'ipv6-only'), help='guest prefixes to register; default ipv4')
    parser.add_argument('--bmc-prefix', action='append', help='dedicated managed BMC Service prefix; at most one per family')
    parser.add_argument('--domain', help='existing DNS domain; default local.nico')
    parser.add_argument('--bmc-username', help='initial BMC username; default root')
    parser.add_argument('--bmc-password-file', type=Path, help='private one-line initial BMC password file')
    parser.add_argument('--timeout', type=int, help='segment readiness timeout in seconds, 60..1800; default 300')
    args = parser.parse_args()
    creation = [v for k, v in vars(args).items() if k not in ('state_dir', 'resume')]
    if args.resume and any(v is not None for v in creation):
        parser.error('--resume uses recorded configuration only')
    if not args.resume and (not args.context or not args.context.startswith('kind-') or not args.target
            or len(args.target) != 2 or len(set(args.target)) != 2 or not args.bmc_prefix or not args.bmc_password_file
            or any(not re.fullmatch('[a-z0-9][a-z0-9-]{0,51}[a-z0-9]|[a-z0-9]', r) for r in args.target)
            or not 60 <= (args.timeout if args.timeout is not None else 300) <= 1800):
        parser.error('require a kind context, two distinct releases, BMC prefixes/password file and timeout 60..1800')
    os.umask(0o077)
    directory = args.state_dir.expanduser().resolve()
    if not args.resume:
        initial_password(args.bmc_password_file.expanduser().resolve())
        cluster = json.loads(run(['kubectl', '--context', args.context, 'get', 'namespace', 'kube-system', '-o', 'json']))['metadata']['uid']
        inventories = {r: target_inventory(args.context, args.namespace or 'bmc-lab', r, cluster) for r in args.target}
        architectures = {i['architecture'] for i in inventories.values()}
        if len(architectures) != 1:
            raise ValueError('target architectures differ')
        state = dict(version=1, context=args.context, namespace=args.core_namespace or 'nico-system', cluster=cluster,
                     target_namespace=args.namespace or 'bmc-lab', owner='lab-' + uuid.uuid4().hex,
                     architecture=next(iter(architectures)), timeout=args.timeout or 300,
                     bmc_username=args.bmc_username or 'root', bmc_password_file=str(args.bmc_password_file.expanduser().resolve()))
        journal = Registration(directory, state)
        journal.verify_cluster()
        domains = journal.admin('domain', 'show', read=True)['domains']
        domains = [d for d in domains if d['name'] == (args.domain or 'local.nico') and not d.get('deleted')]
        if len(domains) != 1:
            raise ValueError('select exactly one existing live DNS domain')
        state['segments'], state['targets'] = plan_targets(inventories, args.network or 'ipv4', args.bmc_prefix, state['owner'], domains[0]['id'])
        directory.mkdir(mode=0o700, exist_ok=False)
    with (directory / 'register.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if args.resume:
            state = json.loads((directory / 'state.json').read_text())
        journal = Registration(directory, state)
        if not args.resume:
            journal.checkpoint()
        journal.apply()


if __name__ == '__main__':
    main()
