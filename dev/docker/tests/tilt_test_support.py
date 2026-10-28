# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Evaluate complete Tilt configuration with external effects recorded, never deployed.

This checks Python-compatible wiring, not Starlark conformance. Helm and the
profile renderer are real; cluster observations are explicit test inputs.
"""

import copy
import json
import os
from pathlib import Path
import subprocess
from types import SimpleNamespace

import yaml

ROOT = Path(__file__).resolve().parents[3]
TILT = ROOT / 'dev/deployment/tilt/Tiltfile'


def baseline():
    return yaml.safe_load((ROOT / 'dev/deployment/stack/values.yaml').read_text())


class TiltResult:
    def __init__(self, options=None, values=None, architecture='aarch64', context='kind-test'):
        self.resources, self.commands, self.objects, self.watches = {}, [], [], []
        self.k8s_resources, self.images, self.repositories = {}, {}, {}
        source = copy.deepcopy(baseline() if values is None else values)

        def fail(message):
            raise ValueError(message)

        def resource(*args, **kwargs):
            name = kwargs.get('name', args[0] if args else None)
            self.resources[name] = dict(kwargs, command=args[1] if len(args) > 1 else None)

        def k8s_resource(*args, **kwargs):
            self.k8s_resources[args[0] if args else kwargs["new_name"]] = kwargs

        def local(command, **kwargs):
            self.commands.append(command)
            if command == ['docker', 'info', '--format', '{{.OSType}}/{{.Architecture}}']:
                return 'linux/' + architecture
            if Path(command[1]).name == 'vault-bootstrap.py' and command[-3:-1] == ['check-mode', '--mode']:
                return ''
            if command[0] == 'helm':
                return subprocess.check_output(command, input=kwargs.get('stdin'), text=True, cwd=TILT.parent)
            raise AssertionError('unexpected load-time command: ' + repr(command))

        def enabled(names):
            self.enabled = names

        def image(name, context, **kwargs):
            self.images[name] = dict(kwargs, context=context)

        def repository(name, url, **kwargs):
            self.repositories[name] = dict(kwargs, url=url)

        def k8s_yaml(text):
            if '\n' not in text and Path(text).is_file():
                text = Path(text).read_text()
            self.objects.extend(yaml.safe_load_all(text))

        noop = lambda *args, **kwargs: None
        self.env = dict(__file__=str(TILT), os=os, load=noop, update_settings=noop,
            config=SimpleNamespace(define_bool=noop, define_string=noop,
                                   parse=lambda: options or {}, set_enabled_resources=enabled),
            k8s_context=lambda: context, fail=fail, local=local,
            read_yaml=lambda path: copy.deepcopy(source) if Path(path) == ROOT / 'dev/deployment/stack/values.yaml' else yaml.safe_load(Path(path).read_text()),
            read_file=lambda path: Path(path).read_text(), encode_json=json.dumps, decode_json=json.loads,
            listdir=lambda path: [str(p) for p in Path(path).iterdir()],
            decode_yaml_stream=lambda text: list(yaml.safe_load_all(text)), encode_yaml=yaml.safe_dump, type=lambda value: type(value).__name__,
            helm_resource=resource, helm_repo=repository, local_resource=resource,
            docker_build=image, k8s_resource=k8s_resource, k8s_kind=noop,
            k8s_yaml=k8s_yaml,
            watch_file=self.watches.append, port_forward=lambda a, b: (a, b), link=lambda *args: args,
            TRIGGER_MODE_MANUAL='manual')
        exec(compile(TILT.read_text(), str(TILT), 'exec'), self.env)
