#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Own one native libvirt guest and its standalone BMC for a Kubernetes pod."""

import argparse
from contextlib import contextmanager
import fcntl
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import platform
import shutil
import signal
import socket
import subprocess
import sys
import time
import uuid
import xml.etree.ElementTree as ET

import preflight
from network import Network, stop_child

ROOT = Path('/data')
READY = Path('/run/nico-target-ready')
DOMAIN = 'target'
STOP = False


def run(*args, timeout=30):
    return subprocess.run(args, check=True, text=True, stdout=subprocess.PIPE, timeout=timeout).stdout.strip()


def virsh(*args, timeout=30):
    return run('virsh', '-c', 'qemu:///system', *args, timeout=timeout)


def configuration(value):
    required = {'clusterUID', 'owner', 'nodeName', 'bmcAddress', 'architecture', 'instanceIndex', 'memoryMiB', 'cpus', 'diskGiB', 'bridgeAddress', 'bmcPort'}
    if not required <= set(value) or set(value) - required - {'hugepages', 'bridgeAddressV6'}:
        raise ValueError('target configuration requires: ' + ', '.join(sorted(required)) + '; optional: hugepages, bridgeAddressV6')
    value = dict(value)
    if type(value.get('hugepages', False)) is not bool:
        raise ValueError('hugepages must be a boolean')
    if not value.get('hugepages'):
        value.pop('hugepages', None)
    if value['architecture'] not in ('amd64', 'arm64'):
        raise ValueError('architecture must be amd64 or arm64')
    for name, lower, upper in [('instanceIndex', 1, 255), ('memoryMiB', 1024, 65536),
                               ('cpus', 1, 32), ('diskGiB', 8, 1024), ('bmcPort', 1024, 65535)]:
        if type(value[name]) is not int or not lower <= value[name] <= upper:
            raise ValueError(f'{name} must be an integer from {lower} to {upper}')
    if value.get('hugepages') and value['memoryMiB'] % 2:
        raise ValueError('hugepages requires memoryMiB to be a multiple of 2')
    for key in ('clusterUID', 'owner', 'nodeName'):
        if not isinstance(value[key], str) or not value[key] or len(value[key]) > 253:
            raise ValueError(f'{key} must be a nonempty identity of at most 253 characters')
    bmc = ipaddress.ip_address(value['bmcAddress'])
    if bmc.version == 6 and (bmc.scope_id is not None or not (
            bmc in ipaddress.IPv6Network('fc00::/7') or bmc in ipaddress.IPv6Network('2000::/3'))):
        raise ValueError('bmcAddress must be an unscoped ULA or global IPv6 Service address')
    value['bmcAddress'] = str(bmc)
    network = ipaddress.IPv4Interface(value['bridgeAddress'])
    if network.ip in (network.network.network_address, network.network.broadcast_address) or network.network.prefixlen > 30:
        raise ValueError('bridgeAddress must be a usable IPv4 gateway address with prefix /30 or shorter')
    if 'bridgeAddressV6' in value:
        address = value['bridgeAddressV6']
        if not isinstance(address, str):
            raise ValueError('bridgeAddressV6 must be a string')
        if address == '':
            value.pop('bridgeAddressV6')
        else:
            ipv6 = ipaddress.IPv6Interface(address)
            if (ipv6.scope_id is not None or ipv6.network.prefixlen != 64 or ipv6.ip == ipv6.network.network_address
                    or not (ipv6.ip in ipaddress.IPv6Network('fc00::/7')
                            or ipv6.ip in ipaddress.IPv6Network('2000::/3'))):
                raise ValueError('bridgeAddressV6 must be a unicast ULA or global /64 gateway with a nonzero host part')
            value['bridgeAddressV6'] = str(ipv6)
    identity = value['clusterUID'] + '/' + value['owner']
    return dict(value, version=1, uuid=str(uuid.uuid5(uuid.NAMESPACE_URL, identity)),
                bridge='nvt' + hashlib.sha256(identity.encode()).hexdigest()[:10],
                mac=f'02:00:00:{value["instanceIndex"]:02x}:00:02',
                serial=f'020000{value["instanceIndex"]:02X}0001')


def claim(root, config):
    inventory = root / 'inventory.json'
    if inventory.exists():
        if json.loads(inventory.read_text()) != config:
            raise ValueError('retained target identity or sizing differs; use its original configuration')
    else:
        if any(path.name not in ('.lock', 'lost+found') for path in root.iterdir()):
            raise ValueError('refusing to adopt unrecognized nonempty target storage')
        with inventory.open('x') as stream:
            json.dump(config, stream, sort_keys=True)
            stream.flush()
            os.fsync(stream.fileno())


@contextmanager
def bound_storage(root, config):
    with (root / '.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        claim(root, config)
        yield


def domain_xml(config, root=ROOT):
    arm = config['architecture'] == 'arm64'
    architecture = 'aarch64' if arm else 'x86_64'
    firmware = '/usr/share/AAVMF/AAVMF_CODE.fd' if arm else '/usr/share/OVMF/OVMF_CODE_4M.fd'
    domain = ET.Element('domain', type='kvm')

    def node(parent, tag, text=None, **attributes):
        element = ET.SubElement(parent, tag, attributes)
        element.text = str(text) if text is not None else None
        return element

    node(domain, 'name', DOMAIN)
    node(domain, 'uuid', config['uuid'])
    node(domain, 'memory', config['memoryMiB'], unit='MiB')
    node(domain, 'vcpu', config['cpus'])
    if config.get('hugepages'):
        backing = node(domain, 'memoryBacking')
        node(node(backing, 'hugepages'), 'page', size='2048', unit='KiB')
        node(backing, 'source', type='memfd')
        node(backing, 'allocation', mode='immediate')
    operating_system = node(domain, 'os')
    node(operating_system, 'type', 'hvm', arch=architecture, machine='virt' if arm else 'q35')
    node(operating_system, 'loader', firmware, readonly='yes', type='pflash')
    node(operating_system, 'nvram', root / 'disks/nvram.fd')
    node(operating_system, 'boot', dev='network')
    node(operating_system, 'boot', dev='hd')
    node(operating_system, 'smbios', mode='sysinfo')
    system = node(node(domain, 'sysinfo', type='smbios'), 'system')
    node(system, 'entry', config['serial'], name='serial')
    node(system, 'entry', config['uuid'], name='uuid')
    node(node(domain, 'features'), 'acpi')
    node(domain, 'cpu', mode='host-passthrough')
    node(domain, 'on_poweroff', 'destroy')
    node(domain, 'on_reboot', 'restart')
    devices = node(domain, 'devices')
    node(devices, 'emulator', f'/usr/bin/qemu-system-{architecture}')
    disk = node(devices, 'disk', type='file', device='disk')
    node(disk, 'driver', name='qemu', type='qcow2')
    node(disk, 'source', file=str(root / 'disks/root.qcow2'))
    node(disk, 'target', dev='vda', bus='virtio')
    nic = node(devices, 'interface', type='bridge')
    node(nic, 'mac', address=config['mac'])
    node(nic, 'source', bridge=config['bridge'])
    node(nic, 'model', type='virtio')
    if arm:
        node(nic, 'rom', enabled='no')
    else:
        node(nic, 'rom', enabled='yes', file='/usr/lib/ipxe/qemu/efi-virtio.rom')
    console = node(devices, 'console', type='pty')
    node(console, 'log', file=str(root / 'logs/console.log'), append='on')
    node(console, 'target', type='serial', port='0')
    return ET.tostring(domain, encoding='unicode')


def check_memory_backing(config, xml):
    try:
        domain = ET.fromstring(xml)
    except ET.ParseError as error:
        raise ValueError('retained domain XML is invalid; restore its original configuration') from error
    expected = ET.fromstring(domain_xml(config))
    memory = domain.find('memory')
    units = {'KiB': 1, 'MiB': 1024}
    valid = (domain.findtext('uuid') == config['uuid'] and memory is not None
             and memory.get('unit', 'KiB') in units
             and (memory.text or '').isdigit()
             and int(memory.text) * units[memory.get('unit', 'KiB')] == config['memoryMiB'] * 1024)
    actual_backing = domain.find('memoryBacking')
    expected_backing = expected.find('memoryBacking')
    if expected_backing is None:
        valid = valid and actual_backing is None
    else:
        valid = valid and actual_backing is not None
        for path in ('hugepages/page', 'source', 'allocation'):
            actual = domain.find('memoryBacking/' + path)
            wanted = expected_backing.find(path)
            valid = valid and actual is not None and actual.attrib == wanted.attrib
        valid = valid and len(domain.findall('memoryBacking/hugepages/page')) == 1
    if not valid:
        raise ValueError('retained domain memory backing or identity differs; restore its original configuration')


def prepare_storage(config):
    for directory in ('disks', 'domains', 'libvirt', 'logs'):
        (ROOT / directory).mkdir(exist_ok=True)
    disk, nvram = ROOT / 'disks/root.qcow2', ROOT / 'disks/nvram.fd'
    if (ROOT / 'initialized').exists() and (not disk.is_file() or not nvram.is_file()):
        raise ValueError('initialized target is missing its disk or firmware state; restore retained data')
    if (ROOT / 'bmc-initialized').exists() and not (ROOT / 'bmc.json').is_file():
        raise ValueError('initialized BMC is missing its credentials; restore retained data')
    autostart = ROOT / 'domains/autostart'
    if autostart.exists() and any(autostart.iterdir()):
        raise ValueError('target domain autostart is unsupported; power must remain under BMC control')
    if not disk.exists():
        run('qemu-img', 'create', '-f', 'qcow2', str(disk), f'{config["diskGiB"]}G')
    if not nvram.exists():
        template = '/usr/share/AAVMF/AAVMF_VARS.fd' if config['architecture'] == 'arm64' else '/usr/share/OVMF/OVMF_VARS_4M.fd'
        shutil.copyfile(template, nvram)


def check_network(config):
    links = json.loads(run('ip', '-json', 'link', 'show'))
    existing = next((link for link in links if link['ifname'] == config['bridge']), None)
    if existing and existing.get('ifalias') != config['uuid']:
        raise ValueError('target bridge exists with a different owner')
    for key in ('bridgeAddress', 'bridgeAddressV6'):
        if key not in config:
            continue
        subnet = ipaddress.ip_interface(config[key]).network
        for route in json.loads(run('ip', '-' + str(subnet.version), '-json', 'route', 'show', 'table', 'all')):
            if route.get('dev') == config['bridge'] or route['dst'] == 'default':
                continue
            if subnet.overlaps(ipaddress.ip_network(route['dst'], strict=False)):
                raise ValueError('target subnet overlaps an existing route: ' + route['dst'])
    return existing


def check_bmc_port(config):
    # Match bmc-mock's wildcard listener; an IPv6 Service cannot use IPv4 fallback.
    listener = None
    address = '::'
    try:
        listener = socket.socket(socket.AF_INET6, socket.SOCK_STREAM)
        listener.setsockopt(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0)
    except OSError:
        if listener is not None:
            listener.close()
        if ipaddress.ip_address(config['bmcAddress']).version == 6:
            raise ValueError('IPv6 BMC Service requires an IPv6 listener')
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        address = '0.0.0.0'
    with listener:
        listener.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        listener.bind((address, config['bmcPort']))
        listener.listen(1)
        if ipaddress.ip_address(config['bmcAddress']).version == 6:
            with socket.create_connection(('::1', config['bmcPort']), timeout=1):
                pass


def bridge(config):
    existing = check_network(config)
    if not existing:
        run('ip', 'link', 'add', config['bridge'], 'type', 'bridge')
        try:
            run('ip', 'link', 'set', config['bridge'], 'alias', config['uuid'])
        except (subprocess.SubprocessError, OSError):
            run('ip', 'link', 'delete', config['bridge'])
            raise
    run('ip', 'address', 'replace', config['bridgeAddress'], 'dev', config['bridge'])
    if config.get('bridgeAddressV6'):
        run('ip', '-6', 'address', 'replace', config['bridgeAddressV6'], 'dev', config['bridge'])
    run('ip', 'link', 'set', config['bridge'], 'up')


def clear_bridge(config):
    links = json.loads(run('ip', '-json', 'link', 'show'))
    owned = next((link for link in links if link['ifname'] == config['bridge']), None)
    if owned and owned.get('ifalias') == config['uuid']:
        attached = json.loads(run('ip', '-json', 'link', 'show', 'master', config['bridge']))
        if not attached:
            run('ip', 'link', 'delete', config['bridge'])


def serve(config, networking=None):
    children = []
    bmc = None
    domain_ready = False
    try:
        prepare_storage(config)
        Path('/run/libvirt').mkdir(exist_ok=True)
        bridge(config)
        if networking is not None:
            children.extend(networking.start())
        children.append(subprocess.Popen(['virtlogd']))
        children.append(subprocess.Popen(['libvirtd']))
        for attempt in range(60):
            if networking is not None:
                networking.refresh()
            if any(child.poll() is not None for child in children):
                raise RuntimeError('target child process exited during startup')
            try:
                virsh('list', '--all')
                break
            except subprocess.CalledProcessError:
                if STOP or attempt == 59:
                    raise
                time.sleep(1)
        domains = virsh('list', '--all', '--name').splitlines()
        if domains and domains != [DOMAIN]:
            raise ValueError('unexpected domains in retained target storage')
        if not domains:
            if (ROOT / 'initialized').exists():
                raise ValueError('initialized target is missing its domain; restore retained data')
            xml = ROOT / 'domain.xml'
            xml.write_text(domain_xml(config))
            virsh('define', str(xml))
        if virsh('domuuid', DOMAIN) != config['uuid']:
            raise ValueError('retained libvirt domain identity differs')
        check_memory_backing(config, virsh('dumpxml', DOMAIN, '--inactive'))
        if (ROOT / 'libvirt/qemu/save/target.save').is_file():
            check_memory_backing(config, virsh('managedsave-dumpxml', DOMAIN))
        domain_ready = True
        (ROOT / 'initialized').touch()
        # A new target stays off. Only Redfish controls power-on or saved-state restoration.
        bmc = subprocess.Popen(['bmc-mock', '--machine-role', 'host', '--hardware-profile', 'generic_ami',
            '--dpu-count', '0', '--instance-index', str(config['instanceIndex']), '--port', str(config['bmcPort']),
            '--redfish-auth', '--state-backend', 'libvirt', '--libvirt-domain', DOMAIN,
            '--state-file', str(ROOT / 'bmc.json')])
        children.append(bmc)
        for attempt in range(60):
            if networking is not None:
                networking.refresh()
            if STOP or any(child.poll() is not None for child in children):
                raise RuntimeError('target runtime stopped during BMC startup')
            try:
                with socket.create_connection(('::1' if ':' in config['bmcAddress'] else '127.0.0.1', config['bmcPort']), timeout=1):
                    break
            except OSError:
                if attempt == 59:
                    raise
                time.sleep(1)
        if not (ROOT / 'bmc.json').is_file():
            raise ValueError('BMC did not persist initial state')
        (ROOT / 'bmc-initialized').touch()
        READY.touch()
        while not STOP:
            if networking is not None:
                networking.refresh()
            if any(child.poll() is not None for child in children):
                raise RuntimeError('target child process exited')
            time.sleep(1)
    finally:
        READY.unlink(missing_ok=True)
        # Stop the BMC writer before saving the guest or stopping libvirt.
        if bmc is not None:
            stop_child(bmc)
            children.remove(bmc)
        if domain_ready:
            try:
                if virsh('domstate', DOMAIN) in ('running', 'paused'):
                    virsh('managedsave', DOMAIN, timeout=150)
            except (subprocess.SubprocessError, OSError) as error:
                print(f'guest save failed; memory may be lost: {error}', file=sys.stderr)
        for child in reversed(children):
            stop_child(child)
        if networking is not None:
            try:
                networking.clear()
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                print(f'network cleanup failed; owned rules will be retried on next start: {error}', file=sys.stderr)
        # Retain a bridge with live TAPs after an unsuccessful guest save.
        clear_bridge(config)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--config', required=True, type=Path)
    parser.add_argument('--dhcp-server', default='', help='optional IPv4 DHCP Service address; requires --pod-cidr')
    parser.add_argument('--pod-cidr', default='', help='selected single-node kind IPv4 pod CIDR; requires --dhcp-server')
    parser.add_argument('--uplink', default='eth0', help='node uplink used by the relay and Internet masquerade')
    parser.add_argument('--dhcp6-server', default='', help='optional IPv6 DHCP Service address; requires bridgeAddressV6')
    parser.add_argument('--pod-cidr-v6', default='', help='selected kind node IPv6 pod CIDR; required with --dhcp6-server')
    parser.add_argument('--ipv6-default-route', action='store_true', help='advertise this bridge as an IPv6 default router; requires --dhcp6-server and separately configured upstream routing')
    args = parser.parse_args()
    config = configuration(json.loads(args.config.read_text()))
    if preflight.ARCHITECTURES.get(platform.machine()) != config['architecture']:
        raise ValueError('target architecture must match the native runtime')
    if os.environ.get('NODE_NAME') != config['nodeName']:
        raise ValueError('target must run on its configured Kubernetes node')
    networking = Network(config, run, args.dhcp_server, args.pod_cidr, args.uplink, args.dhcp6_server, args.ipv6_default_route, args.pod_cidr_v6)
    preflight.check_kvm(config['architecture'])
    if config.get('hugepages'):
        available = preflight.hugepage_capacity()
        if available < config['memoryMiB']:
            raise ValueError(f'insufficient unreserved 2 MiB huge pages: need {config["memoryMiB"]} MiB, '
                             f'available {available} MiB; reserve the pool on the Linux host before starting targets')
    # Reject fixable host preconditions before binding fresh storage to these values.
    check_network(config)
    check_bmc_port(config)
    networking.preflight()
    os.umask(0o077)
    with bound_storage(ROOT, config):
        serve(config, networking)


def stopping(_signal, _frame):
    global STOP
    STOP = True


if __name__ == '__main__':
    signal.signal(signal.SIGTERM, stopping)
    signal.signal(signal.SIGINT, stopping)
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'target runtime failed: {error}', file=sys.stderr)
        sys.exit(1)
