# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
from deployment_test_support import load_script
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'lab/endpoint-values.py'
ENDPOINTS = load_script('endpoint_values', SCRIPT)


def services():
    return [dict(metadata=dict(name=name, namespace='nico-system', annotations={
                    'meta.helm.sh/release-name': release, 'meta.helm.sh/release-namespace': 'nico-system'}),
                 spec=dict(type='ClusterIP', ipFamilies=['IPv6'], ipFamilyPolicy='SingleStack', clusterIP=address,
                           ports=[dict(protocol=protocol, port=port) for protocol, port in ports]))
            for name, release, address, ports in [
                ('nico-lab-dns', 'nico-lab-dns', 'fd00:10:96::11', [('UDP', 53), ('TCP', 53)]),
                ('nico-pxe-external', 'nico-pxe', 'fd00:10:96::12', [('TCP', 8080)])]]


class EndpointValuesTests(unittest.TestCase):
    def test_public_export_is_read_only_repeatable_and_preserves_values_on_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            fixture, calls, output = root / 'services.json', root / 'calls.json', root / 'values.json'
            fixture.write_text(json.dumps({'items': services()}))
            client = root / 'kubectl'
            client.write_text('#!' + sys.executable + '\nimport json,os,pathlib,sys\n'
                              'pathlib.Path(os.environ["CALLS"]).write_text(json.dumps(sys.argv[1:]))\n'
                              'print(pathlib.Path(os.environ["FIXTURE"]).read_text())\n')
            client.chmod(0o755)
            env = dict(os.environ, PATH=str(root) + os.pathsep + os.environ['PATH'], FIXTURE=str(fixture), CALLS=str(calls))
            command = [sys.executable, str(SCRIPT), '--context', 'kind-test', '--output', str(output)]
            subprocess.run(command, env=env, check=True, capture_output=True, text=True)
            original = output.read_text()
            stamp = output.stat().st_mtime_ns
            self.assertEqual(json.loads(original), {'dhcp': {'v6DnsServers': 'fd00:10:96::11', 'v6ProvisioningServer': 'fd00:10:96::12'}})
            self.assertEqual(output.stat().st_mode & 0o777, 0o600)
            self.assertEqual(json.loads(calls.read_text()), ['--context', 'kind-test', '--request-timeout=30s', '-n', 'nico-system',
                             'get', 'service', 'nico-lab-dns', 'nico-pxe-external', '-o', 'json'])
            subprocess.run(command, env=env, check=True, capture_output=True, text=True)
            self.assertEqual(output.stat().st_mtime_ns, stamp)
            fixture.write_text('{"items": []}')
            result = subprocess.run(command, env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertIn('output was not changed', result.stderr)
            self.assertEqual(output.read_text(), original)

    def test_wrong_service_family_owner_or_boot_port_is_not_exported(self):
        for change in ('ipv4', 'dual', 'foreign', 'wrong_port', 'deleting'):
            values = services()
            if change == 'ipv4': values[0]['spec']['clusterIP'] = '10.96.0.10'
            elif change == 'dual': values[0]['spec']['ipFamilies'] = ['IPv6', 'IPv4']
            elif change == 'foreign': values[1]['metadata']['annotations']['meta.helm.sh/release-name'] = 'other'
            elif change == 'wrong_port': values[1]['spec']['ports'][0]['port'] = 80
            else: values[0]['metadata']['deletionTimestamp'] = 'now'
            with self.subTest(change=change), patch.object(ENDPOINTS.subprocess, 'check_output', return_value=json.dumps({'items': values})), self.assertRaises(ValueError):
                ENDPOINTS.export('kind-test', 'nico-system')

    def test_non_kind_context_fails_before_contact_or_output(self):
        with patch.object(sys, 'argv', ['endpoint-values.py', '--context', 'production', '--output', '/unused']), \
                patch.object(ENDPOINTS, 'export') as export, contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
            ENDPOINTS.main()
        export.assert_not_called()


if __name__ == '__main__':
    unittest.main()
