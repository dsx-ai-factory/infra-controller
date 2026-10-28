#!/usr/bin/env python3
# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Durable creation intents for explicitly owned native lab Core resources."""
import json
import os
import subprocess
from pathlib import Path
from lab_io import run, save

HERE = Path(__file__).resolve().parent


class CoreJournal:
    def __init__(self, directory, state):
        self.directory, self.state = directory, state
        self.environment = dict(os.environ, LOCAL_DEV_KUBE_CONTEXT=state['context'],
                                LOCAL_DEV_NAMESPACE=state['namespace'], FORGE_CLIENT_REQUEST_TIMEOUT_SECS='30')

    def checkpoint(self):
        save(self.directory / 'state.json', self.state)

    def admin(self, *args, read=False, operation=None):
        command = ['bash', str(HERE.parents[1] / 'core-admin.sh')]
        if read:
            command += ['-f', 'json']
        else:
            command += ['--cloud-unsafe-op', 'Initialize owned native lab resources']
        sensitive = '--bmc-password' in args
        log = self.directory / (operation + '.log') if operation else None
        try:
            output = run([*command, *args], env=self.environment, log_path=None if sensitive else log)
        except (RuntimeError, subprocess.SubprocessError):
            if not sensitive:
                raise
            message = 'expected-machine registration failed; inspect Core and resume the retained journal'
            if log:
                log.write_text(message + '\n')
            # TimeoutExpired includes argv. Suppress the original exception and
            # command output rather than publishing the initial BMC credential.
            raise RuntimeError(message) from None
        if sensitive:
            if log:
                log.write_text('expected-machine registration command completed\n')
            return ''
        return json.loads(output) if read else output

    def verify_cluster(self):
        if self.state['version'] != 1 or not self.state['context'].startswith('kind-'):
            raise ValueError('unsupported lab journal or context')
        kube = ['kubectl', '--context', self.state['context']]
        identity = json.loads(run([*kube, 'get', 'namespace', 'kube-system', '-o', 'json']))['metadata']['uid']
        if identity != self.state['cluster']:
            raise ValueError('cluster identity changed; retained resources cannot be adopted')
        nodes = json.loads(run([*kube, 'get', 'nodes', '-o', 'json']))['items']
        if len(nodes) != 1 or nodes[0]['status']['nodeInfo']['architecture'] != self.state.get('architecture', self.state.get('artifact', {}).get('architecture')):
            raise ValueError('select a single-node kind lab matching the recorded native architecture')

    def ensure(self, name, find, command):
        current = find()
        if current:
            if not self.state.get(name + '_intent'):
                raise ValueError('resource exists without this journal\'s creation intent')
            return current
        if self.state.get(name + '_intent'):
            raise ValueError(name + ' creation has an unknown outcome or the resource was removed; '
                             'inspect the retained log and Core, then resume to discover it; creation will not be repeated')
        self.state[name + '_intent'] = True
        self.checkpoint()
        self.admin(*command, operation=name + '-create')
        current = find()
        if not current:
            raise ValueError(name + ' was not visible after creation; resume to recheck without repeating it')
        return current
