# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import importlib.util
import errno
import json
import os
from pathlib import Path
import sys
import subprocess
import socket
import shlex
import tempfile
import unittest
from unittest.mock import Mock, patch
import xml.etree.ElementTree as ET

LAB = Path(__file__).resolve().parents[1] / 'lab'
TARGET = load_script('bmc_target', LAB / 'target.py')

CONFIG = {'nodeName': 'node', 'bmcAddress': '10.96.40.10', 'clusterUID': 'cluster', 'owner': 'namespace/release', 'architecture': 'arm64',
          'instanceIndex': 1, 'memoryMiB': 4096, 'cpus': 2, 'diskGiB': 24,
          'bridgeAddress': '10.84.40.1/24', 'bmcPort': 2443}


class TargetTest(unittest.TestCase):
    def test_ipv6_bmc_address_is_canonical_and_part_of_retained_identity(self):
        config = TARGET.configuration({**CONFIG, 'bmcAddress': 'fd00:10:96:0::4010'})
        self.assertEqual(config['bmcAddress'], 'fd00:10:96::4010')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            TARGET.claim(root, config)
            with self.assertRaisesRegex(ValueError, 'retained target'):
                TARGET.claim(root, TARGET.configuration(CONFIG))
        for address in ('::', '::1', 'ff02::1', 'fe80::1%eth0', '::ffff:10.96.40.10'):
            with self.subTest(address=address), self.assertRaises(ValueError):
                TARGET.configuration({**CONFIG, 'bmcAddress': address})

    def test_redfish_smoke_request_serializes_both_address_families(self):
        import io
        import urllib.request
        spec = importlib.util.spec_from_file_location('test_target', LAB / 'test-target.py')
        smoke = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(smoke)
        payload = dict(username='test', password='test', method='GET', path='/redfish/v1/Systems', body=None)
        for address, authority in [('10.96.40.10', '10.96.40.10'), ('fd00:10:96::4010', '[fd00:10:96::4010]')]:
            with self.subTest(address=address), patch.object(Path, 'read_text', return_value=json.dumps({'bmcAddress': address})), \
                    patch.object(sys, 'stdin', io.StringIO(json.dumps(payload))), \
                    patch.object(urllib.request, 'urlopen') as open_url, patch('builtins.print'):
                open_url.return_value.__enter__.return_value.read.return_value = b'{}'
                exec(smoke.REQUEST, {})
                self.assertEqual(open_url.call_args.args[0].full_url, 'https://' + authority + '/redfish/v1/Systems')

    def test_ipv6_bridge_is_optional_canonical_and_immutable(self):
        legacy = TARGET.configuration(CONFIG)
        self.assertEqual(TARGET.configuration({**CONFIG, 'bridgeAddressV6': ''}), legacy)
        dual = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:0040::1/64'})
        self.assertEqual(dual['bridgeAddressV6'], 'fd42:84:40::1/64')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            TARGET.claim(root, legacy)
            with self.assertRaisesRegex(ValueError, 'retained target'):
                TARGET.claim(root, dual)
        for value in ('fd42:84:40::/64', 'fd42:84:40::1/96', 'fe80::1/64',
                      'ff02::1/64', 'fd42:84:40::1%eth0/64', '::1/64', '::ffff:10.0.0.1/64', '10.0.0.1/24', None):
            with self.subTest(value=value), self.assertRaises(ValueError):
                TARGET.configuration({**CONFIG, 'bridgeAddressV6': value})

    def test_ipv6_route_collision_is_rejected_before_bridge_mutation(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        with patch.object(TARGET, 'run', side_effect=['[]', '[]',
                '[{"dst":"fd42:84::/32","dev":"other"}]']) as run:
            with self.assertRaisesRegex(ValueError, 'overlaps'):
                TARGET.bridge(config)
            self.assertEqual([call.args[:3] for call in run.call_args_list],
                             [('ip', '-json', 'link'), ('ip', '-4', '-json'), ('ip', '-6', '-json')])

    def test_dual_stack_bridge_assigns_both_gateways_before_bringing_link_up(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        with patch.object(TARGET, 'check_network', return_value=None), patch.object(TARGET, 'run') as run:
            TARGET.bridge(config)
        self.assertEqual([call.args for call in run.call_args_list], [
            ('ip', 'link', 'add', config['bridge'], 'type', 'bridge'),
            ('ip', 'link', 'set', config['bridge'], 'alias', config['uuid']),
            ('ip', 'address', 'replace', CONFIG['bridgeAddress'], 'dev', config['bridge']),
            ('ip', '-6', 'address', 'replace', 'fd42:84:40::1/64', 'dev', config['bridge']),
            ('ip', 'link', 'set', config['bridge'], 'up')])

    def test_ipv6_forwarding_failure_does_not_claim_fresh_storage(self):
        read = Path.read_text
        def read_text(path, *args, **kwargs):
            return '0' if str(path) == '/proc/sys/net/ipv6/conf/all/forwarding' else read(path, *args, **kwargs)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config_file = root / 'config.json'
            config_file.write_text(json.dumps({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'}))
            data = root / 'data'
            data.mkdir()
            with patch.object(TARGET, 'ROOT', data), \
                    patch.object(sys, 'argv', ['target.py', '--config', str(config_file)]), \
                    patch.dict(TARGET.os.environ, NODE_NAME=CONFIG['nodeName']), \
                    patch.object(TARGET.platform, 'machine', return_value='aarch64'), \
                    patch.object(TARGET.preflight, 'check_kvm'), \
                    patch.object(TARGET, 'check_network'), patch.object(TARGET, 'check_bmc_port'), \
                    patch.object(Path, 'read_text', autospec=True, side_effect=read_text), \
                    self.assertRaisesRegex(ValueError, 'IPv6 forwarding'):
                TARGET.main()
            self.assertEqual(list(data.iterdir()), [])

    def test_hugepage_config_preserves_legacy_identity_and_rejects_backing_changes(self):
        legacy = TARGET.configuration(CONFIG)
        self.assertEqual(TARGET.configuration({**CONFIG, 'hugepages': False}), legacy)
        huge = TARGET.configuration({**CONFIG, 'hugepages': True})
        self.assertEqual(huge['uuid'], legacy['uuid'])
        for invalid in [{'hugepages': 'true'}, {'hugepages': True, 'memoryMiB': 4097}]:
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                TARGET.configuration({**CONFIG, **invalid})
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            TARGET.claim(root, legacy)
            with self.assertRaisesRegex(ValueError, 'retained target'):
                TARGET.claim(root, huge)

    def test_hugepage_xml_requires_explicit_immediate_backing_and_matching_identity(self):
        config = TARGET.configuration({**CONFIG, 'hugepages': True})
        xml = TARGET.domain_xml(config)
        domain = ET.fromstring(xml)
        self.assertEqual(domain.find('memoryBacking/hugepages/page').attrib, {'size': '2048', 'unit': 'KiB'})
        self.assertEqual(domain.find('memoryBacking/source').get('type'), 'memfd')
        self.assertEqual(domain.find('memoryBacking/allocation').get('mode'), 'immediate')
        TARGET.check_memory_backing(config, xml)
        for changed in [xml.replace('immediate', 'ondemand'), xml.replace('2048', '1048576'),
                        xml.replace(config['uuid'], 'wrong'), TARGET.domain_xml(TARGET.configuration(CONFIG))]:
            with self.subTest(changed=changed), self.assertRaisesRegex(ValueError, 'retained domain'):
                TARGET.check_memory_backing(config, changed)

    def test_libvirt_normalized_memory_is_accepted_for_legacy_and_hugepage_guests(self):
        for hugepages in (False, True):
            with self.subTest(hugepages=hugepages):
                config = TARGET.configuration(dict(CONFIG, hugepages=hugepages))
                # Relevant fields from native libvirt dumpxml and managedsave-dumpxml.
                backing = """<memoryBacking><hugepages><page size='2048' unit='KiB'/></hugepages>
                  <source type='memfd'/><allocation mode='immediate'/></memoryBacking>""" if hugepages else ''
                xml = f"""<domain type='kvm'><name>target</name><uuid>{config['uuid']}</uuid>
                  <memory unit='KiB'>4194304</memory><currentMemory unit='KiB'>4194304</currentMemory>
                  {backing}</domain>"""
                TARGET.check_memory_backing(config, xml)
                with self.assertRaisesRegex(ValueError, 'retained domain'):
                    TARGET.check_memory_backing(config, xml.replace('4194304', '4096'))

    def test_missing_hugepage_pool_does_not_claim_fresh_storage(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            config_file = root / 'config.json'
            config_file.write_text(json.dumps({**CONFIG, 'hugepages': True}))
            data = root / 'data'
            data.mkdir()
            with patch.object(TARGET, 'ROOT', data), \
                    patch.object(sys, 'argv', ['target.py', '--config', str(config_file)]), \
                    patch.dict(TARGET.os.environ, NODE_NAME=CONFIG['nodeName']), \
                    patch.object(TARGET.platform, 'machine', return_value='aarch64'), \
                    patch.object(TARGET.preflight, 'check_kvm'), \
                    patch.object(TARGET.preflight, 'hugepage_capacity', return_value=2048), \
                    self.assertRaisesRegex(ValueError, '2 MiB huge pages'):
                TARGET.main()
            self.assertEqual(list(data.iterdir()), [])

    def test_saved_domain_mismatch_prevents_bmc_start_and_preserves_saved_state(self):
        config = TARGET.configuration({**CONFIG, 'hugepages': True})
        children = []
        def spawn(command):
            child = Mock()
            child.poll.return_value = None
            children.append(command[0])
            return child
        def virsh(*args, **kwargs):
            return {'list': 'target', 'domuuid': config['uuid'], 'dumpxml': TARGET.domain_xml(config),
                    'managedsave-dumpxml': TARGET.domain_xml(TARGET.configuration(CONFIG))}[args[0]]
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            saved = root / 'libvirt/qemu/save/target.save'
            saved.parent.mkdir(parents=True)
            saved.write_bytes(b'retained state')
            with patch.object(TARGET, 'ROOT', root), patch.object(TARGET, 'READY', root / 'ready'), \
                    patch.object(TARGET, 'prepare_storage'), patch.object(TARGET, 'bridge'), \
                    patch.object(TARGET, 'clear_bridge'), patch.object(TARGET.Path, 'mkdir'), \
                    patch.object(TARGET, 'virsh', side_effect=virsh), \
                    patch.object(TARGET.subprocess, 'Popen', side_effect=spawn), \
                    self.assertRaisesRegex(ValueError, 'retained domain'):
                TARGET.serve(config)
            self.assertEqual(children, ['virtlogd', 'libvirtd'])
            self.assertEqual(saved.read_bytes(), b'retained state')
            self.assertFalse((root / 'ready').exists())

    def test_native_domain_preserves_identity_and_boot_contract(self):
        for arch, machine, firmware, index, mac, serial in [
                ('arm64', 'virt', 'AAVMF', 1, '02:00:00:01:00:02', '020000010001'),
                ('amd64', 'q35', 'OVMF', 10, '02:00:00:0a:00:02', '0200000A0001')]:
            with self.subTest(arch=arch):
                config = TARGET.configuration({**CONFIG, 'architecture': arch, 'instanceIndex': index})
                xml = ET.fromstring(TARGET.domain_xml(config))
                self.assertEqual(xml.attrib['type'], 'kvm')
                self.assertEqual(xml.findtext('uuid'), config['uuid'])
                self.assertEqual(xml.find('os/type').attrib['machine'], machine)
                self.assertIn(firmware, xml.findtext('os/loader'))
                self.assertEqual([b.attrib['dev'] for b in xml.findall('os/boot')], ['network', 'hd'])
                self.assertEqual(xml.find('devices/interface/mac').attrib['address'], mac)
                self.assertEqual(xml.findtext("sysinfo/system/entry[@name='serial']"), serial)
                self.assertEqual(xml.find('devices/disk/source').attrib['file'], '/data/disks/root.qcow2')
                self.assertEqual(xml.findtext('os/nvram'), '/data/disks/nvram.fd')

    def test_config_rejects_incompatible_guest_inputs(self):
        for changes in [{'architecture': 'ppc64le'}, {'cpus': True}, {'instanceIndex': 0},
                        {'memoryMiB': 0}, {'owner': ''}, {'bridgeAddress': '10.84.40.0/24'},
                        {'bridgeAddress': 'fd42::1/64'}, {'extra': 'field'}]:
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                TARGET.configuration({**CONFIG, **changes})

    def test_retained_storage_is_not_adopted_or_resized(self):
        config = TARGET.configuration(CONFIG)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            TARGET.claim(root, config)
            before = (root / 'inventory.json').read_bytes()
            TARGET.claim(root, config)
            for changed in [{**config, 'clusterUID': 'other'}, {**config, 'diskGiB': 48}]:
                with self.subTest(changed=changed), self.assertRaisesRegex(ValueError, 'retained target'):
                    TARGET.claim(root, changed)
            self.assertEqual((root / 'inventory.json').read_bytes(), before)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'bmc.json').write_text('{}')
            with self.assertRaisesRegex(ValueError, 'unrecognized'):
                TARGET.claim(root, config)

    def test_lock_rejects_a_second_writer_and_releases_after_failure(self):
        config = TARGET.configuration(CONFIG)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(RuntimeError, 'startup failure'):
                with TARGET.bound_storage(root, config):
                    with self.assertRaises(BlockingIOError):
                        with TARGET.bound_storage(root, config):
                            self.fail('second writer acquired the target')
                    raise RuntimeError('startup failure')
            with TARGET.bound_storage(root, config):
                self.assertTrue((root / 'inventory.json').is_file())

    def test_lost_bmc_state_is_not_reinitialized(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(TARGET, 'ROOT', Path(directory)), \
                patch.object(TARGET, 'run') as run:
            (Path(directory) / 'bmc-initialized').touch()
            with self.assertRaisesRegex(ValueError, 'missing its credentials'):
                TARGET.prepare_storage(TARGET.configuration(CONFIG))
            run.assert_not_called()

    def test_bridge_cleanup_preserves_foreign_and_attached_links(self):
        config = TARGET.configuration(CONFIG)
        for alias, attached, delete in [('other', [], False), (config['uuid'], [{'ifname': 'tap0'}], False), (config['uuid'], [], True)]:
            with self.subTest(alias=alias, attached=attached), \
                    patch.object(TARGET, 'run', side_effect=[json.dumps([{'ifname': config['bridge'], 'ifalias': alias}]), json.dumps(attached), '']) as run:
                TARGET.clear_bridge(config)
                self.assertEqual(any(call.args[:3] == ('ip', 'link', 'delete') for call in run.call_args_list), delete)

    def test_initialized_target_never_recreates_lost_disk(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(TARGET, 'ROOT', Path(directory)), \
                patch.object(TARGET, 'run') as run:
            (Path(directory) / 'initialized').touch()
            with self.assertRaisesRegex(ValueError, 'missing its disk'):
                TARGET.prepare_storage(TARGET.configuration(CONFIG))
            run.assert_not_called()

    def test_foreign_bridge_and_overlapping_routes_fail_before_mutation(self):
        config = TARGET.configuration(CONFIG)
        cases = [([{'ifname': config['bridge'], 'ifalias': 'another'}], [], 'different owner'),
                 ([], [{'dst': '10.84.0.0/16', 'dev': 'eth0'}], 'overlaps')]
        for links, routes, error in cases:
            with self.subTest(error=error), patch.object(TARGET, 'run', side_effect=[json.dumps(links), json.dumps(routes)]) as run:
                with self.assertRaisesRegex(ValueError, error):
                    TARGET.bridge(config)
                self.assertTrue(all(call.args[:3] in [('ip', '-json', 'link'), ('ip', '-4', '-json')]
                                    for call in run.call_args_list))

    def test_failed_alias_assignment_removes_only_the_new_bridge(self):
        config = TARGET.configuration(CONFIG)
        error = subprocess.CalledProcessError(1, 'ip')
        with patch.object(TARGET, 'run', side_effect=['[]', '[]', '', error, '']) as run:
            with self.assertRaises(subprocess.CalledProcessError):
                TARGET.bridge(config)
        self.assertEqual(run.call_args_list[-1].args, ('ip', 'link', 'delete', config['bridge']))

    def test_port_probe_matches_bmc_fallback_without_hiding_conflicts(self):
        for failure in ('dual-stack', 'create-v6', 'setup-v6', 'bind'):
            with self.subTest(failure=failure):
                ipv6, ipv4 = Mock(), Mock()
                for listener in (ipv6, ipv4):
                    listener.__enter__ = Mock(return_value=listener)
                    listener.__exit__ = Mock(return_value=False)
                if failure == 'setup-v6':
                    ipv6.setsockopt.side_effect = OSError(errno.ENOPROTOOPT, 'no dual stack')
                if failure == 'bind':
                    ipv6.bind.side_effect = OSError(errno.EADDRINUSE, 'busy')
                creation = [OSError(errno.EAFNOSUPPORT, 'no IPv6') if failure == 'create-v6' else ipv6, ipv4]
                with patch.object(TARGET.socket, 'socket', side_effect=creation) as create:
                    if failure == 'bind':
                        with self.assertRaises(OSError) as error:
                            TARGET.check_bmc_port(CONFIG)
                        self.assertEqual(error.exception.errno, errno.EADDRINUSE)
                        self.assertEqual(create.call_count, 1)
                    elif failure == 'dual-stack':
                        TARGET.check_bmc_port(CONFIG)
                        self.assertEqual(create.call_count, 1)
                        ipv6.setsockopt.assert_any_call(socket.IPPROTO_IPV6, socket.IPV6_V6ONLY, 0)
                        ipv6.bind.assert_called_once_with(('::', CONFIG['bmcPort']))
                        ipv6.listen.assert_called_once()
                    else:
                        TARGET.check_bmc_port(CONFIG)
                        ipv4.bind.assert_called_once_with(('0.0.0.0', CONFIG['bmcPort']))
                        ipv4.listen.assert_called_once()
                        self.assertEqual(create.call_args.args[0], socket.AF_INET)
                        if failure == 'setup-v6':
                            ipv6.close.assert_called_once()

    def test_ipv6_bmc_preflight_checks_local_connectivity(self):
        listener = Mock()
        listener.__enter__ = Mock(return_value=listener)
        listener.__exit__ = Mock(return_value=False)
        with patch.object(TARGET.socket, 'socket', return_value=listener), \
                patch.object(TARGET.socket, 'create_connection', side_effect=OSError(errno.ENETUNREACH, 'IPv6 disabled')) as connect:
            with self.assertRaises(OSError):
                TARGET.check_bmc_port({**CONFIG, 'bmcAddress': 'fd00:10:96::4010'})
            connect.assert_called_once_with(('::1', CONFIG['bmcPort']), timeout=1)

    def test_ipv6_bmc_cannot_fall_back_to_an_ipv4_listener(self):
        with patch.object(TARGET.socket, 'socket', side_effect=OSError(errno.EAFNOSUPPORT, 'no IPv6')) as create:
            with self.assertRaisesRegex(ValueError, 'requires an IPv6 listener'):
                TARGET.check_bmc_port({**CONFIG, 'bmcAddress': 'fd00:10:96::4010'})
            self.assertEqual(create.call_count, 1)

    def test_bad_network_or_busy_port_does_not_claim_fresh_storage(self):
        create_socket = socket.socket

        def ipv4_only(family, kind):
            if family == socket.AF_INET6:
                raise OSError(errno.EAFNOSUPPORT, 'IPv4-only fixture')
            return create_socket(family, kind)

        for failure in ('overlap', 'busy-port'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory, \
                    socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
                listener.bind(('0.0.0.0', 0))
                listener.listen()
                config = {**CONFIG, 'bmcPort': listener.getsockname()[1]}
                config_file = Path(directory) / 'config.json'
                config_file.write_text(json.dumps(config))
                root = Path(directory) / 'data'
                root.mkdir()
                routes = [{'dst': '10.84.0.0/16', 'dev': 'eth0'}] if failure == 'overlap' else []
                with patch.object(TARGET, 'ROOT', root), \
                        patch.object(sys, 'argv', ['target.py', '--config', str(config_file)]), \
                        patch.dict(TARGET.os.environ, NODE_NAME=config['nodeName']), \
                        patch.object(TARGET.platform, 'machine', return_value='aarch64'), \
                        patch.object(TARGET.preflight, 'check_kvm'), \
                        patch.object(TARGET.socket, 'socket', side_effect=ipv4_only), \
                        patch.object(TARGET, 'run', side_effect=['[]', json.dumps(routes)]), \
                        self.assertRaises((ValueError, OSError)):
                    TARGET.main()
                self.assertEqual(list(root.iterdir()), [])

    def test_bmc_stops_before_guest_save_and_libvirt_shutdown(self):
        for hugepages, saved in ((False, False), (False, True), (True, False), (True, True)):
            with self.subTest(hugepages=hugepages, saved=saved):
                events = []
                config = TARGET.configuration(dict(CONFIG, hugepages=hugepages))

                class Child:
                    def __init__(self, command):
                        self.name = command[0]
                    def poll(self):
                        return None
                    def terminate(self):
                        events.append(('stop', self.name))
                    def wait(self, timeout):
                        return 0

                def virsh(*args, **kwargs):
                    events.append(('virsh', args[0]))
                    return {'list': 'target', 'domuuid': config['uuid'], 'domstate': 'running', 'dumpxml': TARGET.domain_xml(config), 'managedsave-dumpxml': TARGET.domain_xml(config)}.get(args[0], '')

                with tempfile.TemporaryDirectory() as directory, patch.object(TARGET, 'ROOT', Path(directory)), \
                        patch.object(TARGET, 'READY', Path(directory) / 'ready'), \
                        patch.object(TARGET, 'prepare_storage'), patch.object(TARGET, 'bridge'), \
                        patch.object(TARGET, 'clear_bridge'), \
                        patch.object(TARGET.Path, 'mkdir'), \
                        patch.object(TARGET, 'virsh', side_effect=virsh), \
                        patch.object(TARGET.subprocess, 'Popen', side_effect=Child), \
                        patch.object(TARGET.socket, 'create_connection'), \
                        patch.object(TARGET.time, 'sleep', side_effect=lambda _: setattr(TARGET, 'STOP', True)), \
                        patch.object(TARGET, 'STOP', False):
                    (Path(directory) / 'bmc.json').write_text('{}')
                    network = Mock()
                    network.start.return_value = [Child(['dhcrelay'])]
                    network.clear.side_effect = lambda: events.append(('clear', 'network'))
                    if saved:
                        save_path = Path(directory) / 'libvirt/qemu/save/target.save'
                        os.makedirs(save_path.parent)
                        save_path.write_bytes(b'saved guest memory')
                    TARGET.serve(config, network)
                self.assertLess(events.index(('stop', 'bmc-mock')), events.index(('virsh', 'managedsave')))
                self.assertLess(events.index(('virsh', 'managedsave')), events.index(('stop', 'libvirtd')))
                self.assertLess(events.index(('stop', 'libvirtd')), events.index(('stop', 'dhcrelay')))
                self.assertLess(events.index(('stop', 'dhcrelay')), events.index(('clear', 'network')))
                self.assertNotIn(('virsh', 'start'), events)
                self.assertEqual(('virsh', 'managedsave-dumpxml') in events, saved)


class ProvisioningNetworkTest(unittest.TestCase):
    def network(self, run=None, **kwargs):
        return TARGET.Network(TARGET.configuration(CONFIG), run or Mock(), **kwargs)

    def test_dhcpv6_requires_a_routed_bridge_and_unscoped_remote_server(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        for selected, server in [(TARGET.configuration(CONFIG), 'fd00::20'),
                (config, 'fe80::1'), (config, 'ff02::1:2'), (config, '::1'),
                (config, 'fd42:84:40::10'), (config, 'fd00::20%eth0')]:
            with self.subTest(server=server), self.assertRaises(ValueError):
                TARGET.Network(selected, Mock(), server_v6=server, pod_cidr_v6="fd00:10:244::/64")

    def test_dhcpv6_preserves_relay_source_port_to_pods_and_validates_the_pod_cidr(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        network = TARGET.Network(config, Mock(), server_v6='fd00::20', pod_cidr_v6='fd00:10:244::/64')
        rules = list(network.ipv6_rules())
        self.assertIn(('nat', 'POSTROUTING', ['-s', 'fd42:84:40::/64', '-d', 'fd00:10:244::/64',
                        '-m', 'comment', '--comment', network.owner, '-j', 'ACCEPT']), rules)
        for server, cidr in [('fd00::20', ''), ('', 'fd00:10:244::/64'),
                             ('fd00::20', '::/0'), ('fd00::20', 'fd42:84:40::/64')]:
            with self.subTest(server=server, cidr=cidr), self.assertRaises(ValueError):
                TARGET.Network(config, Mock(), server_v6=server, pod_cidr_v6=cidr)

    def test_router_advertisements_are_managed_and_do_not_allocate_slaac_addresses(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        network = TARGET.Network(config, Mock(), server_v6='fd00::20', pod_cidr_v6='fd00:10:244::/64')
        advertisement = network.router_advertisement()
        self.assertIn('interface ' + config['bridge'], advertisement)
        self.assertIn('AdvManagedFlag on;', advertisement)
        self.assertIn('AdvAutonomous off;', advertisement)
        self.assertIn('prefix fd42:84:40::/64', advertisement)
        self.assertIn('AdvDefaultLifetime 0;', advertisement)
        with self.assertRaisesRegex(ValueError, 'DHCPv6'):
            TARGET.Network(config, Mock(), default_route_v6=True)

    def test_dhcpv6_only_preflight_requires_matching_kube_proxy_backend(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        run = Mock(return_value='')
        network = TARGET.Network(config, run, server_v6='fd00::20', pod_cidr_v6='fd00:10:244::/64')
        with patch('network.Path.read_text', return_value='1'), patch('network.Path.exists', return_value=False):
            with self.assertRaisesRegex(ValueError, 'DHCPv6 requires kube-proxy'):
                network.preflight()
            run.return_value = ':KUBE-POSTROUTING - [0:0]'
            network.preflight()
        self.assertTrue(all(call.args[0] == 'ip6tables-save' for call in run.call_args_list))

    def test_dhcpv6_partial_start_stops_relay_before_removing_owned_network(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        network = TARGET.Network(config, Mock(return_value=''), server='10.96.0.20', pod_cidr='10.244.0.0/24', server_v6='fd00::20', pod_cidr_v6='fd00:10:244::/64')
        events = []
        ipv4 = Mock()
        ipv4.poll.return_value = None
        ipv4.terminate.side_effect = lambda: events.append('stop IPv4')
        child = Mock()
        child.poll.return_value = None
        child.terminate.side_effect = lambda: events.append('stop')
        with patch.object(network, 'clear', side_effect=lambda: events.append('clear')), \
                patch('network.Path.write_text') as write, \
                patch.object(network, 'upstreams', return_value=['eth0']), \
                patch('network.Relay', return_value=ipv4), \
                patch('network.subprocess.Popen', side_effect=[child, OSError('radvd failed')]) as spawn:
            with self.assertRaisesRegex(OSError, 'radvd failed'):
                network.start()
        self.assertEqual(events, ['clear', 'stop', 'stop IPv4', 'clear'])
        child.wait.assert_called_once_with(timeout=20)
        command = spawn.call_args_list[0].args[0]
        self.assertEqual(command[-6:], ['--link-address', 'fd42:84:40::1', '--server', 'fd00::20', '--mac', config['mac']])
        self.assertIn('AdvAutonomous off;', write.call_args.args[0])

    def test_ipv6_rules_work_without_ipv4_relay_and_preserve_sources(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        run = Mock(return_value='')
        network = TARGET.Network(config, run)
        with patch('network.subprocess.Popen') as spawn:
            self.assertEqual(network.start(), [])
        spawn.assert_not_called()
        rules = [call.args for call in run.call_args_list if call.args[0] == 'ip6tables']
        self.assertEqual(len(rules), 3)
        self.assertTrue(all('fd42:84:40::/64' in rule and network.owner in rule for rule in rules))
        self.assertTrue(all(rule[-1] == 'ACCEPT' for rule in rules))
        self.assertEqual(rules[-1][8:12], ('-d', 'fd42:84:40::/64', '-o', config['bridge']))

    def test_partial_ipv6_install_rolls_back_owned_rules_and_keeps_foreign_rules(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        foreign = '-A FORWARD -s fd42:84:41::/64 -m comment --comment other-target -j ACCEPT'
        tables = {('iptables', 'filter'): [], ('iptables', 'nat'): [],
                  ('ip6tables', 'filter'): [foreign], ('ip6tables', 'nat'): []}
        inserted = []
        def run(*args):
            if args[0].endswith('-save'):
                return '\n'.join(tables[args[0][:-5], args[2]])
            table = tables[args[0], args[4]]
            if args[5] == '-I':
                if inserted:
                    raise subprocess.CalledProcessError(4, 'ip6tables')
                line = shlex.join(['-A', args[6], *args[8:]])
                table.append(line)
                inserted.append(line)
            else:
                self.assertEqual(args[5], '-D')
                table.remove(shlex.join(['-A', args[6], *args[7:]]))
            return ''
        network = TARGET.Network(config, run, server='10.96.40.20', pod_cidr='10.244.0.0/24')
        with patch('network.subprocess.Popen') as spawn, self.assertRaises(subprocess.CalledProcessError):
            network.start()
        spawn.assert_not_called()
        self.assertEqual(len(inserted), 1)
        self.assertEqual(tables[('ip6tables', 'filter')], [foreign])
        self.assertTrue(all(not value for key, value in tables.items() if key != ('ip6tables', 'filter')))

    def test_ipv6_bad_owned_rule_prevents_deletion_in_either_family(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        network = TARGET.Network(config, Mock())
        own = f'-A FORWARD -s 10.84.40.0/24 -m comment --comment {network.owner} -j ACCEPT'
        bad = f'-A FORWARD -s fd42:84:41::/64 -m comment --comment {network.owner} -j ACCEPT'
        network.run.side_effect = [own, '', bad]
        with self.assertRaisesRegex(ValueError, 'retained guest subnet'):
            network.clear()
        self.assertTrue(all(call.args[0].endswith('-save') for call in network.run.call_args_list))

    def test_ipv6_preflight_requires_forwarding_without_host_changes(self):
        config = TARGET.configuration({**CONFIG, 'bridgeAddressV6': 'fd42:84:40::1/64'})
        network = TARGET.Network(config, Mock(return_value=''))
        for forwarding, disabled, error in [('0', '0', 'forwarding'), ('1', '1', 'enabled')]:
            with self.subTest(error=error), patch('network.Path.exists', return_value=True), \
                    patch('network.Path.read_text', side_effect=[forwarding, disabled]), \
                    self.assertRaisesRegex(ValueError, error):
                network.preflight()
        network.run.assert_not_called()

    def test_invalid_network_inputs_fail_without_commands(self):
        for server, cidr, uplink in [('255.255.255.255', '10.244.0.0/24', 'eth0'), ('10.96.40.20', '', 'eth0'), ('', '10.244.0.0/24', 'eth0'),
                                     ('::1', '10.244.0.0/24', 'eth0'), ('10.84.40.5', '10.244.0.0/24', 'eth0'),
                                     ('10.96.40.20', '10.84.40.0/24', 'eth0'), ('10.96.40.20', '0.0.0.0/0', 'eth0'),
                                     ('10.96.40.20', '10.244.0.0/24', 'invalid/name')]:
            with self.subTest(server=server, cidr=cidr, uplink=uplink), self.assertRaises(ValueError):
                self.network(server=server, pod_cidr=cidr, uplink=uplink)

    def test_rules_preserve_pod_sources_before_uplink_masquerade(self):
        network = self.network(server='10.96.40.20', pod_cidr='10.244.0.0/24')
        calls = []
        def run(*args):
            calls.append(args)
            return '[]' if args[0] == 'ip' else ''
        network.run = run
        with patch('network.subprocess.Popen') as spawn:
            network.start()
        insertions = [call for call in calls if call[0] == 'iptables']
        self.assertEqual(len(insertions), 5)
        nat = [call for call in insertions if call[4] == 'nat']
        self.assertEqual([call[-1] for call in nat], ['MASQUERADE', 'ACCEPT', 'ACCEPT'])
        self.assertTrue(all(call[7] == '1' for call in nat))
        self.assertIn('10.244.0.0/24', nat[-2])
        self.assertEqual(nat[-1][8:12], ('-d', '10.84.40.0/24', '-o', network.config['bridge']))
        self.assertTrue(all(network.owner in call for call in insertions))
        self.assertEqual(spawn.call_args.args[0][-5:], ['-id', network.config['bridge'], '-iu', 'eth0', '10.96.40.20'])

    def test_cleanup_removes_only_owned_rules_and_rejects_unknown_owned_shapes_before_deleting(self):
        network = self.network()
        tag = network.owner
        own = f'-A FORWARD -s 10.84.40.0/24 -m comment --comment "{tag}" -j ACCEPT'
        incoming = f'-A POSTROUTING -d 10.84.40.0/24 -o {network.config["bridge"]} -m comment --comment "{tag}" -j ACCEPT'
        foreign = '-A FORWARD -s 10.84.41.0/24 -m comment --comment other-target -j ACCEPT'
        for bad in ('', f'-A OTHER -s 10.84.40.0/24 -m comment --comment "{tag}" -j DROP'):
            with self.subTest(bad=bad):
                commands = []
                def run(*args):
                    commands.append(args)
                    return '\n'.join([own, foreign, bad]) if args == ('iptables-save', '-t', 'filter') else incoming
                network.run = run
                if bad:
                    with self.assertRaises(ValueError):
                        network.clear()
                    self.assertFalse(any(command[0] == 'iptables' for command in commands))
                else:
                    network.start()  # Disabled mode cleans an earlier interrupted enabled run.
                    deletes = [command for command in commands if command[0] == 'iptables']
                    self.assertEqual(len(deletes), 2)
                    self.assertIn('-d', deletes[1])
                    self.assertIn('-D', deletes[0])
                    self.assertIn(tag, deletes[0])
                    self.assertNotIn('other-target', deletes[0])

    def test_partial_install_failure_cleans_owned_rules_without_starting_relay(self):
        network = self.network(server='10.96.40.20', pod_cidr='10.244.0.0/24')
        with patch.object(network, 'clear') as clear, \
                patch.object(network, 'run', side_effect=[None, subprocess.CalledProcessError(4, 'iptables')]), \
                patch('network.subprocess.Popen') as spawn:
            with self.assertRaises(subprocess.CalledProcessError):
                network.start()
        self.assertEqual(clear.call_count, 2)
        spawn.assert_not_called()

    def test_preflight_rejects_missing_host_contracts_without_mutation(self):
        for forwarding, links, nat, message in [
                ('0', [], '', 'forwarding'),
                ('1', [{'flags': []}], '', 'uplink'),
                ('1', [{'flags': ['UP']}], '', 'iptables mode')]:
            with self.subTest(message=message), patch('network.Path.read_text', return_value=forwarding):
                run = Mock(side_effect=[json.dumps(links), nat])
                network = self.network(run, server='10.96.40.20', pod_cidr='10.244.0.0/24')
                with self.assertRaisesRegex(ValueError, message):
                    network.preflight()
                self.assertFalse(any(call.args[0] == 'iptables' for call in run.call_args_list))

    def test_relay_tracks_pod_interfaces_without_restarting_after_process_failure(self):
        network = self.network(server='10.96.40.20', pod_cidr='10.244.0.0/24')
        network.run.return_value = json.dumps([
            {'ifname': 'pod-old', 'flags': ['UP'], 'addr_info': [{'family': 'inet', 'local': '10.244.0.1'}]},
            {'ifname': 'foreign', 'flags': ['UP'], 'addr_info': [{'family': 'inet', 'local': '10.1.0.1'}]},
            {'ifname': 'down', 'flags': [], 'addr_info': [{'family': 'inet', 'local': '10.244.0.1'}]}])
        self.assertEqual(network.upstreams(), ['eth0', 'pod-old'])
        from network import Relay
        old, new = Mock(), Mock()
        old.poll.return_value = None
        new.poll.return_value = None
        with patch('network.subprocess.Popen', side_effect=[old, new]) as spawn:
            relay = Relay(network.config['bridge'], str(network.server), network.upstreams())
            relay.refresh(['eth0', 'pod-old'])
            self.assertEqual(spawn.call_count, 1)
            relay.refresh(['eth0', 'pod-new'])
            old.terminate.assert_called_once()
            old.wait.assert_called_once_with(timeout=5)
            self.assertIn('pod-new', spawn.call_args.args[0])
            self.assertNotIn('pod-old', spawn.call_args.args[0])
            new.poll.return_value = 1
            relay.refresh(['eth0', 'pod-next'])
            self.assertEqual(spawn.call_count, 2)
            self.assertEqual(relay.poll(), 1)

    def test_capture_requires_the_requested_guest_and_optional_lease(self):
        import contextlib
        import io
        spec = importlib.util.spec_from_file_location('test_target_cli', LAB / 'test-target.py')
        cli = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cli)
        config = TARGET.configuration(CONFIG)
        def packet(mac, address, kind):
            frame = bytearray(342)
            frame[12:14] = bytes.fromhex('0800')
            frame[14] = 0x45
            frame[23] = 17
            frame[36:38] = (68).to_bytes(2, 'big')
            frame[42] = 2
            frame[58:62] = socket.inet_aton(address)
            frame[70:76] = bytes.fromhex(mac.replace(':', ''))
            frame[278:282] = bytes.fromhex('63825363')
            frame[282:286] = bytes([53, 1, kind, 255])
            return bytes(frame)
        frames = [packet('02:00:00:02:00:02', '10.84.40.100', 5),
                  packet(config['mac'], '10.84.40.101', 5),
                  packet(config['mac'], '10.84.40.100', 2),
                  packet(config['mac'], '10.84.40.100', 5)]
        request = bytearray(frames[-1])
        request[36:38] = (67).to_bytes(2, 'big')
        request[42] = 1
        wrong_source = bytes(request)
        request[6:12] = bytes.fromhex(config['mac'].replace(':', ''))
        for lease, packets, expected in [
                ('10.84.40.100', frames, {'dhcp_mac': config['mac'], 'lease': '10.84.40.100'}),
                ('', [frames[-1], wrong_source, bytes(request)], {'dhcp_mac': config['mac']})]:
            capture = Mock()
            capture.recv.side_effect = packets
            output = io.StringIO()
            with self.subTest(lease=lease), patch('socket.AF_PACKET', 17, create=True), \
                    patch('socket.socket') as create, patch('pathlib.Path.read_text', return_value=json.dumps(config)), \
                    patch.object(sys, 'argv', ['capture', lease]), contextlib.redirect_stdout(output):
                create.return_value.__enter__.return_value = capture
                exec(cli.CAPTURE, {})
            self.assertEqual(capture.recv.call_count, len(packets))
            self.assertEqual(json.loads(output.getvalue().splitlines()[-1]), expected)

    def test_rule_cleanup_failure_preserves_startup_error_and_attempts_bridge_cleanup(self):
        network = Mock()
        network.clear.side_effect = OSError('iptables unavailable')
        with tempfile.TemporaryDirectory() as directory, patch.object(TARGET, 'READY', Path(directory) / 'ready'), \
                patch.object(TARGET, 'prepare_storage', side_effect=RuntimeError('original startup failure')), \
                patch.object(TARGET, 'clear_bridge') as clear_bridge, patch('sys.stderr'):
            with self.assertRaisesRegex(RuntimeError, 'original startup failure'):
                TARGET.serve(TARGET.configuration(CONFIG), network)
            clear_bridge.assert_called_once()
