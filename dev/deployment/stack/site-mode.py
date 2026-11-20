#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Check or claim the retained target mode of a local kind site."""

import argparse
import json
import subprocess
import sys

NAME = 'nico-local-site-mode'
OWNER = 'nico-local-development'
LABEL = 'app.kubernetes.io/managed-by'
RELEASES = {'mat': 'nico-machine-a-tron', 'bmc': 'nico-pxe'}


class SiteMode:
    def __init__(self, context, namespace, mode):
        if not context.startswith('kind-'):
            raise ValueError('site mode requires a kind- context')
        self.context, self.namespace, self.mode = context, namespace, mode

    def run(self, command, data=None):
        result = subprocess.run(command, input=data, text=True, capture_output=True, timeout=45)
        if result.returncode:
            raise ValueError('site mode check failed; verify cluster access and resource ownership')
        return result.stdout

    def kube(self, namespace, *args, data=None):
        return self.run(['kubectl', '--context', self.context, '--request-timeout=30s',
                         '-n', namespace, *args], data)

    def get(self, namespace, kind, name):
        value = self.kube(namespace, 'get', kind, name, '--ignore-not-found', '-o', 'json')
        return json.loads(value) if value.strip() else None

    def check(self):
        marker = self.get('kube-system', 'configmap', NAME)
        if marker:
            if marker['metadata'].get('labels', {}).get(LABEL) != OWNER:
                raise ValueError('site mode marker has a different owner')
            if marker.get('data', {}).get('mode') != self.mode:
                raise ValueError('Cannot change between MAT and BMC lab modes; use a new kind cluster')
            return True
        releases = self.run(['helm', 'list', '--kube-context', self.context, '--namespace',
                             self.namespace, '--filter', '^nico-(machine-a-tron|pxe)$', '--short', '--all']).splitlines()
        opposite = RELEASES['mat' if self.mode == 'bmc' else 'bmc']
        if opposite in releases:
            raise ValueError('Cannot change between MAT and BMC lab modes; use a new kind cluster')
        if RELEASES[self.mode] not in releases and self.get(self.namespace, 'deployment', 'nico-api'):
            raise ValueError('unmarked partial Core deployment has unknown target mode; use a new kind cluster')
        return False

    def claim(self):
        if self.check():
            return
        marker = {'apiVersion': 'v1', 'kind': 'ConfigMap', 'immutable': True,
                  'metadata': {'name': NAME, 'namespace': 'kube-system', 'labels': {LABEL: OWNER}},
                  'data': {'mode': self.mode}}
        try:
            self.kube('kube-system', 'create', '-f', '-', data=json.dumps(marker))
        except ValueError:
            # Another invocation may have claimed the same mode concurrently.
            if not self.check():
                raise


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--context', required=True)
    parser.add_argument('--namespace', required=True)
    parser.add_argument('--mode', choices=RELEASES, required=True)
    parser.add_argument('action', choices=('check', 'claim'))
    args = parser.parse_args()
    try:
        mode = SiteMode(args.context, args.namespace, args.mode)
        getattr(mode, args.action)()
    except (ValueError, OSError, subprocess.TimeoutExpired) as error:
        print(str(error), file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    sys.exit(main())
