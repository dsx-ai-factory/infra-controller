# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import json
import os
import subprocess
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[3]
LAB = load_script('lifecycle', ROOT / 'dev/deployment/lab/acceptance/lifecycle.py')
INSTANCE = '81240030-0000-4000-8000-000000000001'


class LifecycleTest(unittest.TestCase):
    def setUp(self):
        self.target = dict(machine='machine-one', namespace='bmc-lab', release='one', intent='lab-test-1-one')
        self.current = dict(id=INSTANCE, machine_id='machine-one', metadata={'name': 'lab-test-1-one_0'})
        self.state = dict(context='kind-lab', core_namespace='nico-system', targets=[self.target], timeout=60,
                          generation=1, network='ipv6-only')

    def test_cleanup_recovers_lost_allocation_response_but_never_adopts_a_replacement(self):
        cases = [('matching', self.current, True),
                 ('foreign name', dict(self.current, metadata={'name': 'unrelated'}), False),
                 ('replaced ID', dict(self.current, id='81240030-0000-4000-8000-000000000002'), False)]
        for name, current, allowed in cases:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as directory:
                target = dict(self.target)
                if name == 'replaced ID':
                    target['instance'] = INSTANCE
                lab = LAB.Lifecycle(Path(directory), dict(self.state, targets=[target]))
                lab.admin = Mock(return_value={'instances': [current]})
                lab.mutation = Mock()
                lab.wait = Mock(return_value=True)
                if allowed:
                    def mutation(*args):
                        recorded = json.loads((Path(directory) / 'state.json').read_text())
                        self.assertEqual(recorded['targets'][0]['instance'], INSTANCE)
                    lab.mutation.side_effect = mutation
                    lab.release(target)
                    lab.mutation.assert_called_once_with('instance', 'release', '--instance', INSTANCE)
                    self.assertNotIn('intent', target)
                else:
                    with self.assertRaises(ValueError):
                        lab.release(target)
                    lab.mutation.assert_not_called()

    def test_initial_readiness_waits_for_reconnecting_scout_before_allocation(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'run'
            lab = LAB.Lifecycle(destination, dict(self.state, targets=[self.target], timeout=60))
            lab.verify_ownership = Mock()
            lab.preflight_network = Mock()
            lab.ready = Mock(side_effect=[RuntimeError('Scout is reconnecting'), False, True, True])
            lab.exercise = Mock(side_effect=lambda: self.assertEqual(lab.ready.call_count, 4))
            lab.diagnostics = Mock()
            args = ['lifecycle.py', '--context', 'kind-test', '--state-dir', str(destination),
                    '--target', 'one=machine-one', '--target', 'two=machine-two',
                    '--os-image', INSTANCE, '--vpc', INSTANCE]
            with patch.object(sys, 'argv', args), patch.object(LAB, 'Lifecycle', return_value=lab), \
                    patch.object(LAB, 'run', return_value='{"metadata":{"uid":"cluster"}}'), \
                    patch.object(LAB.time, 'sleep'):
                LAB.main()
            self.assertEqual(lab.ready.call_count, 4)
            lab.exercise.assert_called_once()
            lab.diagnostics.assert_not_called()
            lab.exercise.reset_mock()
            lab.ready = Mock(return_value=False)
            with patch.object(sys, 'argv', [str(destination) + '-timeout' if a == str(destination) else a for a in args]), \
                    patch.object(LAB, 'Lifecycle', return_value=lab), \
                    patch.object(LAB, 'run', return_value='{"metadata":{"uid":"cluster"}}'), \
                    patch.object(LAB.time, 'monotonic', side_effect=[0, 0, 61]), patch.object(LAB.time, 'sleep'), \
                    self.assertRaises(TimeoutError):
                LAB.main()
            lab.exercise.assert_not_called()
            lab.diagnostics.assert_called_once()

    def test_inventory_rejects_cross_cluster_and_wrong_bmc_before_adoption(self):
        inventory = dict(clusterUID='cluster', owner='bmc-lab/one', bmcAddress='fd00::12', instanceIndex=3)
        machine = dict(id='machine-one', bmc_info={'ip': 'fd00::12', 'mac': '02:00:00:03:00:01'})
        LAB.validate_target(self.target, inventory, machine, 'cluster')
        for changes in ({'clusterUID': 'different'}, {'owner': 'bmc-lab/two'}, {'bmcAddress': 'fd00::13'}):
            with self.subTest(changes=changes), self.assertRaises(ValueError):
                LAB.validate_target(self.target, dict(inventory, **changes), machine, 'cluster')

    def test_disk_readiness_pins_only_verified_generation_without_console_history(self):
        with tempfile.TemporaryDirectory() as directory:
            lab = LAB.Lifecycle(Path(directory), self.state)
            target = dict(self.target, console_offset=2000000, addresses={'6': 'fd42:1::a'})
            lab.kube = Mock(side_effect=AssertionError('rotated console is not a readiness contract'))
            lab.checkpoint = Mock()
            pinned = Path(directory) / f'known-hosts-{self.state["generation"]}-{target["release"]}'
            for outcome in ('wrong disk or generation', 'invalid boot ID', 'missing host key', 'success'):
                def ssh(target, command, known_hosts=None, address=None):
                    self.assertNotEqual(known_hosts, pinned)
                    self.assertIn('findmnt -no SOURCE /', command)
                    self.assertIn('cloud-init status --wait', command)
                    self.assertIn('test ! -e /etc/nico-peer-retained', command)
                    self.assertIn(target['intent'], command)
                    known_hosts.write_text('' if outcome == 'missing host key' else 'verified-host-key\n')
                    if outcome == 'wrong disk or generation':
                        raise RuntimeError('guest verification failed')
                    return 'invalid' if outcome == 'invalid boot ID' else INSTANCE + '\n'
                lab.ssh = Mock(side_effect=ssh)
                with self.subTest(outcome=outcome):
                    if outcome == 'success':
                        self.assertTrue(lab.installed(target))
                        self.assertEqual(pinned.read_text(), 'verified-host-key\n')
                    else:
                        with self.assertRaises((RuntimeError, ValueError)):
                            lab.installed(target)
                        self.assertFalse(pinned.exists())
                    self.assertFalse(list(Path(directory).glob('probe-*')))
            lab.ssh = Mock(return_value=INSTANCE + '\n')
            self.assertTrue(lab.installed(target))
            self.assertNotEqual(lab.ssh.call_args.kwargs['known_hosts'], pinned)
            self.assertEqual(pinned.read_text(), 'verified-host-key\n')

    def test_ipv6_only_rejects_ipv4_state_and_requires_a_working_negative_probe(self):
        for ipv4, route, ping_status, accepted in ((True, False, 1, False), (False, True, 1, False),
                                                 (False, False, 0, False), (False, False, 1, True),
                                                 (False, False, None, False)):
            with self.subTest(ipv4=ipv4, route=route, ping_status=ping_status), tempfile.TemporaryDirectory() as directory:
                path = Path(directory)
                if ping_status is not None:
                    ping = path / 'ping'
                    ping.write_text('#!/bin/sh\nexit ' + str(ping_status) + '\n')
                    ping.chmod(0o700)
                target = dict(self.target, addresses={'6': 'fd42:1::a'})
                lab = LAB.Lifecycle(path, dict(self.state, internet_ipv6=False))
                addresses = [{'family': 'inet6', 'local': 'fd42:1::a', 'valid_life_time': 3600}]
                if ipv4:
                    addresses.append({'family': 'inet', 'local': '10.1.0.10'})
                def ssh(target, command):
                    if command == 'ip -j address':
                        return json.dumps([{'ifname': 'eth0', 'addr_info': addresses}])
                    if command == 'ip -j -4 route show table all':
                        return json.dumps([{'dev': 'eth0', 'dst': 'default'}] if route else [])
                    return subprocess.check_output(['/bin/sh', '-c', command], env=dict(os.environ, PATH=str(path)), text=True)
                lab.ssh = Mock(side_effect=ssh)
                if accepted:
                    lab.verify_network(target)
                    self.assertTrue(list(path.glob('addresses-*.json')))
                else:
                    with self.assertRaises((ValueError, subprocess.CalledProcessError)):
                        lab.verify_network(target)
                    self.assertFalse(list(path.glob('addresses-*.json')))

    def test_dual_stack_verifies_each_address_before_publishing_host_keys(self):
        for mismatch in (False, True):
            with self.subTest(mismatch=mismatch), tempfile.TemporaryDirectory() as directory:
                lab = LAB.Lifecycle(Path(directory), self.state)
                target = dict(self.target, addresses={'6': 'fd42:1::a', '4': '10.1.0.10'})
                lab.checkpoint = Mock()
                pinned = lab.host_keys(target)
                pinned.write_text('existing-key\n')
                def ssh(target, command, known_hosts=None, address=None):
                    self.assertIn('existing-key', known_hosts.read_text())
                    with known_hosts.open('a') as stream:
                        stream.write(address + ' verified-key\n')
                    return ('81240030-0000-4000-8000-000000000002' if mismatch and address == '10.1.0.10' else INSTANCE)
                lab.ssh = Mock(side_effect=ssh)
                if mismatch:
                    with self.assertRaisesRegex(ValueError, 'different boots'):
                        lab.installed(target)
                    self.assertEqual(pinned.read_text(), 'existing-key\n')
                    lab.checkpoint.assert_not_called()
                else:
                    self.assertTrue(lab.installed(target))
                    for address in target['addresses'].values():
                        self.assertIn(address + ' verified-key', pinned.read_text())
                self.assertEqual([c.kwargs['address'] for c in lab.ssh.call_args_list], ['fd42:1::a', '10.1.0.10'])

    def test_allocation_intent_survives_lost_response(self):
        with tempfile.TemporaryDirectory() as directory:
            target = dict(self.target, inventory={'mac': '02:00:00:03:00:02'})
            state = dict(self.state, targets=[target], run='lab-test', os_image=INSTANCE, vpc=INSTANCE)
            lab = LAB.Lifecycle(Path(directory), state)
            lab.wait = Mock(return_value=True)
            lab.kube = Mock(return_value='previous console')
            lab.mutation = Mock(side_effect=RuntimeError('connection lost'))
            lab.instance = Mock()
            with patch.object(LAB, 'run', return_value='{}'), self.assertRaises(RuntimeError):
                lab.allocate(target)
            saved = json.loads((Path(directory) / 'state.json').read_text())['targets'][0]
            self.assertEqual(saved['intent'], 'lab-test-1-one')
            self.assertNotIn('instance', saved)
            lab.instance.assert_not_called()

    def test_failed_intent_flush_prevents_allocation_and_preserves_previous_journal(self):
        with tempfile.TemporaryDirectory() as directory:
            target = dict(self.target, inventory={'mac': '02:00:00:03:00:02'})
            state = dict(self.state, targets=[target], run='lab-test', os_image=INSTANCE, vpc=INSTANCE)
            lab = LAB.Lifecycle(Path(directory), state)
            lab.checkpoint()
            previous = (Path(directory) / 'state.json').read_bytes()
            lab.wait = Mock(return_value=True)
            lab.mutation = Mock(side_effect=AssertionError('allocation started before flushing its intent'))
            with patch.object(LAB, 'run', return_value='{}'), patch.object(LAB.os, 'fsync', side_effect=OSError('flush failed')), self.assertRaisesRegex(OSError, 'flush failed'):
                lab.allocate(target)
            lab.mutation.assert_not_called()
            self.assertEqual((Path(directory) / 'state.json').read_bytes(), previous)

    def test_addresses_must_belong_to_the_selected_target(self):
        inventory = {'bridgeAddressV6': 'fd42:1::1/64'}
        for addresses, valid in [(['fd42:1::a'], True), (['fd42:2::a'], False), (['fd42:1::1'], False),
                                 (['fd42:1::a', 'fd42:1::b'], False)]:
            with self.subTest(addresses=addresses):
                instance = {'status': {'network': {'interfaces': [{'addresses': addresses}]}}}
                if valid:
                    self.assertEqual(LAB.selected_addresses(instance, 'ipv6-only', inventory), {'6': 'fd42:1::a'})
                else:
                    with self.assertRaises(ValueError):
                        LAB.selected_addresses(instance, 'ipv6-only', inventory)

    def test_cleanup_attempts_the_peer_after_one_target_fails(self):
        for boundary in ('ownership', 'release'):
            with self.subTest(boundary=boundary), tempfile.TemporaryDirectory() as directory:
                second = dict(self.target, release='two', machine='machine-two')
                lab = LAB.Lifecycle(Path(directory), dict(self.state, targets=[self.target, second]))
                lab.verify_ownership = Mock(side_effect=[ValueError('wrong target'), None] if boundary == 'ownership' else None)
                lab.release = Mock(side_effect=[TimeoutError('still cleaning'), None] if boundary == 'release' else None)
                lab.diagnostics = Mock()
                with self.assertRaisesRegex(RuntimeError, 'cleanup incomplete'):
                    lab.cleanup()
                self.assertEqual(lab.verify_ownership.call_count, 2)
                self.assertEqual(lab.release.call_args.args, (second,))
                lab.diagnostics.assert_called_once()

    def test_cluster_preflight_failure_does_not_consume_new_run_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'new-run'
            args = ['lifecycle.py', '--state-dir', str(destination), '--context', 'kind-test',
                    '--vpc', INSTANCE, '--os-image', INSTANCE, '--target', 'one=machine-one', '--target', 'two=machine-two']
            with patch.object(sys, 'argv', args), patch.object(LAB, 'run', side_effect=RuntimeError('cluster unavailable')), \
                    self.assertRaisesRegex(RuntimeError, 'cluster unavailable'):
                LAB.main()
            self.assertFalse(destination.exists())

    def test_ipv6_preflight_rejects_missing_bridge_without_any_command(self):
        target = dict(self.target, inventory={'bridgeAddress': '10.0.0.1/24'})
        lab = LAB.Lifecycle(Path('/unused'), dict(self.state, targets=[target]))
        with patch.object(LAB, 'run') as run, self.assertRaisesRegex(ValueError, 'IPv6 bridge'):
            lab.preflight_network()
        run.assert_not_called()

    def test_console_preserves_readable_evidence_for_non_utf8_bytes(self):
        output = LAB.run([__import__('sys').executable, '-c', 'import sys; sys.stdout.buffer.write(b"prefix\\xffsuffix")'], errors='replace')
        self.assertEqual(output, 'prefix' + chr(0xfffd) + 'suffix')

    def test_cleanup_of_a_completed_journal_does_not_require_live_targets(self):
        with tempfile.TemporaryDirectory() as directory:
            target = dict(self.target)
            target.pop('intent')
            lab = LAB.Lifecycle(Path(directory), dict(self.state, targets=[target]))
            lab.verify_ownership = Mock(side_effect=ValueError('gone'))
            lab.release = Mock()
            lab.cleanup()
            lab.verify_ownership.assert_not_called()
            lab.release.assert_not_called()

    def test_failed_allocation_preserves_the_cli_explanation(self):
        with tempfile.TemporaryDirectory() as directory:
            target = dict(self.target, inventory={'mac': '02:00:00:03:00:02'})
            lab = LAB.Lifecycle(Path(directory), dict(self.state, targets=[target], run='lab-test', os_image=INSTANCE, vpc=INSTANCE))
            lab.wait = Mock(return_value=True)
            lab.kube = Mock(return_value='')
            lab.instance = Mock(side_effect=RuntimeError('Unknown Instance ID'))
            admin = Path(directory) / 'admin.sh'
            admin.write_text('echo "No available machines" >&2\nexit 0\n')
            real_run = LAB.run
            def run(command, **kwargs):
                return '{}' if 'guest-config.py' in str(command[1]) else real_run(command, **kwargs)
            with patch.object(LAB, 'ADMIN', admin), patch.object(LAB, 'run', side_effect=run), self.assertRaises(RuntimeError):
                lab.allocate(target)
            self.assertEqual((Path(directory) / 'allocate-1-one.log').read_text(), 'No available machines\n')

    def test_reboot_waits_for_both_leases_after_ipv6_ssh_returns(self):
        target = dict(self.target, instance=INSTANCE, addresses={'4': '10.0.0.10', '6': 'fd00::10'})
        lab = LAB.Lifecycle(Path('/unused'), dict(self.state, network='dual-stack'))
        lab.instance = Mock()
        lab.mutation = Mock()
        before, after, peer_boot = (f'81240030-0000-4000-8000-{n:012d}' for n in (1, 2, 3))
        def lease(address):
            return {'local': address, 'valid_life_time': 180}
        lab.ssh = Mock(side_effect=[before, peer_boot, after,
                                   json.dumps([{'addr_info': [lease('fd00::10')]}]), after,
                                   json.dumps([{'addr_info': [lease('fd00::10'), lease('10.0.0.10')]}]), peer_boot])
        def wait(label, check):
            self.assertFalse(check())
            self.assertTrue(check())
        lab.wait = wait
        lab.reboot(target, dict(target, release='two'))
        self.assertEqual(lab.ssh.call_count, 7)

    def test_allocation_command_failure_and_timeout_keep_partial_diagnostics(self):
        import subprocess
        import sys
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / 'allocate.log'
            for suffix, error in [('sys.exit(3)', RuntimeError), ('__import__("time").sleep(30)', subprocess.TimeoutExpired)]:
                with self.subTest(suffix=suffix), self.assertRaises(error) as raised:
                    LAB.run([sys.executable, '-c', 'import sys; print("allocation refused", file=sys.stderr, flush=True); ' + suffix], timeout=1, log_path=log)
                self.assertEqual(log.read_text(), 'allocation refused\n')
                if error is RuntimeError:
                    self.assertIn('allocation refused', str(raised.exception))

    def test_invalid_lease_fails_immediately_instead_of_waiting(self):
        target = dict(self.target, addresses={'6': 'fd00::a'})
        lab = LAB.Lifecycle(Path('/unused'), self.state)
        finite = {'local': 'fd00::a', 'valid_life_time': 180}
        for leases in ([dict(finite, valid_life_time=4294967295)], [finite, finite]):
            with self.subTest(leases=leases), self.assertRaisesRegex(ValueError, 'unique finite'):
                lab.leases_ready(target, [{'addr_info': leases}])

    def test_diagnostics_preserve_prior_snapshots_and_skip_replaced_targets(self):
        with tempfile.TemporaryDirectory() as directory:
            lab = LAB.Lifecycle(Path(directory), self.state)
            lab.verify_ownership = Mock()
            lab.kube = Mock(return_value='first console')
            lab.admin = Mock(return_value={'state': 'first'})
            lab.diagnostics()
            first = next(Path(directory).glob('diagnostics-*'))
            lab.verify_ownership.side_effect = ValueError('replaced')
            lab.kube.reset_mock()
            lab.admin.reset_mock()
            lab.diagnostics()
            self.assertEqual(len(list(Path(directory).glob('diagnostics-*'))), 2)
            self.assertEqual((first / 'one-console.log').read_text(), 'first console')
            lab.kube.assert_not_called()
            lab.admin.assert_not_called()


if __name__ == '__main__':
    unittest.main()
