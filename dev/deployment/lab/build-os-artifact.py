#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Add one verified native Ubuntu qcow2 image to a local boot-artifact bundle."""

import argparse
import hashlib
import json
import platform
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import urllib.parse
import uuid

HERE = Path(__file__).resolve().parent
ARCHITECTURES = {'aarch64': ('arm64', 'aarch64'), 'arm64': ('arm64', 'aarch64'),
                 'x86_64': ('amd64', 'x86_64'), 'amd64': ('amd64', 'x86_64')}


def run(args, **kwargs):
    return subprocess.run(args, check=True, text=True, **kwargs)


def sha256(path):
    digest = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(block)
    return digest.hexdigest()


def source_url(value):
    url = urllib.parse.urlsplit(value)
    if url.scheme not in ('http', 'https') or not url.hostname or url.username or url.password or url.query or url.fragment:
        raise argparse.ArgumentTypeError('use an HTTP(S) image URL without credentials, query or fragment')
    return value


def digest_argument(value):
    if not re.fullmatch('[0-9a-fA-F]{64}', value):
        raise argparse.ArgumentTypeError('use exactly 64 hexadecimal SHA256 digits')
    return value.lower()


def inspect(image):
    return json.loads(run(['docker', 'image', 'inspect', image], stdout=subprocess.PIPE).stdout)[0]


def verify_image(path, expected):
    if sha256(path) != expected:
        raise ValueError('OS image SHA256 mismatch; no image was built')
    with path.open('rb') as stream:
        if stream.read(4) != b'QFI\xfb':
            raise ValueError('this native lab fixture requires a qcow2 source')


def build(args):
    native = ARCHITECTURES.get(platform.machine())
    if not native or platform.system() not in ('Linux', 'Darwin'):
        raise ValueError('use a native ARM64 or AMD64 Linux/macOS client')
    docker = json.loads(run(['docker', 'info', '--format', '{{json .}}'], stdout=subprocess.PIPE).stdout)
    if docker['OSType'] != 'linux' or ARCHITECTURES.get(docker['Architecture']) != native:
        raise ValueError('Docker must be native Linux with the same architecture as the client')
    parent = inspect(args.boot_artifact_image)
    if not parent.get('RepoTags'):
        raise ValueError('boot-artifact input must retain a local tag; untagged images are not adopted')
    if parent['Os'] != 'linux' or parent['Architecture'] != native[0]:
        raise ValueError('boot-artifact image architecture does not match the native client')
    # Listing is authoritative; a failed inspect must not be treated as permission to overwrite.
    tags = run(['docker', 'image', 'ls', '--format', '{{.Repository}}:{{.Tag}}'], stdout=subprocess.PIPE).stdout.splitlines()
    if args.image in tags:
        raise ValueError('output image tag already exists; select a new tag')
    run(['docker', 'run', '--rm', '--network', 'none', '--entrypoint', '/bin/sh', parent['Id'], '-ec',
         'for file in ipxe.efi scout.efi scout.squashfs qcow-imager.efi; do test -s "/$1/$file"; done', '_', native[1]])
    output = args.output.expanduser().absolute()
    output.mkdir(parents=False, exist_ok=False)
    base_tag = 'nico-lab-os-parent:' + uuid.uuid4().hex
    with tempfile.TemporaryDirectory(prefix='build-', dir=output) as temporary:
        work = Path(temporary)
        payload = work / 'ubuntu.img'
        run(['curl', '--fail', '--location', '--silent', '--show-error', '--retry', '3',
             '--proto', '=http,https', '--proto-redir', '=http,https', '--connect-timeout', '30',
             '--max-time', '1800', '--output', str(payload), args.source_url])
        verify_image(payload, args.sha256)
        run(['docker', 'tag', parent['Id'], base_tag])
        try:
            run(['docker', 'build', '--platform', 'linux/' + native[0], '--file', str(HERE / 'Dockerfile.os-artifact'),
                 '--build-arg', 'BOOT_ARTIFACT_IMAGE=' + base_tag, '--build-arg', 'ARTIFACT_ARCH=' + native[1],
                 '--tag', args.image, str(work)])
            built = inspect(args.image)
            if built['Architecture'] != native[0]:
                raise ValueError('built image architecture differs from the native client')
            info = {'architecture': native[0], 'artifact_directory': native[1], 'source_url': args.source_url,
                    'sha256': args.sha256, 'boot_artifact_image_id': parent['Id'], 'image': args.image,
                    'image_id': built['Id'], 'pxe_path': '/public/blobs/internal/' + native[1] + '/ubuntu.img'}
            (output / 'build-info.json').write_text(json.dumps(info, indent=2) + '\n')
            payload.rename(output / 'ubuntu.img')
        finally:
            try:
                run(['docker', 'image', 'rm', base_tag], stdout=subprocess.DEVNULL)
            except (OSError, subprocess.SubprocessError) as error:
                print(f'Warning: could not remove temporary tag {base_tag}: {error}', file=sys.stderr)
    print(json.dumps(info, indent=2))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--boot-artifact-image', required=True, help='existing local native Scout/imager bundle')
    parser.add_argument('--source-url', required=True, type=source_url, help='native Ubuntu cloud qcow2 URL, without authentication')
    parser.add_argument('--sha256', required=True, type=digest_argument, help='trusted publisher SHA256 for that architecture')
    parser.add_argument('--image', required=True, help='new local output image name:tag; existing tags are refused')
    parser.add_argument('--output', required=True, type=Path, help='new output directory under an existing parent; retains image and manifest')
    args = parser.parse_args()
    if not re.fullmatch(r'[a-z0-9]+(?:[._-][a-z0-9]+)*:[A-Za-z0-9_][A-Za-z0-9_.-]{0,127}', args.image):
        parser.error('--image must be an explicit local name:tag without a registry, namespace or digest')
    try:
        build(args)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f'build-os-artifact: {error}\n')


if __name__ == '__main__':
    main()
