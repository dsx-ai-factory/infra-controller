# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
from deployment_test_support import load_script
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
OS = load_script('os_artifact', ROOT / 'lab/build-os-artifact.py')
PAYLOAD = b'QFI\xfb' + b'fixture qcow2'


class OsArtifactTest(unittest.TestCase):
    def test_build_records_verified_source_and_native_parent_without_changing_parent_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(output=Path(directory) / 'output', boot_artifact_image='boot:fixture',
                                      image='os:fixture', source_url='https://images.example/ubuntu.img',
                                      sha256=hashlib.sha256(PAYLOAD).hexdigest())
            calls = []
            def run(command, **kwargs):
                calls.append(command)
                result = ''
                if command[1] == 'info': result = json.dumps({'OSType': 'linux', 'Architecture': 'aarch64'})
                elif command[1:3] == ['image', 'inspect']:
                    result = json.dumps([{'Id': 'sha256:parent' if command[3] == 'boot:fixture' else 'sha256:built', 'Os': 'linux', 'Architecture': 'arm64', 'RepoTags': ['boot:fixture']}])
                elif command[0] == 'curl': Path(command[command.index('--output') + 1]).write_bytes(PAYLOAD)
                elif command[1] == 'build':
                    self.assertEqual(sorted(p.name for p in Path(command[-1]).iterdir()), ['ubuntu.img'])
                    self.assertIn('ARTIFACT_ARCH=aarch64', command)
                return subprocess.CompletedProcess(command, 0, stdout=result)
            with patch.object(OS.platform, 'system', return_value='Linux'), patch.object(OS.platform, 'machine', return_value='aarch64'), patch.object(OS, 'run', side_effect=run):
                OS.build(args)
            manifest = json.loads((args.output / 'build-info.json').read_text())
            self.assertEqual(manifest['sha256'], args.sha256)
            self.assertEqual(manifest['boot_artifact_image_id'], 'sha256:parent')
            self.assertEqual(manifest['image_id'], 'sha256:built')
            self.assertEqual(manifest['pxe_path'], '/public/blobs/internal/aarch64/ubuntu.img')
            self.assertEqual((args.output / 'ubuntu.img').read_bytes(), PAYLOAD)
            tags = [c for c in calls if c[1] == 'tag']
            self.assertEqual(len(tags), 1)
            self.assertEqual(tags[0][2], 'sha256:parent')
            build = next(c for c in calls if c[1] == 'build')
            self.assertIn('BOOT_ARTIFACT_IMAGE=' + tags[0][3], build)
            payload_check = next(c for c in calls if c[1] == 'run')
            self.assertIn('sha256:parent', payload_check)
            self.assertEqual(calls[-1], ['docker', 'image', 'rm', tags[0][3]])

    def test_failed_build_removes_only_the_temporary_parent_tag(self):
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(output=Path(directory) / 'output', boot_artifact_image='boot:fixture',
                                      image='os:fixture', source_url='https://images.example/image', sha256=hashlib.sha256(PAYLOAD).hexdigest())
            parent = {'Id': 'sha256:parent', 'Os': 'linux', 'Architecture': 'arm64', 'RepoTags': ['boot:fixture']}
            calls = []
            def run(command, **kwargs):
                calls.append(command)
                result = ''
                if command[1] == 'info': result = json.dumps({'OSType': 'linux', 'Architecture': 'aarch64'})
                elif command[0] == 'curl': Path(command[command.index('--output') + 1]).write_bytes(PAYLOAD)
                elif command[1] == 'build': raise subprocess.CalledProcessError(42, command)
                elif command[1:3] == ['image', 'rm']: raise subprocess.CalledProcessError(43, command)
                return subprocess.CompletedProcess(command, 0, stdout=result)
            with patch.object(OS.platform, 'system', return_value='Linux'), patch.object(OS.platform, 'machine', return_value='aarch64'), patch.object(OS, 'inspect', return_value=parent), patch.object(OS, 'run', side_effect=run), self.assertRaises(subprocess.CalledProcessError) as failure:
                OS.build(args)
            self.assertEqual(failure.exception.returncode, 42)
            tag = next(c[-1] for c in calls if c[1] == 'tag')
            self.assertEqual(calls[-1], ['docker', 'image', 'rm', tag])
            self.assertEqual(list(args.output.iterdir()), [])

    def test_cli_rejects_nonlocal_output_names_before_contacting_docker(self):
        for name in ('name', 'registry:5000/name:tag', 'namespace/name:tag', 'name@sha256:' + '0' * 64, 'name:' + 'x' * 129):
            with self.subTest(name=name):
                result = subprocess.run(['python3', str(ROOT / 'lab/build-os-artifact.py'), '--boot-artifact-image', 'boot:fixture', '--source-url', 'https://images.example/image', '--sha256', '0' * 64, '--image', name, '--output', '/unused'], capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode, 2)
                self.assertIn('explicit local name:tag', result.stderr)

    def test_untrusted_or_wrong_format_payload_never_reaches_docker_build(self):
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(output=Path(directory) / 'output', boot_artifact_image='boot:fixture',
                                      image='os:fixture', source_url='https://images.example/image')
            for content, digest, reason in [(PAYLOAD, '0' * 64, 'SHA256 mismatch'),
                                             (b'raw image', hashlib.sha256(b'raw image').hexdigest(), 'qcow2')]:
                with self.subTest(reason=reason):
                    args.sha256 = digest
                    def run(command, **kwargs):
                        if command[1] == 'info': result = json.dumps({'OSType': 'linux', 'Architecture': 'aarch64'})
                        elif command[1:3] == ['image', 'inspect']:
                            result = json.dumps([{'Id': 'parent', 'Os': 'linux', 'Architecture': 'arm64', 'RepoTags': ['boot:fixture']}])
                        elif command[1:3] == ['image', 'ls'] or command[1] == 'run': result = ''
                        elif command[0] == 'curl':
                            Path(command[command.index('--output') + 1]).write_bytes(content)
                            result = ''
                        else: self.fail('unverified payload reached an image mutation: ' + str(command))
                        return subprocess.CompletedProcess(command, 0, stdout=result)
                    with patch.object(OS.platform, 'system', return_value='Linux'), patch.object(OS.platform, 'machine', return_value='aarch64'), \
                            patch.object(OS, 'run', side_effect=run), self.assertRaisesRegex(ValueError, reason):
                        OS.build(args)
                    self.assertEqual(list(args.output.iterdir()), [])
                    args.output.rmdir()

    def test_native_mismatch_and_existing_tag_fail_before_output_creation(self):
        with tempfile.TemporaryDirectory() as directory:
            args = argparse.Namespace(output=Path(directory) / 'output', boot_artifact_image='boot:fixture', image='os:fixture')
            for arch, parent_tags, tags, message in [('amd64', ['boot:fixture'], '', 'architecture'),
                                                       ('arm64', ['boot:fixture'], 'os:fixture\n', 'already exists'),
                                                       ('arm64', [], '', 'untagged'),
                                                       ('arm64', ['boot:fixture'], '', 'missing payload')]:
                with self.subTest(message=message):
                    def run(command, **kwargs):
                        if command[1] == 'info': result = {'OSType': 'linux', 'Architecture': 'aarch64'}
                        elif command[1:3] == ['image', 'inspect']: result = [{'Id': 'parent', 'Os': 'linux', 'Architecture': arch, 'RepoTags': parent_tags}]
                        elif command[1:3] == ['image', 'ls']: return subprocess.CompletedProcess(command, 0, stdout=tags)
                        elif command[1] == 'run':
                            self.assertIn('parent', command)
                            raise ValueError('missing payload')
                        else: self.fail('unexpected mutation: ' + str(command))
                        return subprocess.CompletedProcess(command, 0, stdout=json.dumps(result))
                    with patch.object(OS.platform, 'system', return_value='Linux'), patch.object(OS.platform, 'machine', return_value='aarch64'), patch.object(OS, 'run', side_effect=run), self.assertRaisesRegex(ValueError, message): OS.build(args)
                    self.assertFalse(args.output.exists())

    def test_source_urls_do_not_embed_credentials_or_query_tokens(self):
        for value in ('https://user:secret@example/image', 'https://example/image?token=secret', 'file:///image', 'https://example/image#fragment'):
            with self.subTest(value=value), self.assertRaises(argparse.ArgumentTypeError): OS.source_url(value)
        self.assertEqual(OS.source_url('http://[fd00::1]:8080/image'), 'http://[fd00::1]:8080/image')


if __name__ == '__main__': unittest.main()
