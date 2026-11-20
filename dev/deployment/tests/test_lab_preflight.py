# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'lab' / 'preflight.py'
PREFLIGHT = load_script('lab_preflight', SCRIPT)


class PreflightTest(unittest.TestCase):
    def test_hugepage_capacity_excludes_reserved_pages(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for free, reserved, expected in [(4096, 2048, 4096), (2, 3, 0)]:
                with self.subTest(free=free, reserved=reserved):
                    (root / 'free_hugepages').write_text(str(free))
                    (root / 'resv_hugepages').write_text(str(reserved))
                    self.assertEqual(PREFLIGHT.hugepage_capacity(root), expected)
            (root / 'free_hugepages').write_text('invalid')
            with self.assertRaises(ValueError):
                PREFLIGHT.hugepage_capacity(root)

    def test_host_hugepage_budget_is_separate_from_ordinary_ram(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PREFLIGHT.platform, 'system', return_value='Linux'), \
                patch.object(PREFLIGHT.platform, 'machine', return_value='aarch64'), \
                patch.object(PREFLIGHT, 'check_kvm', return_value=12), \
                patch.object(PREFLIGHT, 'host_capacity', return_value={'memory_mib': 2048, 'cpus': 4, 'disk_gib': 48}), \
                patch.object(PREFLIGHT, 'hugepage_capacity', return_value=4096), \
                patch.object(PREFLIGHT.sys, 'stdout') as output:
            status = PREFLIGHT.main(['host', '--storage-path', directory, '--memory-mib', '2048',
                                     '--cpus', '4', '--disk-gib', '48', '--hugepages-mib', '8192'])
            document = json.loads(''.join(call.args[0] for call in output.write.call_args_list))
            self.assertEqual(status, 1)
            self.assertEqual(document['available']['hugepages_mib'], 4096)
            self.assertEqual(document['requested']['hugepages_mib'], 8192)
            self.assertEqual(document['errors'], ['insufficient hugepages_mib: requested 8192, available 4096'])

    def test_kvm_checks_api_and_releases_vm_and_device(self):
        for architecture, ipa, vm_type in [('amd64', 0, 0), ('arm64', 36, 36), ('arm64', 48, 40)]:
            with self.subTest(architecture=architecture), patch.object(PREFLIGHT.os, 'open', return_value=10), \
                    patch.object(PREFLIGHT.os, 'close') as close, \
                    patch.object(PREFLIGHT.fcntl, 'ioctl') as ioctl:
                responses = {PREFLIGHT.KVM_GET_API_VERSION: 12, PREFLIGHT.KVM_CHECK_EXTENSION: ipa,
                             PREFLIGHT.KVM_CREATE_VM: 11}
                ioctl.side_effect = lambda fd, operation, arg=0: responses[operation]
                self.assertEqual(PREFLIGHT.check_kvm(architecture), 12)
                self.assertIn(unittest.mock.call(10, PREFLIGHT.KVM_CREATE_VM, vm_type), ioctl.call_args_list)
                self.assertEqual(close.call_args_list, [unittest.mock.call(11), unittest.mock.call(10)])

    def test_kvm_failure_does_not_leak_or_pass(self):
        for responses, error in [([11], 'API version'), ([12, OSError('creation denied')], 'creation denied')]:
            with self.subTest(error=error), patch.object(PREFLIGHT.os, 'open', return_value=10), \
                    patch.object(PREFLIGHT.os, 'close') as close, \
                    patch.object(PREFLIGHT.fcntl, 'ioctl', side_effect=responses):
                with self.assertRaisesRegex((ValueError, OSError), error):
                    PREFLIGHT.check_kvm('amd64')
                close.assert_called_once_with(10)

    def test_capacity_checks_each_requested_resource_and_exact_boundary(self):
        available = {'memory_mib': 8192, 'cpus': 4, 'disk_gib': 48}
        self.assertEqual(PREFLIGHT.capacity_errors(available, available), [])
        for resource in available:
            with self.subTest(resource=resource):
                requested = {**available, resource: available[resource] + 1}
                errors = PREFLIGHT.capacity_errors(available, requested)
                self.assertEqual(len(errors), 1)
                self.assertIn(resource, errors[0])

    def test_capacity_uses_available_memory_and_unprivileged_disk_space(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            memory = root / 'meminfo'
            memory.write_text('MemTotal: 999999 kB\nMemAvailable: 8388608 kB\n')
            with patch.object(PREFLIGHT.os, 'sched_getaffinity', return_value={1, 3}, create=True), \
                    patch.object(PREFLIGHT.os, 'statvfs') as statvfs:
                statvfs.return_value.f_bavail = 24 * 1024
                statvfs.return_value.f_frsize = 1024 * 1024
                self.assertEqual(PREFLIGHT.host_capacity(root, memory),
                                 {'memory_mib': 8192, 'cpus': 2, 'disk_gib': 24})
                statvfs.assert_called_once_with(root)

    def test_cli_rejects_missing_or_invalid_budget_before_probing(self):
        cases = [[], ['host'], ['host', '--storage-path', '/tmp', '--memory-mib', '0', '--cpus', '2', '--disk-gib', '24']]
        for arguments in cases:
            with self.subTest(arguments=arguments):
                result = subprocess.run([sys.executable, str(SCRIPT), *arguments], capture_output=True, text=True)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn('usage:', result.stderr)
                self.assertNotIn('Traceback', result.stderr)

    def test_public_command_serializes_failure_and_success(self):
        for kvm_error in [None, PermissionError('KVM denied')]:
            with self.subTest(kvm_error=kvm_error), patch.object(PREFLIGHT.platform, 'system', return_value='Linux'), \
                    patch.object(PREFLIGHT.platform, 'machine', return_value='aarch64'), \
                    patch.object(PREFLIGHT, 'check_kvm', side_effect=kvm_error, return_value=12), \
                    patch.object(PREFLIGHT.sys, 'stdout') as output:
                status = PREFLIGHT.main(['kvm'])
                document = json.loads(''.join(call.args[0] for call in output.write.call_args_list))
                self.assertEqual(status, 1 if kvm_error else 0)
                self.assertEqual(document['ready'], kvm_error is None)
                self.assertEqual(document['architecture'], 'arm64')
                self.assertEqual(document['scope'], 'kvm')
                self.assertEqual(document['errors'], ['KVM denied'] if kvm_error else [])

    def test_host_command_reports_all_budget_failures(self):
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(PREFLIGHT.platform, 'system', return_value='Linux'), \
                patch.object(PREFLIGHT.platform, 'machine', return_value='x86_64'), \
                patch.object(PREFLIGHT, 'check_kvm', return_value=12), \
                patch.object(PREFLIGHT, 'host_capacity', return_value={'memory_mib': 8192, 'cpus': 4, 'disk_gib': 48}), \
                patch.object(PREFLIGHT.sys, 'stdout') as output:
            status = PREFLIGHT.main(['host', '--storage-path', directory,
                                     '--memory-mib', '8193', '--cpus', '5', '--disk-gib', '49'])
            document = json.loads(''.join(call.args[0] for call in output.write.call_args_list))
            self.assertEqual(status, 1)
            self.assertFalse(document['ready'])
            self.assertEqual(document['scope'], 'host')
            self.assertEqual(document['requested'], {'memory_mib': 8193, 'cpus': 5, 'disk_gib': 49})
            self.assertEqual(len(document['errors']), 3)

    def test_host_read_failure_omits_all_capacity_fields(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            loop = root / 'loop'
            loop.symlink_to(loop)
            for path in (root / 'missing', loop):
                with self.subTest(path=path.name), \
                        patch.object(PREFLIGHT.platform, 'system', return_value='Linux'), \
                        patch.object(PREFLIGHT.platform, 'machine', return_value='x86_64'), \
                        patch.object(PREFLIGHT, 'check_kvm', return_value=12), \
                        patch.object(PREFLIGHT.sys, 'stdout') as output:
                    status = PREFLIGHT.main(['host', '--storage-path', str(path),
                                             '--memory-mib', '1', '--cpus', '1', '--disk-gib', '1'])
                    document = json.loads(''.join(call.args[0] for call in output.write.call_args_list))
                    self.assertEqual(status, 1)
                    self.assertFalse(document['ready'])
                    self.assertTrue(document['errors'])
                    self.assertEqual(document['kvm_api_version'], 12)
                    self.assertFalse({'storage_path', 'requested', 'available'} & document.keys())

    def test_unsupported_host_and_missing_storage_fail_closed(self):
        with patch.object(PREFLIGHT.platform, 'system', return_value='Darwin'), \
                patch.object(PREFLIGHT, 'check_kvm') as kvm, patch.object(PREFLIGHT.sys, 'stdout'):
            self.assertEqual(PREFLIGHT.main(['kvm']), 1)
            kvm.assert_not_called()
        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(ValueError, 'existing directory'):
            PREFLIGHT.host_capacity(Path(directory) / 'absent')
