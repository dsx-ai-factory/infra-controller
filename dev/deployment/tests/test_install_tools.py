# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import hashlib
import io
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "install-tools.py"
tools = load_script('install_tools', SCRIPT)


class InstallToolsTests(unittest.TestCase):
    def test_native_artifact_names(self):
        for system, machine, expected in (("Darwin", "arm64", ("darwin", "arm64")),
                                          ("Linux", "x86_64", ("linux", "amd64"))):
            with self.subTest(system=system), patch.object(tools.platform, "system", return_value=system), patch.object(tools.platform, "machine", return_value=machine):
                self.assertEqual(tools.native_platform(), expected)
                url, checksum, member = tools.artifact("tilt", *expected)
                suffix = "mac.arm64.tar.gz" if system == "Darwin" else "linux.x86_64.tar.gz"
                self.assertTrue(url.endswith(suffix))
                self.assertTrue(checksum.endswith("/checksums.txt"))
                self.assertEqual(member, "tilt")

    def test_public_install_extracts_only_binary_and_rejects_bad_checksum_without_replacement(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            payload = root / "helm.tar.gz"
            system, arch = tools.native_platform()
            member = tools.artifact("helm", system, arch)[2]
            with tarfile.open(payload, "w:gz") as archive:
                for name in (member, "../escaped"):
                    data = b"#!/bin/sh\necho fixture\n"
                    entry = tarfile.TarInfo(name)
                    entry.size = len(data)
                    archive.addfile(entry, io.BytesIO(data))
            raw = root / "kind-raw"
            raw.write_bytes(b"#!/bin/sh\necho kind-fixture\n")
            fake_bin = root / "fake-bin"
            fake_bin.mkdir()
            curl = fake_bin / "curl"
            curl.write_text(r"""#!/usr/bin/env python3
import hashlib, os, pathlib, sys
url = sys.argv[-1]
output = pathlib.Path(sys.argv[sys.argv.index('--output') + 1])
root = pathlib.Path(os.environ['FIXTURE'])
name = url.rsplit('/', 1)[1]
payload = root / ('helm.tar.gz' if name.startswith('helm-') else 'kind-raw')
if name.endswith('.sha256sum'):
    digest = '0' * 64 if os.environ.get('CORRUPT') and name.startswith('helm-') else hashlib.sha256(payload.read_bytes()).hexdigest()
    output.write_text(digest + '  ' + name.removesuffix('.sha256sum') + '\n')
else:
    output.write_bytes(payload.read_bytes())
""")
            curl.chmod(0o755)
            prefix = root / "installed with spaces"
            env = {**os.environ, "PATH": str(fake_bin) + os.pathsep + os.environ['PATH'], "FIXTURE": str(root)}
            command = ["python3", str(SCRIPT), "--bin-dir", str(prefix), "kind", "helm"]
            result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(subprocess.check_output([str(prefix / 'helm')], text=True), 'fixture\n')
            self.assertFalse((prefix / 'escaped').exists())
            (prefix / 'kind').write_text('retained')
            result = subprocess.run(command, env={**env, "CORRUPT": "1"}, capture_output=True, text=True, timeout=20)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('checksum verification failed', result.stderr)
            self.assertEqual((prefix / 'kind').read_text(), 'retained')
            self.assertEqual(sorted(p.name for p in prefix.iterdir()), ['helm', 'kind'])

    def test_publisher_build_path_matches_only_the_requested_filename(self):
        with tempfile.TemporaryDirectory() as directory:
            payload, manifest = Path(directory) / 'payload', Path(directory) / 'checksum'
            payload.write_bytes(b'published tool')
            digest = hashlib.sha256(payload.read_bytes()).hexdigest()
            manifest.write_text(digest + '  /build/release/devspace-linux-arm64\n')
            tools.verify(payload, manifest, 'devspace-linux-arm64')
            with self.assertRaisesRegex(ValueError, 'checksum'):
                tools.verify(payload, manifest, 'devspace-linux-amd64')

    def test_archive_link_and_destination_link_are_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / 'tilt').symlink_to(root / 'other')
            with self.assertRaisesRegex(ValueError, 'nonregular destination'):
                tools.install(root, ['tilt'])
            (root / 'tilt').unlink()
            def download(url, destination):
                if url.endswith('checksums.txt'):
                    payload = destination.with_name('tilt.download')
                    destination.write_text(hashlib.sha256(payload.read_bytes()).hexdigest() + '  ' + tools.artifact('tilt', *tools.native_platform())[0].rsplit('/', 1)[1])
                else:
                    with tarfile.open(destination, 'w:gz') as archive:
                        entry = tarfile.TarInfo('tilt')
                        entry.type = tarfile.SYMTYPE
                        entry.linkname = '/bin/sh'
                        archive.addfile(entry)
            with patch.object(tools, 'download', side_effect=download), self.assertRaisesRegex(ValueError, 'not a regular file'):
                tools.install(root, ['tilt'])
            self.assertFalse((root / 'tilt').exists())

    def test_invalid_selection_fails_before_creating_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / 'missing'
            for selection in (['unknown'], ['kind', 'kind']):
                result = subprocess.run(['python3', str(SCRIPT), '--bin-dir', str(destination), *selection], capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 2)
                self.assertFalse(destination.exists())


if __name__ == '__main__':
    unittest.main()
