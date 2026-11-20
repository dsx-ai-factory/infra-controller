# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import contextlib
import io
import json
import select
from pathlib import Path
import subprocess
import socket
import time
import sys
import unittest
from unittest.mock import patch

CHECK = load_script('guest_connectivity', Path(__file__).resolve().parents[1] / 'lab/acceptance/check-connectivity.py')


class GuestConnectivityTest(unittest.TestCase):
    def test_real_exchange_checks_source_and_payload_and_closes_listener(self):
        def local_command(args, guest, code, *values):
            return [sys.executable, '-u', '-c', code, *map(str, values)]
        guest = {'address': '127.0.0.1'}
        with patch.object(CHECK, 'ssh_command', side_effect=local_command):
            result = CHECK.probe(None, guest, guest)
        self.assertEqual(result, {'source': '127.0.0.1', 'destination': '127.0.0.1', 'bytes': 32})

    def test_real_ipv6_exchange_when_loopback_is_available(self):
        try:
            with socket.socket(socket.AF_INET6) as listener:
                listener.bind(('::1', 0))
        except OSError as error:
            self.skipTest('IPv6 loopback unavailable: ' + str(error))
        def local_command(args, guest, code, *values):
            return [sys.executable, '-u', '-c', code, *map(str, values)]
        with patch.object(CHECK, 'ssh_command', side_effect=local_command):
            result = CHECK.probe(None, {'address': '::1'}, {'address': '::1'})
        self.assertEqual(result, {'source': '::1', 'destination': '::1', 'bytes': 32})

    def test_failed_sender_reaps_listener_process(self):
        processes = []
        original = CHECK.guest_process

        @contextlib.contextmanager
        def record(command):
            with original(command) as process:
                processes.append(process)
                yield process

        def local_command(args, guest, code, *values):
            if code == CHECK.CLIENT:
                return [sys.executable, '-c', 'raise SystemExit(7)']
            return [sys.executable, '-u', '-c', code, *map(str, values)]

        with patch.object(CHECK, 'ssh_command', side_effect=local_command), \
                patch.object(CHECK, 'guest_process', side_effect=record), \
                self.assertRaisesRegex(RuntimeError, 'sender failed'):
            CHECK.probe(None, {'address': '127.0.0.1'}, {'address': '127.0.0.1'})
        self.assertEqual(len(processes), 2)
        self.assertTrue(all(process.poll() is not None for process in processes))
        self.assertTrue(all(process.stdout.closed for process in processes))

    def test_listener_startup_handles_partial_output_and_early_exit(self):
        for code, error, timeout in [("import time; print('{',end='',flush=True); time.sleep(10)", 'timed out', 0.2),
                                     ('raise SystemExit(7)', 'exited before reporting', 5)]:
            with self.subTest(code=code), CHECK.guest_process([sys.executable, '-u', '-c', code]) as process:
                if error == 'timed out':
                    self.assertTrue(select.select([process.stdout], [], [], 5)[0])
                with self.assertRaisesRegex(RuntimeError, error):
                    CHECK.listener_port(process, timeout=timeout)

    def test_cleanup_stops_descendant_after_direct_child_exits(self):
        child = "import socket,time; s=socket.socket(); s.bind(('127.0.0.1',0)); s.listen(); print(s.getsockname()[1],flush=True); time.sleep(10)"
        parent = 'import subprocess,sys; subprocess.Popen([sys.executable,"-u","-c",sys.argv[1]])'
        with CHECK.guest_process([sys.executable, '-u', '-c', parent, child]) as process:
            port = int(process.stdout.readline())
            process.wait(timeout=3)
        for attempt in range(20):
            try:
                with socket.create_connection(('127.0.0.1', port), timeout=0.1):
                    pass
            except OSError:
                return
            time.sleep(0.05)
        self.fail('descendant listener survived process-group cleanup')

    def test_receiver_rejects_translated_source_or_wrong_payload(self):
        for expected_source, payload in [('127.0.0.2', 'expected'), ('127.0.0.1', 'wrong')]:
            with self.subTest(source=expected_source, payload=payload), subprocess.Popen(
                    [sys.executable, '-u', '-c', CHECK.SERVER, '127.0.0.1', expected_source, 'expected'],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True) as server:
                port = json.loads(server.stdout.readline())['port']
                client = subprocess.run([sys.executable, '-c', CHECK.CLIENT, '127.0.0.1', '127.0.0.1', str(port), payload],
                                        capture_output=True, text=True, timeout=10)
                output, errors = server.communicate(timeout=10)
                self.assertNotEqual(server.returncode, 0, errors)
                self.assertNotEqual(client.returncode, 0, client.stderr)
                self.assertNotIn('destination', output)

    def test_cli_probes_both_directions_and_does_not_report_success_after_failure(self):
        argv = ['check-connectivity', '--context', 'kind-test', '--first-address', '10.84.40.10',
                '--second-address', '10.84.41.10', '--identity', '/tmp/key',
                '--first-known-hosts', '/tmp/one', '--second-known-hosts', '/tmp/two']
        for failure in (False, True):
            output = io.StringIO()
            effects = [{'source': 'first'}, RuntimeError('peer unreachable') if failure else {'source': 'second'}]
            with self.subTest(failure=failure), patch.object(sys, 'argv', argv), \
                    patch.object(CHECK, 'probe', side_effect=effects) as probe, \
                    contextlib.redirect_stdout(output), contextlib.redirect_stderr(io.StringIO()):
                if failure:
                    with self.assertRaises(SystemExit) as error:
                        CHECK.main()
                    self.assertEqual(error.exception.code, 1)
                    self.assertEqual(output.getvalue(), '')
                else:
                    CHECK.main()
                    self.assertEqual(len(json.loads(output.getvalue())['directions']), 2)
                first, second = probe.call_args_list
                self.assertEqual(first.args[1]['address'], second.args[2]['address'])
                self.assertEqual(first.args[2]['address'], second.args[1]['address'])

    def test_same_guest_rejected_before_starting_probe(self):
        for option, value in [('--second-address', '10.84.40.10'), ('--second-release', 'target-one'),
                              ('--second-address', 'fd42:84:41::10')]:
            argv = ['check-connectivity', '--context', 'kind-test', '--first-address', '10.84.40.10',
                    '--second-address', '10.84.41.10', '--identity', '/tmp/key',
                    '--first-known-hosts', '/tmp/one', '--second-known-hosts', '/tmp/two', option, value]
            with self.subTest(option=option), patch.object(sys, 'argv', argv), patch.object(CHECK, 'probe') as probe, \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as error:
                CHECK.main()
            self.assertEqual(error.exception.code, 2)
            probe.assert_not_called()
