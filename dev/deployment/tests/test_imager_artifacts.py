# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
IMAGER = load_script('build_imager', ROOT / 'dev/deployment/lab/build-imager.py')


class ImagerArtifactsTest(unittest.TestCase):
    def test_native_profiles_preserve_installer_and_hardware_sources(self):
        for architecture, profile in [('aarch64', 'qcow-imager-aarch64'), ('x86_64', 'qcow-imager')]:
            with self.subTest(architecture=architecture), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                original = (ROOT / 'pxe/mkosi.profiles' / profile / 'mkosi.conf').read_bytes()
                self.assertEqual(IMAGER.stage_profile(ROOT, work, architecture), profile)
                staged = work / 'mkosi.profiles' / profile
                self.assertEqual((staged / 'mkosi.conf').read_bytes(), original)
                self.assertEqual((ROOT / 'pxe/mkosi.profiles' / profile / 'mkosi.conf').read_bytes(), original)
                self.assertEqual((staged / 'mkosi.extra/opt/forge/disk_imaging.sh').read_bytes(),
                                 (ROOT / 'pxe/common_files/disk_imaging.sh').read_bytes())
                self.assertTrue((staged / 'mkosi.extra/opt/forge/disk_imaging.sh').stat().st_mode & 0o111)
                self.assertEqual((staged / 'mkosi.extra/etc/systemd/resolved.conf.d/90-nico-lab.conf').read_text(),
                                 '[Resolve]\nDNS=10.96.0.10\nDomains=~cluster.local\n')

    def test_ipv6_profile_does_not_pin_an_ipv4_cluster_resolver(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            profile = IMAGER.stage_profile(ROOT, work, 'x86_64', ipv6_boot=True)
            self.assertFalse((work / 'mkosi.profiles' / profile /
                'mkosi.extra/etc/systemd/resolved.conf.d/90-nico-lab.conf').exists())

    def test_existing_output_is_rejected_before_staging(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mkosi = root / 'pxe/mkosi/bin/mkosi'
            mkosi.parent.mkdir(parents=True)
            mkosi.touch()
            output = root / 'output'
            output.mkdir()
            (output / 'keep').write_text('prior artifact')
            with patch.object(IMAGER, 'REPO', root), patch.object(IMAGER.platform, 'system', return_value='Linux'), \
                    patch.object(IMAGER.platform, 'machine', return_value='aarch64'), \
                    patch.object(IMAGER.os, 'geteuid', return_value=0), patch.object(IMAGER, 'stage_profile') as stage, \
                    patch.object(sys, 'argv', ['build-imager', '--output', str(output)]), \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                IMAGER.main()
            stage.assert_not_called()
            self.assertEqual((output / 'keep').read_text(), 'prior artifact')

    def test_build_failure_publishes_no_payload_or_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            mkosi = root / 'pxe/mkosi/bin/mkosi'
            mkosi.parent.mkdir(parents=True)
            mkosi.touch()
            output = root / 'output'
            with patch.object(IMAGER, 'REPO', root), patch.object(IMAGER.platform, 'system', return_value='Linux'), \
                    patch.object(IMAGER.platform, 'machine', return_value='x86_64'), \
                    patch.object(IMAGER.os, 'geteuid', return_value=0), patch.object(IMAGER, 'stage_profile', return_value='qcow-imager'), \
                    patch.object(IMAGER.subprocess, 'run', side_effect=subprocess.CalledProcessError(1, 'mkosi')), \
                    patch.object(sys, 'argv', ['build-imager', '--output', str(output)]), \
                    self.assertRaises(subprocess.CalledProcessError):
                IMAGER.main()
            self.assertTrue((output / 'build').is_dir())
            self.assertFalse((output / 'x86_64').exists())
            self.assertFalse((output / 'build-info.json').exists())

    def test_main_publishes_the_selected_network_profile_with_its_payload(self):
        for ipv6 in (False, True):
            with self.subTest(ipv6=ipv6), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                output = root / 'output'
                mkosi = root / 'pxe/mkosi/bin/mkosi'
                mkosi.parent.mkdir(parents=True)
                mkosi.touch()
                installer = root / 'pxe/common_files/disk_imaging.sh'
                installer.parent.mkdir(parents=True)
                installer.write_bytes(b'fixture')
                def build(*args, **kwargs):
                    built = output / 'build/output'
                    built.mkdir(exist_ok=True)
                    for name in ['qcow-imager.efi']:
                        (built / name).write_bytes(b'payload')
                argv = ['builder', '--output', str(output)]
                if ipv6:
                    argv.append('--ipv6-boot')
                with patch.object(IMAGER, 'REPO', root), \
                        patch.object(IMAGER.platform, 'machine', return_value='x86_64'), \
                        patch.object(IMAGER.platform, 'system', return_value='Linux'), \
                        patch.object(IMAGER.os, 'geteuid', return_value=0), \
                        patch.object(IMAGER, 'stage_profile', return_value='qcow-imager'), \
                        patch.object(IMAGER.subprocess, 'run', side_effect=build), \
                        patch.object(sys, 'argv', argv), contextlib.redirect_stdout(io.StringIO()):
                    IMAGER.main()
                self.assertEqual((output / 'x86_64/qcow-imager.network-profile').read_text(),
                                 'ipv6\n' if ipv6 else 'default\n')
                manifest = json.loads((output / 'build-info.json').read_text())
                self.assertEqual(manifest['ipv6_boot'], ipv6)
                self.assertTrue((output / 'x86_64/qcow-imager.efi').is_file())
