#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Exercise two explicitly selected, admitted native lab targets through instance lifecycles."""

import argparse
import fcntl
import ipaddress
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import time
import uuid

from lab_io import run, save

HERE = Path(__file__).resolve().parent
ADMIN = HERE.parents[1] / 'core-admin.sh'


def matching_instance(instance, target, name):
    if instance.get('machine_id') != target['machine'] or instance.get('metadata', {}).get('name') != name + '_0':
        raise ValueError('instance identity does not match the recorded allocation intent')
    return str(uuid.UUID(instance['id']))


def validate_target(target, inventory, machine, cluster):
    if inventory['clusterUID'] != cluster or inventory['owner'] != target['namespace'] + '/' + target['release']:
        raise ValueError('target inventory belongs to a different cluster or release')
    if machine['id'] != target['machine'] or ipaddress.ip_address(machine['bmc_info']['ip']) != ipaddress.ip_address(inventory['bmcAddress']):
        raise ValueError('Core machine does not match the selected target BMC')
    if machine['bmc_info']['mac'].lower() != f'02:00:00:{inventory["instanceIndex"]:02x}:00:01':
        raise ValueError('Core BMC MAC does not match the selected target')


def selected_addresses(instance, profile, inventory):
    interfaces = instance['status']['network']['interfaces']
    addresses = [ipaddress.ip_address(a) for interface in interfaces for a in interface['addresses']]
    wanted = (4,) if profile == 'ipv4' else (6,) if profile == 'ipv6-only' else (4, 6)
    result = {}
    for version in wanted:
        network = ipaddress.ip_interface(inventory['bridgeAddress' if version == 4 else 'bridgeAddressV6'])
        candidates = [a for a in addresses if a.version == version and a in network.network and a != network.ip]
        if len(candidates) != 1:
            raise ValueError(f'expected one allocated IPv{version} address in the target subnet')
        result[str(version)] = str(candidates[0])
    return result


class Lifecycle:
    def __init__(self, directory, state):
        self.directory, self.state = directory, state
        self.environment = dict(os.environ, LOCAL_DEV_KUBE_CONTEXT=state['context'],
                                LOCAL_DEV_NAMESPACE=state['core_namespace'], FORGE_CLIENT_REQUEST_TIMEOUT_SECS='30')

    def checkpoint(self):
        save(self.directory / 'state.json', self.state)

    def admin(self, *args, json_output=False, log_path=None):
        command = ['bash', str(ADMIN)] + (['-f', 'json'] if json_output else []) + list(args)
        output = run(command, env=self.environment, log_path=log_path)
        return json.loads(output) if json_output else output

    def mutation(self, *args, log_path=None):
        return self.admin('--cloud-unsafe-op', 'Validate owned native lab lifecycle', *args, log_path=log_path)

    def kube(self, target, *args):
        return run(['kubectl', '--context', self.state['context'], '-n', target['namespace'], *args],
                   errors='replace' if '/data/logs/console.log' in args else 'strict')

    def verify_ownership(self, targets=None):
        if self.state['version'] != 1 or not self.state['context'].startswith('kind-'):
            raise ValueError('unsupported journal version or non-kind context')
        cluster = json.loads(run(['kubectl', '--context', self.state['context'], 'get', 'namespace', 'kube-system', '-o', 'json']))['metadata']['uid']
        if cluster != self.state['cluster']:
            raise ValueError('cluster identity changed; refusing retained-state adoption')
        for target in self.state['targets'] if targets is None else targets:
            deployment = json.loads(self.kube(target, 'get', 'deployment', target['release'], '-o', 'json'))
            if deployment['metadata']['labels'].get('app.kubernetes.io/name') != 'nico-bmc-target':
                raise ValueError('selected deployment is not a native BMC target')
            inventory = json.loads(self.kube(target, 'exec', 'deployment/' + target['release'], '--', 'cat', '/data/inventory.json'))
            machine = self.admin('machine', 'show', target['machine'], json_output=True)
            validate_target(target, inventory, machine, cluster)
            if target.get('inventory') and target['inventory'] != inventory:
                raise ValueError('target identity or configuration changed since recording the run')
            target['inventory'] = inventory

    def preflight_network(self):
        if self.state['network'] != 'ipv4' and any(not t['inventory'].get('bridgeAddressV6') for t in self.state['targets']):
            raise ValueError('IPv6 guest profile requires an IPv6 bridge on both targets')

    def cleanup(self):
        failures = []
        for target in self.state['targets']:
            if not target.get('intent'):
                continue
            try:
                self.verify_ownership([target])
                self.release(target)
            except Exception as error:
                failures.append(target['release'] + ': ' + str(error))
        if failures:
            self.diagnostics()
            raise RuntimeError('cleanup incomplete: ' + '; '.join(failures))

    def wait(self, label, operation):
        deadline = time.monotonic() + self.state['timeout']
        last = ''
        while time.monotonic() < deadline:
            try:
                result = operation()
                if result:
                    print('PASS ' + label, flush=True)
                    return result
            except (RuntimeError, subprocess.SubprocessError) as error:
                last = str(error)
            time.sleep(10)
        raise TimeoutError(label + ': ' + last)

    def ready(self, target):
        machine = self.admin('machine', 'show', target['machine'], json_output=True)
        if machine['state'] != 'Ready':
            return False
        self.admin('scout-stream', 'ping', target['machine'])
        return True

    def instance(self, target):
        current = self.admin('instance', 'show', target['machine'], json_output=True)
        identity = matching_instance(current, target, target['intent'])
        if target.get('instance') and target['instance'] != identity:
            raise ValueError('allocated instance was replaced; refusing to operate')
        return current

    def host_keys(self, target):
        return self.directory / f'known-hosts-{self.state["generation"]}-{target["release"]}'

    def ssh(self, target, command, known_hosts=None, address=None):
        addresses = target['addresses']
        address = address or addresses.get('6', addresses.get('4'))
        return run([sys.executable, str(HERE.parent / 'ssh-guest.py'), '--context', self.state['context'],
                    '--namespace', target['namespace'], '--release', target['release'], '--address', address,
                    '--identity', str(self.directory / 'id_ed25519'), '--known-hosts',
                    str(known_hosts if known_hosts is not None else self.host_keys(target)), '--', command])

    def allocate(self, target):
        self.wait(target['release'] + ' Ready/Scout', lambda: self.ready(target))
        target['intent'] = self.state['run'] + '-' + str(self.state['generation']) + '-' + target['release']
        target.pop('instance', None)
        target.pop('boot_id', None)
        self.checkpoint()  # Persist intent before a potentially successful but timed-out allocation.
        config = json.loads(run([sys.executable, str(HERE / 'guest-config.py'), '--os-image-id', self.state['os_image'],
                                '--mac', target['inventory']['mac'], '--public-key', str(self.directory / 'id_ed25519.pub'),
                                '--generation', target['intent'], '--network', self.state['network']]))
        self.mutation('instance', 'allocate', '--prefix-name', target['intent'], '--machine-id', target['machine'],
                      '--flat-vpc-id', self.state['vpc'], '--os', json.dumps(config),
                      log_path=self.directory / f'allocate-{self.state["generation"]}-{target["release"]}.log')
        current = self.instance(target)
        target['instance'] = current['id']
        target['addresses'] = selected_addresses(current, self.state['network'], target['inventory'])
        self.checkpoint()

    def installed(self, target):
        # Libvirt rotates serial logs. Verify the disk generation over SSH before
        # persisting its host key, so an imager or previous OS cannot poison pins.
        pinned = self.host_keys(target)
        with tempfile.TemporaryDirectory(prefix='probe-', dir=self.directory) as temporary:
            known_hosts = Path(temporary) / 'known-hosts'
            if pinned.exists():
                known_hosts.write_bytes(pinned.read_bytes())
            boot_id = None
            for family in ('6', '4'):
                if family not in target['addresses']:
                    continue
                output = self.ssh(target, 'set -e; test "$(findmnt -no SOURCE /)" = /dev/vda1; sudo cloud-init status --wait >/dev/null; '
                                  'test ! -e /etc/nico-peer-retained; test "$(cat /etc/nico-lab-generation)" = ' + shlex.quote(target['intent']) + '; cat /proc/sys/kernel/random/boot_id',
                                  known_hosts=known_hosts, address=target['addresses'][family])
                observed = str(uuid.UUID(output.strip()))
                if boot_id is not None and observed != boot_id:
                    raise ValueError('guest address families identify different boots')
                boot_id = observed
            if boot_id is None or not known_hosts.read_bytes().strip():
                raise ValueError('verified guest did not persist its SSH host key')
            known_hosts.replace(pinned)
        target['boot_id'] = boot_id
        self.checkpoint()
        return True

    def leases_ready(self, target, info=None):
        if info is None:
            info = json.loads(self.ssh(target, 'ip -j address'))
        for address in target['addresses'].values():
            leases = [a for i in info for a in i.get('addr_info', []) if a.get('local') == address]
            if not leases:
                return False
            if len(leases) != 1 or leases[0]['valid_life_time'] == 4294967295:
                raise ValueError('allocated address is not a unique finite DHCP lease')
        return True

    def verify_network(self, target):
        info = json.loads(self.ssh(target, 'ip -j address'))
        if not self.leases_ready(target, info):
            raise ValueError('allocated address is missing from the guest')
        if self.state['network'] == 'ipv6-only':
            v4 = [a for i in info if i['ifname'] != 'lo' for a in i.get('addr_info', []) if a['family'] == 'inet']
            routes = json.loads(self.ssh(target, 'ip -j -4 route show table all'))
            if v4 or any(r.get('dev') != 'lo' for r in routes):
                raise ValueError('IPv6-only guest has an IPv4 address or route')
            self.ssh(target, 'command -v ping >/dev/null || exit 1; if ping -4 -c 1 -W 2 1.1.1.1 >/dev/null 2>&1; then exit 1; fi')
        if self.state['internet_ipv6']:
            result = self.ssh(target, 'curl -6 --noproxy "*" -fsS --connect-timeout 10 --max-time 30 -o /dev/null '
                              '-w "HTTP %{http_code} local=%{local_ip} remote=%{remote_ip}\\n" https://www.google.com')
            print(target['release'] + ' ' + result.strip(), flush=True)
        save(self.directory / f'addresses-{self.state["generation"]}-{target["release"]}.json', info)

    def peers(self):
        first, second = self.state['targets']
        for family in first['addresses']:
            output = run([sys.executable, str(HERE / 'check-connectivity.py'), '--context', self.state['context'],
                          '--namespace', first['namespace'], '--identity', str(self.directory / 'id_ed25519'),
                          '--first-release', first['release'], '--second-release', second['release'],
                          '--first-address', first['addresses'][family], '--second-address', second['addresses'][family],
                          '--first-known-hosts', str(self.directory / f'known-hosts-{self.state["generation"]}-{first["release"]}'),
                          '--second-known-hosts', str(self.directory / f'known-hosts-{self.state["generation"]}-{second["release"]}')])
            print(output.strip(), flush=True)

    def reboot(self, target, peer):
        self.instance(target)
        before = self.ssh(target, 'sudo touch /etc/nico-peer-retained; sudo sync; cat /proc/sys/kernel/random/boot_id').strip()
        peer_before = self.ssh(peer, 'cat /proc/sys/kernel/random/boot_id').strip()
        self.mutation('instance', 'reboot', '--instance', target['instance'])
        def restarted():
            after = self.ssh(target, 'test -f /etc/nico-peer-retained && cat /proc/sys/kernel/random/boot_id').strip()
            return str(uuid.UUID(after)) != str(uuid.UUID(before)) and self.leases_ready(target)
        self.wait(target['release'] + ' reboot with retained disk', restarted)
        if self.ssh(peer, 'cat /proc/sys/kernel/random/boot_id').strip() != peer_before:
            raise ValueError('peer rebooted unexpectedly')

    def release(self, target):
        if not target.get('intent'):
            return
        # Listing also handles an allocation whose response was lost before its ID was saved.
        matches = [i for i in self.admin('instance', 'show', json_output=True)['instances'] if i['machine_id'] == target['machine']]
        if matches:
            if len(matches) != 1:
                raise ValueError('multiple instances on selected machine')
            identity = matching_instance(matches[0], target, target['intent'])
            if target.get('instance') and identity != target['instance']:
                raise ValueError('refusing to release a replacement instance')
            target['instance'] = identity
            self.checkpoint()
            self.mutation('instance', 'release', '--instance', identity)
        def released():
            current = self.admin('instance', 'show', json_output=True)['instances']
            return not any(i['machine_id'] == target['machine'] for i in current) and self.ready(target)
        self.wait(target['release'] + ' released, Ready/Scout', released)
        target.pop('intent', None)
        target.pop('instance', None)
        self.checkpoint()

    def diagnostics(self):
        directory = self.directory / ('diagnostics-' + uuid.uuid4().hex)
        directory.mkdir()
        for target in self.state['targets']:
            try:
                self.verify_ownership([target])
                output = self.kube(target, 'exec', 'deployment/' + target['release'], '--', 'cat', '/data/logs/console.log')
                (directory / (target['release'] + '-console.log')).write_text(output)
                save(directory / (target['release'] + '-machine.json'), self.admin('machine', 'show', target['machine'], json_output=True))
            except Exception as error:
                print('Diagnostic collection failed: ' + str(error), file=sys.stderr)

    def exercise(self):
        self.verify_ownership()
        self.preflight_network()
        # A retained journal is deliberately cleanup-only; do not replay allocations or adopt guests.
        for generation in range(1, self.state['cycles'] + 1):
            self.state['generation'] = generation
            for target in self.state['targets']:
                self.allocate(target)
            if self.state['targets'][0]['instance'] == self.state['targets'][1]['instance']:
                raise ValueError('instance IDs are not distinct')
            for target in self.state['targets']:
                self.wait(target['release'] + ' installed disk/cloud-init', lambda: self.installed(target))
                self.wait(target['release'] + ' DHCP leases', lambda: self.leases_ready(target))
                self.verify_network(target)
            self.peers()
            first, second = self.state['targets']
            for target, peer in ((first, second), (second, first)):
                self.reboot(target, peer)
                for guest in self.state['targets']:
                    self.verify_network(guest)
                self.peers()
            peer_boot = self.ssh(second, 'cat /proc/sys/kernel/random/boot_id')
            self.release(first)
            if self.ssh(second, 'cat /proc/sys/kernel/random/boot_id') != peer_boot:
                raise ValueError('surviving peer changed during release')
            self.release(second)
        self.state['complete'] = True
        self.checkpoint()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state-dir', required=True, type=Path, help='new private run directory, or retained directory for --release-only')
    parser.add_argument('--release-only', action='store_true', help='release only recorded instances; retain lab infrastructure and evidence')
    parser.add_argument('--context', help='explicit kind context')
    parser.add_argument('--namespace', default='bmc-lab')
    parser.add_argument('--core-namespace', default='nico-system')
    parser.add_argument('--target', action='append', metavar='RELEASE=MACHINE', help='exactly two distinct native target/machine pairs')
    parser.add_argument('--vpc', type=uuid.UUID, help='existing flat VPC UUID')
    parser.add_argument('--os-image', type=uuid.UUID, help='existing native OS image UUID with an accessible, digest-verified image')
    parser.add_argument('--network', choices=('ipv4', 'dual-stack', 'ipv6-only'), default='ipv4')
    parser.add_argument('--cycles', type=int, choices=(1, 2), default=2)
    parser.add_argument('--timeout', type=int, default=1800, help='seconds per lifecycle wait (60..7200; default 1800)')
    parser.add_argument('--internet-ipv6', action='store_true', help='require certificate-verified Internet IPv6 HTTPS for both guests')
    args = parser.parse_args()
    os.umask(0o077)
    directory = args.state_dir.resolve()
    if any(c.isspace() or c in chr(34) + chr(39) + '%$' + chr(92) for c in str(directory)):
        parser.error('state directory cannot contain whitespace, quotes, %, $, or backslashes')
    if args.release_only:
        if args.context or args.target or args.vpc or args.os_image:
            parser.error('--release-only uses the context and identities from its journal')
        with (directory / 'run.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            state = json.loads((directory / 'state.json').read_text())
            lab = Lifecycle(directory, state)
            lab.cleanup()
            return
    if not args.context or not args.context.startswith('kind-') or not args.vpc or not args.os_image or len(args.target or []) != 2:
        parser.error('require --context kind-..., --vpc, --os-image and two --target RELEASE=MACHINE pairs')
    if not 60 <= args.timeout <= 7200 or (args.internet_ipv6 and args.network == 'ipv4'):
        parser.error('timeout must be 60..7200; Internet IPv6 requires an IPv6 guest profile')
    targets = []
    for value in args.target:
        parts = value.split('=')
        if len(parts) != 2 or not all(parts) or '/' in parts[0] or parts[0].startswith('-'):
            parser.error('each target must be RELEASE=MACHINE')
        targets.append(dict(release=parts[0], machine=parts[1], namespace=args.namespace))
    if len({t['release'] for t in targets}) != 2 or len({t['machine'] for t in targets}) != 2:
        parser.error('targets and machine IDs must be distinct')
    cluster = json.loads(run(['kubectl', '--context', args.context, 'get', 'namespace', 'kube-system', '-o', 'json']))['metadata']['uid']
    directory.mkdir(mode=0o700, parents=False, exist_ok=False)
    with (directory / 'run.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        state = dict(version=1, run='lab-' + uuid.uuid4().hex[:12], context=args.context, cluster=cluster,
                     core_namespace=args.core_namespace, targets=targets, vpc=str(args.vpc), os_image=str(args.os_image),
                     network=args.network, cycles=args.cycles, timeout=args.timeout, internet_ipv6=args.internet_ipv6, generation=0)
        lab = Lifecycle(directory, state)
        lab.checkpoint()
        try:
            lab.verify_ownership()
            lab.preflight_network()
            for target in targets:
                lab.wait(target['release'] + ' initial Ready/Scout', lambda: lab.ready(target))
            run(['ssh-keygen', '-q', '-t', 'ed25519', '-N', '', '-C', 'nico-lab-test', '-f', str(directory / 'id_ed25519')])
            lab.exercise()
        except (Exception, KeyboardInterrupt):
            lab.diagnostics()
            raise

if __name__ == '__main__':
    main()
