# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
from deployment_test_support import load_script
import contextlib
import io
import json
import os
import time
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import Mock, patch

PATH = Path(__file__).resolve().parents[1] / 'lab/acceptance/traffic-guard.py'
GUARD = load_script('traffic_guard', PATH)
MAC = bytes.fromhex('020000030002')


def frame(protocol, payload):
    return b'\xff' * 6 + MAC + protocol.to_bytes(2, 'big') + payload


def ipv4(source=b'\0' * 4, destination=b'\xff' * 4, ports=b'\0D\0C', fragment=0):
    packet = bytearray(28)
    packet[0] = 0x45
    packet[2:4] = len(packet).to_bytes(2, 'big')
    packet[6:8] = fragment.to_bytes(2, 'big')
    packet[9] = 17
    packet[12:20] = source + destination
    packet[20:24] = ports
    packet[24:26] = b'\0\x08'
    return bytes(packet)


class TrafficGuardTests(unittest.TestCase):
    def test_packet_policy_distinguishes_firmware_probes_from_ipv4_use(self):
        v6 = b'\x60' + b'\0' * 39
        cases = [
            (frame(0x0806, b'\0' * 28), 'arp'),
            (b'\xff' * 6 + b'\x01' * 6 + b'\x08\x00' + ipv4(ports=b'\0C\0D'), 'ipv4'),
            (b'\xff' * 6 + b'\x01' * 6 + b'\x08\x06' + b'\0' * 28, 'arp'),
            (frame(0x86dd, v6), 'ipv6'),
            (b'\x33\x33' + b'\0' * 10 + b'\x86\xdd' + v6, None),
            (frame(0x8100, b'\0\1\x86\xdd' + v6), 'ipv6'),
            (frame(0x0800, ipv4()), 'dhcp4_probe'),
            (frame(0x0800, ipv4(source=b'\x0a\0\0\1')), 'ipv4'),
            (frame(0x0800, ipv4(ports=b'\0C\0D')), 'ipv4'),
            (frame(0x0800, ipv4(fragment=0x2000)), 'ipv4'),
            (frame(0x0800, ipv4(ports=b'\0\x35\0\x35')), 'ipv4'),
            (frame(0x0800, b'\x45'), 'malformed'),
            (frame(0x86dd, b'\x60'), 'malformed'),
            (frame(0x88cc, b'lldp'), 'other'),
            (b'\0' * 14, None),
        ]
        for packet, expected in cases:
            with self.subTest(expected=expected, packet=packet):
                self.assertEqual(GUARD.classify(packet, MAC), expected)

    def test_capture_classifies_large_ipv6_frames_in_live_and_stop_drain_paths(self):
        inventory = dict(mac='02:00:00:03:00:02', bridge='test-bridge')
        packet = frame(0x86dd, b'\x60' + b'\0' * 3 + b'\xff\xff' + b'\0' * (34 + 65535))
        packets, stdin = Mock(), Mock()
        stdin.buffer.readline.return_value = json.dumps(inventory)
        received = iter([packet, packet])
        def receive(buffers, ancillary_size, flags):
            self.assertEqual(flags, GUARD.socket.MSG_TRUNC)
            try: data = next(received)
            except StopIteration: raise BlockingIOError()
            buffers[0][:] = data[:len(buffers[0])]
            return len(data), [], GUARD.socket.MSG_TRUNC, None
        packets.recvmsg_into.side_effect = receive
        packets.getsockopt.return_value = b'\0' * 8
        output = io.StringIO()
        socket_context = Mock()
        socket_context.__enter__ = Mock(return_value=packets)
        socket_context.__exit__ = Mock(return_value=False)
        with patch.object(GUARD.socket, 'AF_PACKET', 17, create=True), \
                patch.object(GUARD.socket, 'socket', return_value=socket_context), \
                patch.object(GUARD.socket, 'if_nametoindex', return_value=1), \
                patch.object(GUARD.Path, 'read_text', return_value=json.dumps(inventory)), \
                patch.object(GUARD.sys, 'stdin', stdin), patch.object(GUARD.os, 'read', return_value=b'!'), \
                patch.object(GUARD.select, 'select', side_effect=[([packets], [], []), ([stdin.buffer], [], [])]), \
                contextlib.redirect_stdout(output):
            GUARD.capture()
        report = GUARD.assess([json.loads(line) for line in output.getvalue().splitlines()])
        self.assertEqual(report['counts']['ipv6'], 2)
        self.assertEqual(report['counts']['malformed'], 0)

    def test_header_capture_checks_declared_lengths_against_original_wire_length(self):
        for packet in (frame(0x86dd, b'\x60' + b'\0' * 3 + b'\xff\xff' + b'\0' * (34 + 65535)),
                       frame(0x0800, ipv4(source=b'\x0a\0\0\1'))):
            with self.subTest(protocol=packet[12:14]):
                header = memoryview(packet)[:GUARD.CAPTURE_HEADER_BYTES]
                self.assertEqual(GUARD.classify(header, MAC, len(packet)), GUARD.classify(packet, MAC))
                self.assertEqual(GUARD.classify(header, MAC, len(packet) - 1), 'malformed')

    def test_incomplete_lossy_or_ipv4_observation_cannot_pass(self):
        good = {'event': 'finished', 'stop_requested': True, 'counts': dict.fromkeys(GUARD.COUNTERS, 0)}
        good['counts'].update(ipv6=10, dhcp4_probe=4)
        records = [{'event': 'ready'}, good]
        self.assertEqual(GUARD.assess(records), good)
        cases = [[], records[:1], [dict(good)], [records[0], dict(good, stop_requested=False)]]
        for counter in ('arp', 'ipv4', 'malformed', 'dropped'):
            cases.append([records[0], dict(good, counts=dict(good['counts'], **{counter: 1}))])
        cases.append([records[0], dict(good, counts=dict(good['counts'], ipv6=0))])
        for invalid in cases:
            with self.subTest(records=invalid), self.assertRaises(ValueError):
                GUARD.assess(invalid)

    def test_stop_attempts_both_targets_and_records_incomplete_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            guard = object.__new__(GUARD.Guard)
            guard.args = Mock(output=Path(directory), namespace='bmc-lab')
            guard.cluster, guard.kube, guard.command_succeeded = 'cluster', ['kubectl'], True
            jobs = []
            for name in ('one', 'two'):
                process = Mock()
                process.poll.return_value = 1
                jobs.append(dict(release=name, process=process, log=Mock(), errors=Mock()))
            guard.jobs = jobs
            guard.outcome = {}
            with patch.object(GUARD, 'checked', side_effect=RuntimeError('cluster unavailable')), self.assertRaises(ValueError):
                guard.stop()
            report = json.loads((Path(directory) / 'report.json').read_text())
            self.assertFalse(report['passed'])
            self.assertEqual(len(report['failures']), 4)
            self.assertIn('cluster unavailable', report['failures'][-2])
            for job in jobs:
                job['log'].close.assert_called_once()
                job['errors'].close.assert_called_once()

    def test_failed_capture_start_never_launches_wrapped_command_and_stops_started_observers(self):
        with tempfile.TemporaryDirectory() as directory:
            observer = Mock()
            observer.outcome = {}
            observer.start.side_effect = ValueError('second capture failed')
            args = ['traffic-guard.py', '--context', 'kind-test', '--target', 'one', '--target', 'two',
                    '--output', str(Path(directory) / 'output'), '--', 'unexpected-command']
            with patch.object(GUARD.sys, 'argv', args), patch.object(GUARD, 'Guard', return_value=observer), \
                    patch.object(GUARD.subprocess, 'Popen') as launch, \
                    self.assertRaisesRegex(ValueError, 'second capture failed'):
                GUARD.main()
            launch.assert_not_called()
            observer.stop.assert_called_once()

    def test_termination_signals_stop_the_wrapped_process_group_and_observers(self):
        for number in (GUARD.signal.SIGINT, GUARD.signal.SIGTERM, GUARD.signal.SIGHUP):
            with self.subTest(signal=number), tempfile.TemporaryDirectory() as directory:
                observer, process = Mock(), Mock()
                observer.outcome = {}
                process.poll.return_value = None
                handlers = {}
                def install(number, handler):
                    previous = handlers.get(number, GUARD.signal.SIG_DFL)
                    handlers[number] = handler
                    return previous
                def wait(**kwargs):
                    if kwargs.get("timeout") != GUARD.STOP_GRACE_SECONDS:
                        handlers[number](number, None)
                    else:
                        # A second cancellation records state without raising
                        # across the cleanup boundary.
                        handlers[number](number, None)
                    return 0
                process.wait.side_effect = wait
                args = ['traffic-guard.py', '--context', 'kind-test', '--target', 'one', '--target', 'two',
                        '--output', str(Path(directory) / 'output'), '--', 'command']
                with patch.object(GUARD.sys, 'argv', args), patch.object(GUARD, 'Guard', return_value=observer), \
                        patch.object(GUARD.signal, 'signal', side_effect=install), \
                        patch.object(GUARD.subprocess, 'Popen', return_value=process), \
                        patch.object(GUARD.os, 'killpg') as kill, self.assertRaises(InterruptedError):
                    GUARD.main()
                kill.assert_called_once_with(process.pid, GUARD.signal.SIGINT)
                observer.stop.assert_called_once()
                self.assertEqual(handlers[number], GUARD.signal.SIG_DFL)

    def test_cancellation_during_evidence_collection_cannot_report_success(self):
        with tempfile.TemporaryDirectory() as directory:
            observer, process = Mock(), Mock()
            observer.outcome = {}
            process.poll.return_value = 0
            process.wait.return_value = 0
            handlers = {}
            def install(number, handler):
                previous = handlers.get(number, GUARD.signal.SIG_DFL)
                handlers[number] = handler
                return previous
            observer.stop.side_effect = lambda: handlers[GUARD.signal.SIGTERM](GUARD.signal.SIGTERM, None)
            args = ['traffic-guard.py', '--context', 'kind-test', '--target', 'one', '--target', 'two',
                    '--output', str(Path(directory) / 'output'), '--', 'command']
            with patch.object(GUARD.sys, 'argv', args), patch.object(GUARD, 'Guard', return_value=observer), \
                    patch.object(GUARD.signal, 'signal', side_effect=install), \
                    patch.object(GUARD.subprocess, 'Popen', return_value=process), self.assertRaises(InterruptedError):
                GUARD.main()
            self.assertFalse(observer.command_succeeded)
            self.assertEqual(observer.outcome['cancellation_signal'], GUARD.signal.SIGTERM)
            observer.stop.assert_called_once()

    def test_cancellation_preserves_primary_error_and_stops_at_report_boundary(self):
        for finished in (False, True):
            with self.subTest(finished=finished), tempfile.TemporaryDirectory() as directory:
                observer, process = Mock(), Mock()
                observer.outcome = {}
                process.poll.return_value = 1
                process.wait.return_value = 1
                handlers = {}
                def install(number, handler):
                    previous = handlers.get(number, GUARD.signal.SIG_DFL)
                    handlers[number] = handler
                    return previous
                def stop():
                    if finished:
                        observer.outcome['finished'] = 'completion boundary'
                    handlers[GUARD.signal.SIGTERM](GUARD.signal.SIGTERM, None)
                observer.stop.side_effect = stop
                args = ['traffic-guard.py', '--context', 'kind-test', '--target', 'one', '--target', 'two',
                        '--output', str(Path(directory) / 'output'), '--', 'command']
                with patch.object(GUARD.sys, 'argv', args), patch.object(GUARD, 'Guard', return_value=observer), \
                        patch.object(GUARD.signal, 'signal', side_effect=install), \
                        patch.object(GUARD.subprocess, 'Popen', return_value=process), self.assertRaises(subprocess.CalledProcessError):
                    GUARD.main()
                self.assertIn('CalledProcessError', observer.outcome['error'])
                self.assertEqual(observer.outcome.get('cancellation_signal'), None if finished else GUARD.signal.SIGTERM)

    def test_start_rejects_ambiguous_or_foreign_targets_before_launch(self):
        pod = {'metadata': {'name': 'one-pod', 'uid': 'pod', 'labels': {'app.kubernetes.io/name': 'nico-bmc-target'}}}
        wrong_label = {'metadata': dict(pod['metadata'], labels={})}
        for pods, inventory in [([pod, pod], None), ([wrong_label], None),
                                ([pod], {'clusterUID': 'other', 'owner': 'bmc-lab/one'})]:
            with self.subTest(pods=pods), tempfile.TemporaryDirectory() as directory:
                args = Mock(output=Path(directory), context='kind-test', namespace='bmc-lab', target=['one', 'two'], command=['true'])
                guard = GUARD.Guard(args)
                results = [{'metadata': {'uid': 'cluster'}}, {'items': pods}, inventory]
                with patch.object(GUARD, 'checked', side_effect=results), patch.object(GUARD.subprocess, 'Popen') as launch, self.assertRaises(ValueError):
                    guard.start()
                launch.assert_not_called()

    def test_graceful_stop_allows_lifecycle_style_child_session_cleanup(self):
        with tempfile.TemporaryDirectory() as directory:
            ready, stopped = Path(directory) / 'ready', Path(directory) / 'stopped'
            script = """import pathlib,subprocess,sys,time
child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],start_new_session=True)
pathlib.Path(sys.argv[1]).write_text(str(child.pid))
try:
 time.sleep(60)
finally:
 child.terminate();child.wait();pathlib.Path(sys.argv[2]).write_text('done')
"""
            process = subprocess.Popen(['python3', '-c', script, str(ready), str(stopped)],
                                       start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            child_pid = None
            try:
                deadline = time.monotonic() + 5
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(.01)
                self.assertTrue(ready.exists())
                child_pid = int(ready.read_text())
                outcome = {'forced_stop': False}
                with patch.object(GUARD, 'STOP_GRACE_SECONDS', 5):
                    GUARD.stop_command(process, outcome)
                self.assertTrue(stopped.exists())
                self.assertFalse(outcome['forced_stop'])
                with self.assertRaises(ProcessLookupError): os.kill(child_pid, 0)
            finally:
                if process.poll() is None:
                    process.kill(); process.wait()
                if child_pid:
                    try: os.kill(child_pid, GUARD.signal.SIGKILL)
                    except ProcessLookupError: pass

    def test_same_source_executes_through_python_c_without_file_context(self):
        result = subprocess.run(['python3', '-c', PATH.read_text(), '--help'], capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('--context', result.stdout)

    def test_invalid_cli_never_launches_command_or_creates_output(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'output'
            result = subprocess.run(['python3', str(PATH), '--context', 'production', '--target', 'one',
                                     '--target', 'two', '--output', str(output), '--', 'false'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertFalse(output.exists())


if __name__ == '__main__':
    unittest.main()
