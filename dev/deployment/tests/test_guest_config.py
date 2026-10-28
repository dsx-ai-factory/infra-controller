# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import base64
import configparser
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[3]
SCRIPT = ROOT / 'dev/deployment/lab/acceptance/guest-config.py'
IMAGE = '81240025-0000-4000-8000-000000000001'
MAC = '02:00:00:01:00:02'
# A structurally valid public key; no private key is needed by this generator.
KEY = 'ssh-ed25519 ' + base64.b64encode(b'\0\0\0\x0bssh-ed25519\0\0\0\x20' + bytes(range(32))).decode()


class GuestConfigTest(unittest.TestCase):
    def invoke(self, directory, *args, key=KEY):
        public_key = Path(directory) / 'key.pub'
        public_key.write_text(key + '\n')
        return subprocess.run([sys.executable, str(SCRIPT), '--os-image-id', IMAGE,
                               '--mac', MAC, '--public-key', str(public_key),
                               '--generation', 'generation-2', *args],
                              capture_output=True, text=True, timeout=10)

    def test_profiles_serialize_complete_os_contract(self):
        cases = [('ipv4', 'ipv4', 'no', 'no'), ('dual-stack', 'yes', 'yes', 'ipv6'),
                 ('ipv6-only', 'ipv6', 'yes', 'ipv6')]
        with tempfile.TemporaryDirectory() as directory:
            for mode, dhcp, ra, link_local in cases:
                with self.subTest(mode=mode):
                    result = self.invoke(directory, '--network', mode)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    config = json.loads(result.stdout)
                    self.assertEqual(config['variant'], {'OsImageId': {'value': IMAGE}})
                    self.assertFalse(config['phone_home_enabled'])
                    self.assertFalse(config['run_provisioning_instructions_on_every_boot'])
                    self.assertTrue(config['user_data'].startswith('#cloud-config\n'))
                    data = yaml.safe_load(config['user_data'])
                    self.assertEqual(data['users'][0]['ssh_authorized_keys'], [KEY])
                    files = {item['path']: item for item in data['write_files']}
                    self.assertEqual(files['/etc/nico-lab-generation']['content'], 'generation-2\n')
                    network = configparser.ConfigParser()
                    network.read_string(files['/etc/systemd/network/05-nico-lab.network']['content'])
                    self.assertEqual(network['Match']['MACAddress'], MAC)
                    self.assertEqual(dict(network['Network']), {'dhcp': dhcp, 'ipv6acceptra': ra,
                                                               'linklocaladdressing': link_local})
                    self.assertEqual(network.has_section('DHCPv6'), mode != 'ipv4')
                    if mode != 'ipv4':
                        self.assertEqual(network['DHCPv6']['WithoutRA'], 'solicit')
                    self.assertEqual(data['runcmd'], [['networkctl', 'reload']])

    def test_cloud_init_yaml_preserves_unicode_marker_and_key_comment(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.invoke(directory, '--generation', 'generation-🌐', key=KEY + ' lab-🌐')
            self.assertEqual(result.returncode, 0, result.stderr)
            data = yaml.safe_load(json.loads(result.stdout)['user_data'])
            self.assertEqual(data['write_files'][0]['content'].encode(), 'generation-🌐\n'.encode())
            self.assertEqual(data['users'][0]['ssh_authorized_keys'][0].encode(), (KEY + ' lab-🌐').encode())

    def test_rejects_invalid_inputs_without_partial_output_or_key_disclosure(self):
        cases = [(['--network', 'unknown'], KEY),
                 (['--os-image-id', 'not-an-id'], KEY),
                 (['--mac', '01:00:00:01:00:02'], KEY),
                 (['--mac', '00:00:00:00:00:00'], KEY),
                 (['--generation', ''], KEY),
                 (['--generation', 'generation\u2028two'], KEY),
                 (['--network', 'ipv6-only'], '-----BEGIN OPENSSH PRIVATE KEY-----'),
                 (['--network', 'ipv6-only'], 'ssh-ed25519 malformed'),
                 (['--network', 'ipv6-only'], KEY + ' lab\x7fcomment')]
        with tempfile.TemporaryDirectory() as directory:
            for case, (args, key) in enumerate(cases):
                with self.subTest(case=case, args=args, key_type=key.split()[0]):
                    result = self.invoke(directory, *args, key=key)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertEqual(result.stdout, '')
                    self.assertNotIn(key, result.stderr)


if __name__ == '__main__':
    unittest.main()
