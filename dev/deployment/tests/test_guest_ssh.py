# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import argparse
import contextlib
import io
import ipaddress
import json
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import threading
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
SSH = load_script('guest_ssh', ROOT / 'dev/deployment/lab/ssh-guest.py')


class GuestSshTest(unittest.TestCase):
    def test_relay_preserves_binary_response_after_stdin_eof(self):
        request = bytes(range(256)) * 512
        response = request[::-1]
        received = bytearray()
        with socket.socket() as server:
            server.bind(('127.0.0.1', 0))
            server.listen(1)
            server.settimeout(5)

            def serve():
                with server.accept()[0] as connection:
                    connection.settimeout(5)
                    while data := connection.recv(65536):
                        received.extend(data)
                    connection.sendall(response)

            thread = threading.Thread(target=serve, daemon=True)
            thread.start()
            result = subprocess.run([sys.executable, '-c', SSH.RELAY, '127.0.0.1', str(server.getsockname()[1])],
                                    input=request, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
            thread.join(5)
        self.assertFalse(thread.is_alive())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(received, request)
        self.assertEqual(result.stdout, response)

    def test_proxy_keeps_key_on_client_and_preserves_argument_boundaries(self):
        args = argparse.Namespace(release='target-one', identity=Path('/tmp/key'),
                                  known_hosts=Path('/tmp/known_hosts'), user='lab', command=['uname -m'])
        kube = ['kubectl', '--context', 'kind-lab', '-n', 'bmc-lab']
        command = SSH.ssh_command(args, kube, ipaddress.IPv4Address('10.84.40.10'))
        proxy = shlex.split(next(value.removeprefix('ProxyCommand=') for value in command if value.startswith('ProxyCommand=')))
        self.assertEqual(proxy, kube + ['exec', '-i', 'deployment/target-one', '--', 'python3', '-c', SSH.RELAY, '10.84.40.10', '22'])
        self.assertNotIn(str(args.identity), proxy)
        self.assertIn('StrictHostKeyChecking=accept-new', command)
        self.assertEqual(command[-2:], ['lab@10.84.40.10', 'uname -m'])

    def test_non_kind_context_rejected_before_cluster_access(self):
        with patch.object(sys, 'argv', ['ssh-guest', '--context', 'production', '--address', '10.84.40.10',
                                       '--identity', '/unused', '--known-hosts', '/unused']), \
                patch.object(SSH.subprocess, 'check_output') as command, \
                contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
            SSH.main()
        self.assertEqual(error.exception.code, 2)
        command.assert_not_called()

    def test_wrong_target_or_address_rejected_before_ssh(self):
        cases = [('other-service', '10.84.40.10'), ('nico-bmc-target', '10.84.41.10'),
                 ('nico-bmc-target', '10.84.40.1')]
        for label, address in cases:
            with self.subTest(label=label, address=address), \
                    patch.object(sys, 'argv', ['ssh-guest', '--context', 'kind-lab', '--address', address,
                                               '--identity', '/unused', '--known-hosts', '/unused']), \
                    patch.object(Path, 'is_file', return_value=True), \
                    patch.object(SSH.subprocess, 'check_output', side_effect=[
                        ('{"metadata":{"labels":{"app.kubernetes.io/name":"' + label + '"}}}').encode(),
                        b'{"bridgeAddress":"10.84.40.1/24"}']), \
                    patch.object(SSH.subprocess, 'call') as connect, \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                SSH.main()
            self.assertEqual(error.exception.code, 2)
            connect.assert_not_called()

    def test_ipv6_ssh_uses_only_the_selected_targets_retained_subnet(self):
        for address, configured, success in [('fd42:84:40::10', True, True),
                                               ('fd42:84:40::ffff:ffff:ffff:ffff', True, True),
                                               ('fd42:84:40::1', True, False),
                                               ('fd42:84:41::10', True, False),
                                               ('fd42:84:40::10', False, False),
                                               ('fd42:84:40::10%eth0', True, False)]:
            inventory = {'bridgeAddress': '10.84.40.1/24'}
            if configured:
                inventory['bridgeAddressV6'] = 'fd42:84:40::1/64'
            with self.subTest(address=address, configured=configured), \
                    patch.object(sys, 'argv', ['ssh-guest', '--context', 'kind-lab', '--address', address,
                                               '--identity', '/unused', '--known-hosts', '/unused']), \
                    patch.object(Path, 'is_file', return_value=True), \
                    patch.object(SSH.subprocess, 'check_output', side_effect=[
                        b'{"metadata":{"labels":{"app.kubernetes.io/name":"nico-bmc-target"}}}',
                        json.dumps(inventory).encode()]), \
                    patch.object(SSH, 'prepare_known_hosts'), \
                    patch.object(SSH.subprocess, 'call', return_value=0) as connect, \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                SSH.main()
            self.assertEqual(error.exception.code, 0 if success else 2)
            if success:
                self.assertEqual(connect.call_args.args[0][-1], 'lab@' + str(ipaddress.ip_address(address)))
            else:
                connect.assert_not_called()

    def test_known_hosts_creation_retention_and_invalid_destination(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            path = root / 'known_hosts'
            SSH.prepare_known_hosts(path)
            self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            path.write_text('existing pinned key\n')
            SSH.prepare_known_hosts(path)
            self.assertEqual(path.read_text(), 'existing pinned key\n')
            for invalid in (root, root / 'missing/known_hosts', Path('/dev/null')):
                with self.subTest(path=invalid), self.assertRaises((OSError, ValueError)):
                    SSH.prepare_known_hosts(invalid)

    def test_main_passes_remote_command_and_exit_status(self):
        with tempfile.TemporaryDirectory() as directory:
            key = Path(directory) / 'key'
            key.touch()
            known = Path(directory) / 'known_hosts'
            with patch.object(sys, 'argv', ['ssh-guest', '--context', 'kind-lab', '--address', '10.84.40.10',
                                           '--identity', str(key), '--known-hosts', str(known), '--', 'exit 7']), \
                    patch.object(SSH.subprocess, 'check_output', side_effect=[
                        b'{"metadata":{"labels":{"app.kubernetes.io/name":"nico-bmc-target"}}}',
                        b'{"bridgeAddress":"10.84.40.1/24"}']), \
                    patch.object(SSH.subprocess, 'call', return_value=7) as connect, \
                    self.assertRaises(SystemExit) as error:
                SSH.main()
            self.assertEqual(error.exception.code, 7)
            self.assertEqual(connect.call_args.args[0][-3:], ['--', 'lab@10.84.40.10', 'exit 7'])
            self.assertTrue(known.is_file())

    def test_ssh_config_path_transformations_rejected_before_file_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            for name in ('with spaces', 'token%u', 'env${USER}', 'quote"', 'back\\slash'):
                path = Path(directory) / name
                with self.subTest(name=name), self.assertRaises(ValueError):
                    SSH.prepare_known_hosts(path)
                self.assertFalse(path.exists())

    def test_reinterpreted_paths_rejected_before_cluster_access(self):
        for option, value in [('--identity', '/tmp/key%u'), ('--known-hosts', '/tmp/known hosts')]:
            arguments = ['ssh-guest', '--context', 'kind-lab', '--address', '10.84.40.10',
                         '--identity', '/tmp/key', '--known-hosts', '/tmp/known_hosts']
            arguments[arguments.index(option) + 1] = value
            with self.subTest(option=option), patch.object(sys, 'argv', arguments), \
                    patch.object(Path, 'is_file', return_value=True), \
                    patch.object(SSH.subprocess, 'check_output') as command, \
                    patch.object(SSH, 'prepare_known_hosts') as prepare, \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                SSH.main()
            self.assertEqual(error.exception.code, 2)
            command.assert_not_called()
            prepare.assert_not_called()
