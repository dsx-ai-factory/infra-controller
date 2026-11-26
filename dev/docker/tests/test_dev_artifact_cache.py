# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Check artifact handoff without storing Rust binaries in builder layers."""

from pathlib import Path
import re
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[3]


class DevArtifactCacheTest(unittest.TestCase):
    def test_runtime_checks_the_builder_digest_before_accepting_cached_artifacts(self):
        source = (ROOT / 'dev/docker/Dockerfile.dev-services').read_text()
        stages = dict(re.findall(r'^FROM [^\n]* AS ([\w-]+)\n(.*?)(?=^FROM |\Z)', source, re.M | re.S))
        targets = {
            'api': ('carbide-api', 'nico-admin-cli'), 'pxe': ('carbide',),
            'bmc-proxy': ('carbide-bmc-proxy',), 'dhcp': ('libdhcp.so',),
            'machine-a-tron': ('machine-a-tron',), 'ssh-console': ('ssh-console',),
            'mat-protocol-gateway': ('mat-protocol-gateway',),
            'nico-postgres-exporter': ('nico-postgres-exporter',), 'bmc-target': ('bmc-mock',),
        }
        for target, artifacts in targets.items():
            with self.subTest(target=target):
                builder = stages[('bmc-mock' if target == 'bmc-target' else target) + '-builder']
                runtime = stages[target]
                self.assertNotRegex(builder, r'\bcp /cargo-target/\S+ /out/')
                self.assertLess(runtime.index('ARG TARGETARCH'), runtime.index('id=nico-dev-cargo-target-${TARGETARCH}'))
                commands = [line for line in runtime.replace('\\\n', ' ').splitlines()
                            if line.startswith('RUN ') and 'sha256sum' in line]
                self.assertTrue(commands)
                for command in commands:
                    self.assertIn('id=nico-dev-cargo-target-${TARGETARCH}', command)
                    self.assertIn('sharing=locked', command)
                setup = re.findall(r'^RUN (mkdir -p [^\n]+)', runtime, re.M)
                setup += [command.replace('-o nico -g nico ', '') for command in
                          re.findall(r'(?<![\w-])(install -d -o nico -g nico [^\n]+)', runtime)]
                shell = '\n'.join(setup) + '\n' + '\n'.join(re.sub(r'--mount=\S+\s*', '', command[4:].replace('\\\n', ' '))
                                  for command in commands)
                # File ownership belongs to the native image check; this fixture
                # exercises cache integrity without requiring a root test runner.
                shell = re.sub(r'&& chown [^&\n]+', '', shell)
                for condition in ('matching', 'modified', 'missing'):
                    with self.subTest(cache=condition), tempfile.TemporaryDirectory() as directory:
                        root = Path(directory)
                        for artifact in artifacts:
                            original = re.search(r'sha256sum <(/cargo-target/\S+/' + re.escape(artifact) + r') >', builder)
                            self.assertIsNotNone(original)
                            path = root / original[1].lstrip('/')
                            path.parent.mkdir(parents=True, exist_ok=True)
                            path.write_bytes(b'original build output\n')
                            digest = subprocess.check_output(['sha256sum'], input=path.read_bytes())
                            manifest = root / 'tmp' / (artifact + '.sha256sum')
                            manifest.parent.mkdir(exist_ok=True)
                            manifest.write_bytes(digest)
                            if condition == 'modified':
                                path.write_bytes(b'another build replaced the cache\n')
                            elif condition == 'missing':
                                path.unlink()
                        # Execute the actual runtime handoff with paths isolated in this fixture.
                        isolated = re.sub(r'(?<![\w-])/(?:cargo-target|tmp|opt|usr|mnt|var|forge-boot-artifacts|mat-protocol-gateway)[^\s";)]*',
                                          lambda match: str(root) + match[0], shell)
                        # The distribution supplies this directory; service-specific
                        # directories must be created by the real Dockerfile commands.
                        (root / 'usr/local/bin').mkdir(parents=True)
                        result = subprocess.run(['sh', '-ec', isolated], capture_output=True, text=True)
                        self.assertEqual(result.returncode == 0, condition == 'matching', result.stderr)


if __name__ == '__main__':
    unittest.main()
