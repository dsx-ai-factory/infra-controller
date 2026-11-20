# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from deployment_test_support import load_script
import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[3]
IPXE = load_script('build_ipxe', ROOT / 'dev/deployment/lab/build-ipxe.py')


class IpxeArtifactsTest(unittest.TestCase):
    def test_existing_output_is_never_overwritten(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'pxe/ipxe/upstream/src'
            source.mkdir(parents=True)
            (source / 'Makefile').touch()
            output = root / 'output'
            output.mkdir()
            (output / 'keep').write_text('retained')
            with patch.object(IPXE, 'REPO', root), patch.object(IPXE.platform, 'system', return_value='Linux'), \
                    patch.object(IPXE.platform, 'machine', return_value='aarch64'), \
                    patch.object(IPXE.subprocess, 'run') as run, \
                    patch.object(sys, 'argv', ['build-ipxe', '--output', str(output)]), \
                    contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                IPXE.main()
            run.assert_not_called()
            self.assertEqual((output / 'keep').read_text(), 'retained')

    def test_failed_compile_retains_workspace_but_publishes_no_bootloader(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'pxe/ipxe/upstream/src'
            source.mkdir(parents=True)
            (source / 'Makefile').touch()
            output = root / 'output'
            with patch.object(IPXE, 'REPO', root), patch.object(IPXE.platform, 'system', return_value='Linux'), \
                    patch.object(IPXE.platform, 'machine', return_value='x86_64'), \
                    patch.object(IPXE.subprocess, 'run', side_effect=[subprocess.CompletedProcess([], 0), subprocess.CalledProcessError(1, 'make')]), \
                    patch.object(sys, 'argv', ['build-ipxe', '--output', str(output)]), \
                    self.assertRaises(subprocess.CalledProcessError):
                IPXE.main()
            self.assertTrue((output / 'build/src/Makefile').exists())
            self.assertFalse((output / 'x86_64').exists())
