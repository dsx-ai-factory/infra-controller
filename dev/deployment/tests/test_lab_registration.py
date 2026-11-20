# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
from deployment_test_support import load_script
import contextlib
import copy
import io
import json
from pathlib import Path
import sys
import tempfile
import subprocess
import traceback
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1] / 'lab/acceptance'
REG = load_script('lab_registration', ROOT / 'register.py')


def inventories():
    return {name: dict(clusterUID='cluster', owner='bmc-lab/' + name, architecture='arm64',
                       instanceIndex=index, bmcAddress=f'fd00:10:96::401{index}',
                       mac=f'02:00:00:0{index}:00:02', serial=f'0200000{index}0001',
                       bridgeAddress=f'10.84.{index}.1/24', bridgeAddressV6=f'fd42:8124:{index}::1/64')
            for name, index in [('one', 1), ('two', 2)]}


class RegistrationTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.directory = Path(directory.name)
        secret = self.directory / 'password'
        secret.write_text('test-only-password\n')
        secret.chmod(0o600)
        segments, targets = REG.plan_targets(inventories(), 'ipv6-only', ['fd00:10:96::4010/124'], 'lab-test', 'domain')
        self.state = dict(version=1, context='kind-test', namespace='nico-system', target_namespace='bmc-lab',
                          cluster='cluster', owner='lab-test', architecture='arm64', segments=segments, targets=targets,
                          timeout=60, bmc_username='root', bmc_password_file=str(secret))
        self.segments, self.expected, self.writes = [], [], []
        self.lab = REG.Registration(self.directory, self.state)
        self.lab.verify_targets = Mock()
        self.lab.admin = Mock(side_effect=self.admin)

    def admin(self, *args, read=False, operation=None):
        if read:
            return {'network_segments': self.segments} if args[0] == 'network-segment' else {'expected_machines': self.expected}
        self.assertTrue(json.loads((self.directory / 'state.json').read_text())[operation.removesuffix('-create') + '_intent'])
        self.writes.append(args)
        resource_id = args[args.index('--id') + 1]
        if args[0] == 'network-segment':
            desired = next(s for s in self.state['segments'] if s['id'] == resource_id)
            self.segments.append(dict(id=resource_id, deleted=None, metadata={'name': desired['name']},
                                      config=dict(mtu=1500, vpc_id=None, **{k: copy.deepcopy(desired[k]) for k in ('segment_type', 'subdomain_id', 'prefixes')}),
                                      status={'lifecycle': {'state': '{"state":"ready"}'}}))
        else:
            target = next(t for t in self.state['targets'] if t['expected_id'] == resource_id)
            inv = target['inventory']
            self.expected.append(dict(id={'value': resource_id}, bmc_mac_address=target['bmc_mac'], bmc_ip_address=inv['bmcAddress'],
                                      chassis_serial_number=inv['serial'], dpu_mode=3, dpf_enabled=False,
                                      metadata={'name': target['name'], 'description': self.state['owner']},
                                      interfaces=[dict(mac_address=inv['mac'], primary=True, network_segment_type=3)]))

    def test_registration_then_resume_does_not_overwrite_rotated_credentials(self):
        with contextlib.redirect_stdout(io.StringIO()):
            self.lab.apply()
            for expected in self.expected:
                expected['bmc_password'] = 'rotated'
            Path(self.state['bmc_password_file']).unlink()
            self.lab.apply()
        self.assertEqual(len(self.writes), 5)
        self.assertEqual([w[0] for w in self.writes], ['network-segment'] * 3 + ['em'] * 2)
        self.assertEqual([e['bmc_password'] for e in self.expected], ['rotated', 'rotated'])
        self.assertNotIn('test-only-password', (self.directory / 'state.json').read_text())
        self.assertTrue(self.state['complete'])
        self.assertNotIn('--gateway', self.writes[1])

    def test_existing_overlapping_segment_stops_before_any_registration_write(self):
        self.segments = [dict(id='other', deleted=None, config={'prefixes': [{'prefix': 'fd42:8124::/32'}]})]
        with self.assertRaisesRegex(ValueError, 'overlaps'):
            self.lab.apply()
        self.assertEqual(self.writes, [])

    def test_shared_identity_or_changed_policy_never_adopts_expected_machine(self):
        with contextlib.redirect_stdout(io.StringIO()): self.lab.apply()
        for change in ('owner', 'id', 'fixed_ip', 'dpu', 'duplicate'):
            original = copy.deepcopy(self.expected)
            if change == 'owner': self.expected[0]['metadata']['description'] = 'someone-else'
            elif change == 'id': self.expected[0]['id']['value'] = 'other'
            elif change == 'fixed_ip': self.expected[0]['interfaces'][0]['fixed_ip'] = 'fd42:8124:1::123'
            elif change == 'dpu': self.expected[0]['dpu_mode'] = 1
            else: self.expected.append(copy.deepcopy(self.expected[0]))
            before = len(self.writes)
            with self.subTest(change=change), self.assertRaises(ValueError): self.lab.apply()
            self.assertEqual(len(self.writes), before)
            self.expected = original

    def test_lost_expected_response_recovers_identity_without_repeating_create(self):
        original = self.admin
        def lost(*args, **kwargs):
            result = original(*args, **kwargs)
            if not kwargs.get('read') and args[0] == 'em': raise RuntimeError('lost response')
            return result
        self.lab.admin.side_effect = lost
        with self.assertRaisesRegex(RuntimeError, 'lost response'): self.lab.apply()
        self.lab.admin.side_effect = original
        with contextlib.redirect_stdout(io.StringIO()): self.lab.apply()
        self.assertEqual(len(self.writes), 5)
        self.assertEqual(len(self.expected), 2)

    def test_removed_segment_during_readiness_blocks_expected_machine_creation(self):
        self.lab.ensure = Mock()
        self.lab.find_segment = Mock(side_effect=[{}] * len(self.state['segments']) + [None])
        with self.assertRaisesRegex(ValueError, 'segment disappeared'):
            self.lab.apply()
        self.assertEqual(self.writes, [])

    def test_changed_target_inventory_blocks_all_core_writes(self):
        self.lab.verify_targets = REG.Registration.verify_targets.__get__(self.lab)
        self.lab.verify_cluster = Mock()
        with patch.object(REG, 'target_inventory', return_value={'owner': 'different'}), self.assertRaisesRegex(ValueError, 'identity'):
            self.lab.apply()
        self.lab.admin.assert_not_called()

    def test_initial_credential_timeout_does_not_print_or_log_password(self):
        secret = 'fixture-password-must-stay-private'
        journal = REG.CoreJournal(self.directory, self.state)
        with patch('core_journal.run', side_effect=subprocess.TimeoutExpired(['cli', '--bmc-password', secret], 90)):
            try:
                journal.admin('em', 'add', '--bmc-password', secret, operation='expected-create')
            except Exception:
                evidence = traceback.format_exc()
            else:
                self.fail('timeout must fail the registration')
        self.assertNotIn(secret, evidence)
        self.assertIn('registration failed', evidence)
        self.assertNotIn(secret, (self.directory / 'expected-create.log').read_text())

    def test_checkpoint_flush_failure_preserves_previous_intents(self):
        self.lab.checkpoint()
        original = (self.directory / 'state.json').read_text()
        self.state['new_intent'] = True
        with patch('core_journal.os.fsync', side_effect=OSError('disk full')), self.assertRaises(OSError):
            self.lab.checkpoint()
        self.assertEqual((self.directory / 'state.json').read_text(), original)

    def test_invalid_initial_credentials_never_create_expected_machines(self):
        secret = Path(self.state['bmc_password_file'])
        for contents, mode, owner in [('', 0o600, REG.os.getuid()), ('one\ntwo', 0o600, REG.os.getuid()),
                                      ('test', 0o644, REG.os.getuid()), ('test', 0o600, REG.os.getuid() + 1)]:
            secret.write_text(contents)
            secret.chmod(mode)
            with self.subTest(contents=contents, mode=mode, owner=owner), patch.object(REG.os, 'getuid', return_value=owner), \
                    self.assertRaisesRegex(ValueError, 'password file'):
                self.lab.apply()
            self.assertEqual(self.expected, [])

    def test_segment_timeout_can_resume_without_recreating_segments(self):
        original = self.admin
        def pending(*args, **kwargs):
            result = original(*args, **kwargs)
            if not kwargs.get('read') and args[0] == 'network-segment':
                self.segments[-1]['status']['lifecycle']['state'] = '{"state":"provisioning"}'
            return result
        self.lab.admin.side_effect = pending
        with patch.object(REG.time, 'monotonic', side_effect=[0, 61]), self.assertRaisesRegex(TimeoutError, 'not Ready'):
            self.lab.apply()
        self.assertEqual(len(self.writes), 3)
        self.assertEqual(self.expected, [])
        self.lab.admin.side_effect = original
        for segment in self.segments:
            segment['status']['lifecycle']['state'] = '{"state":"ready"}'
        with contextlib.redirect_stdout(io.StringIO()):
            self.lab.apply()
        self.assertEqual(len(self.writes), 5)

    def test_dual_stack_segment_commands_keep_gateway_and_domain_policy(self):
        self.state['segments'], self.state['targets'] = REG.plan_targets(
            inventories(), 'dual-stack', ['fd00:10:96::4010/124'], 'lab-test', 'domain')
        with contextlib.redirect_stdout(io.StringIO()):
            self.lab.apply()
        for flag in ('--gateway', '--subdomain-id', '--reserve-first'):
            self.assertNotIn(flag, self.writes[0])
        for command, index in zip(self.writes[1:3], (1, 2)):
            self.assertEqual(command[command.index('--gateway') + 1], f'10.84.{index}.1')
            self.assertEqual(command[command.index('--reserve-first') + 1], '10')
            self.assertEqual(command[command.index('--subdomain-id') + 1], 'domain')
            self.assertEqual(command.count('--prefix'), 2)

    def test_invalid_cli_and_read_only_preflight_leave_no_journal(self):
        destination = self.directory / 'new-journal'
        valid = ['register.py', '--state-dir', str(destination), '--context', 'kind-test',
                 '--target', 'one', '--target', 'two', '--bmc-prefix', 'fd00:10:96::4010/124',
                 '--bmc-password-file', self.state['bmc_password_file']]
        cases = [valid + ['--resume'], [v if v != 'kind-test' else 'production' for v in valid],
                 valid + ['--target', 'three'], [v if v != 'two' else 'one' for v in valid],
                 [v if v != 'two' else 'Invalid' for v in valid], valid + ['--timeout', '0']]
        for args in cases:
            with self.subTest(args=args), patch.object(sys, 'argv', args), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                REG.main()
            self.assertFalse(destination.exists())
        Path(self.state['bmc_password_file']).chmod(0o644)
        with patch.object(sys, 'argv', valid), patch.object(REG, 'run') as command, \
                self.assertRaisesRegex(ValueError, 'password file'):
            REG.main()
        command.assert_not_called()
        self.assertFalse(destination.exists())
        Path(self.state['bmc_password_file']).chmod(0o600)
        with patch.object(sys, 'argv', valid), patch.object(REG, 'run', side_effect=ValueError('cluster unavailable')), \
                self.assertRaisesRegex(ValueError, 'cluster unavailable'):
            REG.main()
        self.assertFalse(destination.exists())

    def test_prefix_plan_preserves_families_gateway_reservation_and_separation(self):
        for profile, families in [('ipv4', [4]), ('dual-stack', [4, 6]), ('ipv6-only', [6])]:
            segments, _ = REG.plan_targets(inventories(), profile, ['fd00:10:96::4010/124'], 'owner', 'domain')
            for s in segments[1:]:
                self.assertEqual([REG.ipaddress.ip_network(p['prefix']).version for p in s['prefixes']], families)
                self.assertTrue(all(p['reserve_first'] == 10 for p in s['prefixes']))
        for change in ('overlap', 'gateway', 'identity', 'missing_ipv6'):
            inv = inventories()
            if change == 'overlap': inv['two']['bridgeAddressV6'] = inv['one']['bridgeAddressV6']
            elif change == 'gateway': inv['one']['bridgeAddressV6'] = 'fd42:8124:1::20/64'
            elif change == 'identity': inv['two']['mac'] = inv['one']['mac']
            else: del inv['one']['bridgeAddressV6']
            with self.subTest(change=change), self.assertRaises(ValueError):
                REG.plan_targets(inv, 'ipv6-only', ['fd00:10:96::4010/124'], 'owner', 'domain')


if __name__ == '__main__':
    unittest.main()
