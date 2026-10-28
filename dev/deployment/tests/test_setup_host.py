# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
import os
import platform
import shlex
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'setup-host.sh'


class SetupHostTest(unittest.TestCase):
    def test_fresh_runtime_is_configured_but_existing_daemon_is_not_restarted(self):
        source = SCRIPT.read_text()
        prepare = source[source.index('prepare_host() {'):source.rindex('\nprepare_host')]
        for state, allowed in [('fresh', True), ('existing', True), ('partial', False), ('buildx', False), ('stopped', False), ('group', False)]:
            with self.subTest(state=state), tempfile.TemporaryDirectory() as directory:
                helper = Path(directory) / 'docker-host.sh'
                helper.write_text("install_docker() { echo INSTALL; }\nconfigure_docker() { echo CONFIGURE; }\n")
                program = """set -euo pipefail
log() { :; }
die() { echo "$*" >&2; exit 1; }
command() {
  if [[ "$1" == -v ]]; then
    [[ "$STATE" != fresh ]] || return 1
    [[ "$STATE" != partial || "$2" != containerd ]]
  else builtin command "$@"; fi
}
docker() {
  [[ "$STATE" != buildx || "$1" != buildx ]] &&
    [[ "$STATE" != stopped || "$1" != --host ]]
}
getent() { [[ "$STATE" != group ]]; }
apt-get() { echo PACKAGES; }
usermod() { echo ACCESS; }
run_as_user() { printf 'USER <%s>\\n' "$*"; }
""" + prepare + '\nprepare_host\n'
                result = subprocess.run(['bash', '-c', program], env={**os.environ, 'STATE': state, 'DEV_USER': 'fixture', 'SCRIPT_DIR': directory}, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode == 0, allowed, result.stderr)
                if state == 'fresh':
                    self.assertIn('INSTALL\nCONFIGURE\n', result.stdout)
                elif state == 'existing':
                    self.assertNotIn('INSTALL', result.stdout)
                    self.assertNotIn('CONFIGURE', result.stdout)
                    self.assertIn('ACCESS', result.stdout)
                else:
                    self.assertNotIn('PACKAGES', result.stdout)
                    self.assertIn('docker access group' if state == 'group' else 'repair it before bootstrap', result.stderr)
                if allowed:
                    self.assertIn('USER <python3 ' + directory + '/install-tools.py>', result.stdout)

    @unittest.skipIf(os.geteuid() == 0 or platform.system() != 'Linux', 'non-root Linux sudo forwarding')
    def test_non_root_invoker_takes_precedence_over_inherited_sudo_user(self):
        release_path = Path('/etc/os-release')
        if not release_path.exists():
            self.skipTest('sudo forwarding requires Ubuntu 22.04+')
        os_release = dict(line.split('=', 1) for line in
                          release_path.read_text().splitlines()
                          if '=' in line and not line.startswith('#'))
        distro = shlex.split(os_release.get('ID', ''))
        version = shlex.split(os_release.get('VERSION_ID', ''))
        if distro != ['ubuntu'] or not version or tuple(map(int, version[0].split('.'))) < (22, 4):
            self.skipTest('sudo forwarding requires Ubuntu 22.04+')
        with tempfile.TemporaryDirectory() as directory:
            sudo = Path(directory) / 'sudo'
            sudo.write_text('#!/bin/sh\nprintf "<%s>\\n" "$@"\n')
            sudo.chmod(0o755)
            result = subprocess.run(['bash', str(SCRIPT)], env={**os.environ, 'USER': 'developer', 'SUDO_USER': 'different', 'PATH': directory + os.pathsep + os.environ['PATH']}, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('<--user>\n<developer>', result.stdout)
            self.assertNotIn('different', result.stdout)

    def test_unsupported_platform_fails_before_sudo(self):
        with tempfile.TemporaryDirectory() as directory:
            for name, body in [('uname', 'echo Unsupported'), ('sudo', 'echo UNEXPECTED_SUDO; exit 99')]:
                path = Path(directory) / name
                path.write_text('#!/bin/sh\n' + body + '\n')
                path.chmod(0o755)
            result = subprocess.run(['bash', str(SCRIPT), '--user', 'fixture'],
                                    env={**os.environ, 'PATH': directory + os.pathsep + os.environ['PATH']},
                                    capture_output=True, text=True, timeout=10)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('[setup-host]', result.stderr)
            self.assertNotIn('UNEXPECTED_SUDO', result.stdout)

    def test_public_argument_errors_precede_sudo(self):
        for arguments in (['--user'], ['--user', '--help'], ['--unknown']):
            with self.subTest(arguments=arguments):
                result = subprocess.run(['bash', str(SCRIPT), *arguments], capture_output=True, text=True, timeout=10)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('[setup-host]', result.stderr)
        result = subprocess.run(['bash', str(SCRIPT), '--help'], capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 0)
        self.assertIn('Existing complete, running Docker is preserved.', result.stdout)


if __name__ == '__main__':
    unittest.main()
