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
SCOUT = load_script('build_scout', ROOT / 'dev/deployment/lab/build-scout.py')


class ScoutArtifactsTest(unittest.TestCase):
    def test_staging_preserves_real_scout_and_hardware_sources_on_both_architectures(self):
        for arch in ('aarch64', 'x86_64'):
            with self.subTest(architecture=arch), tempfile.TemporaryDirectory() as directory:
                work = Path(directory)
                package = work / 'scout.deb'
                package.write_bytes(b'scout package fixture')
                originals = {profile: (ROOT / 'pxe/mkosi.profiles' / profile / 'mkosi.conf').read_bytes()
                             for profile in SCOUT.profiles(arch)}
                SCOUT.stage_profiles(ROOT, work, package, arch)
                for profile, original in originals.items():
                    self.assertEqual((ROOT / 'pxe/mkosi.profiles' / profile / 'mkosi.conf').read_bytes(), original)
                    staged = work / 'mkosi.profiles' / profile
                    self.assertEqual((staged / 'mkosi.extra/etc/systemd/resolved.conf.d/90-nico-lab.conf').read_text(),
                                     '[Resolve]\nDNS=10.96.0.10\nDomains=~cluster.local\n')
                    lines = [line.strip() for line in (staged / 'mkosi.conf').read_text().splitlines()]
                    self.assertEqual([line for line in lines if line.startswith('linux-image-')], ['linux-image-virtual'])
                    self.assertFalse(any(line.startswith(('linux-headers-', 'linux-modules-', 'linux-firmware')) for line in lines))
                    self.assertEqual((staged / 'mkosi.extra/opt/forge/forge-scout-network.sh').read_bytes(),
                                     (ROOT / 'pxe/common_files/forge-scout-network.sh').read_bytes())
                loader = work / 'mkosi.profiles' / f'scout-loader-{arch}' / 'mkosi.extra/etc/rc.local'
                original = (ROOT / 'pxe/common_files/scout-loader-rclocal').read_text()
                self.assertEqual(loader.read_text(), original.replace(
                    'http://carbide-static-pxe.forge/public/blobs/internal/',
                    'http://nico-pxe.nico-system.svc.cluster.local/public/blobs/internal/'))
                self.assertTrue(loader.stat().st_mode & 0o111)
                rootfs = work / 'mkosi.profiles' / f'scout-oss-{arch}'
                self.assertEqual((rootfs / 'mkosi.extra/build-output/forge-scout.deb').read_bytes(), package.read_bytes())
                self.assertEqual((rootfs / 'mkosi.extra/etc/systemd/system/forge-scout.service').read_bytes(),
                                 (ROOT / 'crates/scout/misc/forge-scout.service').read_bytes())
                self.assertFalse((rootfs / 'mkosi.extra/etc/apt').exists())
                self.assertIn('mlx-firmware-supervisor', (rootfs / 'mkosi.postinst.chroot').read_text())
                self.assertTrue((rootfs / 'mkosi.postinst.chroot').stat().st_mode & 0o111)

    def test_ipv6_profile_uses_dhcp_dns_and_the_ipv6_pxe_endpoint(self):
        with tempfile.TemporaryDirectory() as directory:
            work = Path(directory)
            package = work / 'scout.deb'
            package.write_bytes(b'fixture')
            SCOUT.stage_profiles(ROOT, work, package, 'aarch64', ipv6_boot=True)
            for profile in SCOUT.profiles('aarch64'):
                self.assertFalse((work / 'mkosi.profiles' / profile /
                    'mkosi.extra/etc/systemd/resolved.conf.d/90-nico-lab.conf').exists())
            loader = work / 'mkosi.profiles/scout-loader-aarch64/mkosi.extra/etc/rc.local'
            self.assertIn('http://carbide-static-pxe.forge:8080/public/blobs/internal/', loader.read_text())
            self.assertNotIn('svc.cluster.local', loader.read_text())

    def test_changed_kernel_profile_contract_is_rejected(self):
        for config in ('[Content]\nPackages=\n curl\n', '[Content]\n linux-image-something\n'):
            with self.subTest(config=config), self.assertRaises(ValueError):
                SCOUT.vm_config(config)

    def test_invalid_package_or_existing_output_does_not_start_a_build(self):
        for package_arch, existing, initialized in [('amd64', False, True), ('arm64', True, True), ('arm64', False, False)]:
            with self.subTest(package_arch=package_arch, existing=existing, initialized=initialized), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                package = root / 'scout.deb'
                package.touch()
                output = root / 'output'
                if existing:
                    output.mkdir()
                    (output / 'keep').write_text('retained artifact')
                mkosi = root / 'pxe/mkosi/bin/mkosi'
                mkosi.parent.mkdir(parents=True)
                if initialized:
                    mkosi.touch()
                with patch.object(SCOUT, 'REPO', root), patch.object(SCOUT.platform, 'machine', return_value='aarch64'), \
                        patch.object(SCOUT.platform, 'system', return_value='Linux'), patch.object(SCOUT.os, 'geteuid', return_value=0), \
                        patch.object(SCOUT.subprocess, 'check_output', side_effect=[package_arch, '0.0.0-test']), \
                        patch.object(SCOUT.subprocess, 'run') as run, \
                        patch.object(sys, 'argv', ['build-scout', '--scout-package', str(package), '--output', str(output)]), \
                        contextlib.redirect_stderr(io.StringIO()):
                    with self.assertRaises(SystemExit) as error:
                        SCOUT.main()
                    self.assertEqual(error.exception.code, 2)
                    run.assert_not_called()
                if existing:
                    self.assertEqual((output / 'keep').read_text(), 'retained artifact')
                else:
                    self.assertFalse(output.exists())

    def test_unsupported_runtime_is_rejected_before_package_inspection(self):
        cases = [('armv7l', 'Linux', 0), ('aarch64', 'Darwin', 0), ('x86_64', 'Linux', 1000)]
        for architecture, system, uid in cases:
            with self.subTest(architecture=architecture, system=system, uid=uid), \
                    patch.object(SCOUT.platform, 'machine', return_value=architecture), \
                    patch.object(SCOUT.platform, 'system', return_value=system), \
                    patch.object(SCOUT.os, 'geteuid', return_value=uid), \
                    patch.object(SCOUT.subprocess, 'check_output') as inspect, \
                    patch.object(sys, 'argv', ['build-scout', '--scout-package', '/missing.deb', '--output', '/unused']), \
                    contextlib.redirect_stderr(io.StringIO()):
                with self.assertRaises(SystemExit) as error:
                    SCOUT.main()
                self.assertEqual(error.exception.code, 2)
                inspect.assert_not_called()

    def test_failed_mkosi_build_does_not_publish_a_manifest_or_payload(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            package = root / 'scout.deb'
            package.touch()
            output = root / 'output'
            mkosi = root / 'pxe/mkosi/bin/mkosi'
            mkosi.parent.mkdir(parents=True)
            mkosi.touch()
            with patch.object(SCOUT, 'REPO', root), patch.object(SCOUT, 'stage_profiles'), \
                    patch.object(SCOUT.platform, 'machine', return_value='x86_64'), \
                    patch.object(SCOUT.platform, 'system', return_value='Linux'), patch.object(SCOUT.os, 'geteuid', return_value=0), \
                    patch.object(SCOUT.subprocess, 'check_output', side_effect=['amd64', '0.0.0-test']), \
                    patch.object(SCOUT.subprocess, 'run', side_effect=[None, subprocess.CalledProcessError(1, 'mkosi')]), \
                    patch.object(sys, 'argv', ['build-scout', '--scout-package', str(package), '--output', str(output)]):
                with self.assertRaises(subprocess.CalledProcessError):
                    SCOUT.main()
            self.assertFalse((output / 'build-info.json').exists())
            self.assertFalse((output / 'x86_64').exists())

    def test_main_publishes_the_selected_network_profile_with_its_payload(self):
        for ipv6 in (False, True):
            with self.subTest(ipv6=ipv6), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                output = root / 'output'
                mkosi = root / 'pxe/mkosi/bin/mkosi'
                mkosi.parent.mkdir(parents=True)
                mkosi.touch()
                package = root / 'scout.deb'
                package.write_bytes(b'fixture')
                def build(*args, **kwargs):
                    built = output / 'build/output'
                    built.mkdir(exist_ok=True)
                    for name in ['loader.efi', 'scout.squashfs']:
                        (built / name).write_bytes(b'payload')
                argv = ['builder', '--output', str(output)]
                argv += ['--scout-package', str(package)]
                if ipv6:
                    argv.append('--ipv6-boot')
                with patch.object(SCOUT, 'REPO', root), \
                        patch.object(SCOUT.platform, 'machine', return_value='aarch64'), \
                        patch.object(SCOUT.platform, 'system', return_value='Linux'), \
                        patch.object(SCOUT.os, 'geteuid', return_value=0), \
                        patch.object(SCOUT, 'stage_profiles'), \
                        patch.object(SCOUT.subprocess, 'check_output', side_effect=['arm64', 'fixture']), \
                        patch.object(SCOUT.subprocess, 'run', side_effect=build), \
                        patch.object(sys, 'argv', argv), contextlib.redirect_stdout(io.StringIO()):
                    SCOUT.main()
                self.assertEqual((output / 'aarch64/scout.network-profile').read_text(),
                                 'ipv6\n' if ipv6 else 'default\n')
                manifest = json.loads((output / 'build-info.json').read_text())
                self.assertEqual(manifest['ipv6_boot'], ipv6)
                self.assertTrue((output / 'aarch64/scout.efi').is_file())
