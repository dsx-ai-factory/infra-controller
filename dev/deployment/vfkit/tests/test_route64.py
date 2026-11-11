# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Route64 import and lab lifecycle contracts; no provider credentials needed."""

import base64
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import lab_manager
import route64
import vm

KEY = base64.b64encode(bytes(range(32))).decode()
EXPORT = f"""[Interface]
PrivateKey = {KEY}
Address = 2001:4860:ffff::2/64
[Peer]
PublicKey = {KEY}
AllowedIPs = ::/1, 8000::/1
Endpoint = 192.0.2.1:51820
PersistentKeepalive = 15
"""


class Route64Tests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.export = self.root / 'export.conf'
        self.export.write_text(EXPORT)
        self.lab = lab_manager.Lab(self.root / 'state', 12)
        self.lab.directory.mkdir(parents=True)
        self.lab.config_path.write_text(json.dumps({
            'uplink': 'nat', 'interface': None,
            'wan_mac': '02:00:00:00:12:01', 'lan_mac': '02:00:00:00:12:02'}))

    def test_export_normalization_retains_peer_but_owns_routes_and_dns(self):
        for allowed in ('::/0', '::/1, 8000::/1'):
            with self.subTest(allowed=allowed):
                self.export.write_text(EXPORT.replace('::/1, 8000::/1', allowed).replace(
                    '[Peer]', 'DNS = 2606:4700:4700::1111\n[Peer]'))
                normalized = route64.configuration(self.export)
                self.assertIn('AllowedIPs = ::/0\n', normalized)
                self.assertIn('Table = off\n', normalized)
                self.assertIn('PersistentKeepalive = 15\n', normalized)
                self.assertIn('MTU = 1420\n', normalized)
                self.assertIn(f'PrivateKey = {KEY}\n', normalized)
                self.assertNotIn('DNS', normalized)

    def test_invalid_exports_never_disclose_secret_source_lines(self):
        cases = {
            'hook': EXPORT.replace('[Peer]', 'PostUp = echo secret\n[Peer]'),
            'ipv4 route': EXPORT.replace('::/1, 8000::/1', '0.0.0.0/0'),
            'partial route': EXPORT.replace('::/1, 8000::/1', '::/1'),
            'ipv6 endpoint': EXPORT.replace('192.0.2.1', '[2001:db8::1]'),
            'mtu': EXPORT.replace('[Peer]', 'MTU = 1500\n[Peer]'),
            'defaults': '[DEFAULT]\nMTU = 1400\n' + EXPORT,
            'duplicate secret': EXPORT.replace('[Peer]', f'PrivateKey = {KEY}\n[Peer]'),
            'appended command': EXPORT + 'ping 2001:4860:ffff::1\n',
        }
        for name, content in cases.items():
            with self.subTest(name=name):
                self.export.write_text(content)
                with self.assertRaisesRegex(ValueError, 'invalid Route64 export') as raised:
                    route64.configuration(self.export)
                self.assertNotIn(KEY, str(raised.exception))

    def import_without_boot(self):
        # Stop at dependency discovery after the real import/persistence path.
        with patch.object(self.lab, 'clients', return_value=[]), \
                patch.object(self.lab, 'pid', return_value=None), \
                patch.object(lab_manager.shutil, 'which', return_value=None):
            with self.assertRaisesRegex(ValueError, 'require vfkit|requires vfkit'):
                self.lab.ensure(route64_config=self.export)

    def test_saved_secret_survives_source_removal_and_status_is_redacted(self):
        self.import_without_boot()
        secret = self.lab.directory / 'data/route64.conf'
        self.assertEqual(secret.stat().st_mode & 0o777, 0o600)
        self.assertEqual(secret.read_text(), route64.configuration(self.export))
        self.export.unlink()
        with patch.object(self.lab, 'pid', return_value=123), \
                patch.object(self.lab, 'clients', return_value=[]), \
                patch.object(self.lab, 'ready'), patch.object(lab_manager, 'run') as run:
            self.lab.ensure()
            status = self.lab.status()
            run.assert_not_called()
        self.assertEqual(status['config']['ipv6_uplink'], 'route64')
        self.assertNotIn(KEY, json.dumps(status))
        self.assertEqual(self.lab.environment()['VXLAB_IPV6_UPLINK'], 'route64')

    def test_running_lab_or_attached_vm_prevents_tunnel_change(self):
        for clients, pid in ((['attached VM'], None), ([], 123)):
            with self.subTest(clients=clients), \
                    patch.object(self.lab, 'clients', return_value=clients), \
                    patch.object(self.lab, 'pid', return_value=pid):
                with self.assertRaisesRegex(ValueError, 'stop|running VM'):
                    self.lab.ensure(route64_config=self.export)
                self.assertFalse((self.lab.directory / 'data/route64.conf').exists())
                self.assertNotIn('ipv6_uplink', self.lab.config)

    def test_missing_secret_fails_before_starting_or_native_fallback(self):
        self.import_without_boot()
        (self.lab.directory / 'data/route64.conf').unlink()
        with patch.object(self.lab, 'pid') as pid:
            with self.assertRaisesRegex(ValueError, 'configuration is missing'):
                self.lab.ensure()
            pid.assert_not_called()

    def test_route64_probe_is_bound_to_tunnel_only_for_ipv6(self):
        self.import_without_boot()
        with patch.object(self.lab, 'ssh') as ssh:
            self.lab.verify()
        self.assertNotIn('--interface', ssh.call_args_list[0].args[0])
        self.assertIn('--interface route64 -6', ssh.call_args_list[1].args[0])

    def test_cli_rejects_tunnel_option_outside_setup(self):
        for action in ('start', 'lab-stop', 'lab-status'):
            with self.subTest(action=action), self.assertRaisesRegex(ValueError, 'lab options|uplink options'):
                vm.main(['--lab-route64-config', str(self.export), action])


if __name__ == '__main__':
    unittest.main()
