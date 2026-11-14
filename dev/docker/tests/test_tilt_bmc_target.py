# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
import copy
import importlib.util
import json
import subprocess
import sys

import yaml
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

ROOT = Path(__file__).resolve().parents[3]
TILTFILE = ROOT / 'dev/deployment/lab/Tiltfile'


class TargetWiringTest(unittest.TestCase):
    def test_helm_configuration_is_accepted_by_the_native_runtime(self):
        lab = ROOT / 'dev/deployment/lab'
        sys.path.insert(0, str(lab))
        try:
            spec = importlib.util.spec_from_file_location('native_target', lab / 'target.py')
            runtime = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(runtime)
        finally:
            sys.path.pop(0)
        for hugepages, ipv6 in [(False, ''), (True, ''), (True, 'fd42:8124:40::1/64')]:
            with self.subTest(hugepages=hugepages, ipv6=ipv6):
                result = subprocess.run(
                    ['helm', 'template', 'target-two', str(ROOT / 'helm/nico-bmc-target'),
                     '--namespace', 'test-lab', '--show-only', 'templates/configmap.yaml',
                     '--set', 'nodeName=node,clusterUID=cluster,storage.existingClaim=data,service.clusterIP=10.96.40.10',
                     '--set', f'architecture=arm64,target.instanceIndex=2,target.memoryMiB=8192,target.hugepages={str(hugepages).lower()}',
                     '--set-string', 'network.bridgeAddressV6=' + ipv6],
                    capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 0, result.stderr)
                config = runtime.configuration(json.loads(yaml.safe_load(result.stdout)['data']['config.json']))
                self.assertEqual(config['owner'], 'test-lab/target-two')
                self.assertEqual(config['memoryMiB'], 8192)
                self.assertEqual(config['mac'], '02:00:00:02:00:02')
                self.assertEqual(config['architecture'], 'arm64')
                self.assertEqual(config.get('hugepages', False), hugepages)
                self.assertEqual(config.get('bridgeAddressV6', ''), ipv6)


def fail(message):
    raise ValueError(message)


class LabTiltTests(unittest.TestCase):
    def setUp(self):
        defaults = yaml.safe_load((ROOT / 'helm/nico-bmc-target/values.yaml').read_text())
        self.values = {
            '/lab/one.json': dict(architecture='arm64', storage={'existingClaim': 'one-data'},
                                 network={'bridgeAddress': defaults['network']['bridgeAddress'], 'bridgeAddressV6': 'fd42:8124:40::1/64'},
                                 target={'instanceIndex': defaults['target']['instanceIndex']},
                                 service={'clusterIP': 'fd00::10', 'targetPort': defaults['service']['targetPort']}),
            '/lab/two.json': dict(architecture='arm64', storage={'existingClaim': 'two-data'},
                                 network={'bridgeAddress': '10.84.41.1/24', 'bridgeAddressV6': 'fd42:8124:41::1/64'},
                                 target={'instanceIndex': 2}, service={'clusterIP': 'fd00::11', 'targetPort': 2444}),
        }
        self.build, self.deploy, self.settings = Mock(), Mock(), Mock()

    def evaluate(self, options, plan=None, architecture='aarch64', context='kind-lab'):
        # Tilt joins path components without Python's absolute-path reset.
        tilt_os = SimpleNamespace(path=SimpleNamespace(
            realpath=os.path.realpath, dirname=os.path.dirname, abspath=os.path.abspath,
            join=lambda *parts: os.path.normpath('/'.join(parts))))
        environment = dict(__file__=str(TILTFILE), os=tilt_os, load=Mock(), update_settings=self.settings,
                           k8s_context=lambda: context, fail=fail, custom_build=self.build,
                           helm_resource=self.deploy, read_yaml=lambda path: self.values[path],
                           read_json=lambda path: plan or {'one': 'one.json', 'two': '/lab/two.json'},
                           local=lambda *args, **kwargs: architecture,
                           type=lambda value: 'dict' if isinstance(value, dict) else 'string' if isinstance(value, str) else 'other',
                           config=SimpleNamespace(define_string=Mock(), parse=lambda: options))
        exec(TILTFILE.read_text(), environment)

    def test_native_images_and_chart_share_one_build_and_values_contract(self):
        for platform, architecture in [('aarch64', 'arm64'), ('x86_64', 'amd64')]:
            with self.subTest(platform=platform):
                self.build.reset_mock()
                self.deploy.reset_mock()
                self.values['/lab/one.json']['architecture'] = architecture
                self.evaluate({'values': '/lab/one.json', 'release': 'custom-target', 'namespace': 'custom-lab'}, architecture=platform)
                build, chart = self.build.call_args, self.deploy.call_args
                self.assertIn('make images-dev DEV_TARGET=bmc-target', build.kwargs['command'])
                self.assertEqual(build.kwargs['dir'], str(ROOT))
                self.assertEqual(chart.kwargs['release_name'], 'custom-target')
                self.assertEqual(chart.kwargs['namespace'], 'custom-lab')
                self.assertEqual(chart.kwargs['image_deps'], [build.args[0]])
                self.assertEqual(chart.kwargs['flags'][:2], ['--values', '/lab/one.json'])
                self.assertIn('--wait', chart.kwargs['flags'])

    def test_build_watches_runtime_without_acceptance_or_artifact_workflows(self):
        self.evaluate({'values': '/lab/one.json', 'release': 'custom-target'})
        dependencies = [Path(path) for path in self.build.call_args.kwargs['deps']]
        lab = ROOT / 'dev/deployment/lab'
        for name in ('preflight.py', 'target.py', 'network.py', 'dhcp6_relay.py'):
            self.assertIn(lab / name, dependencies)
        for name in ('acceptance/lifecycle.py', 'acceptance/README.md', 'README.md',
                     'build-scout.py', 'build-os-artifact.py', 'lab_profile.py'):
            path = lab / name
            self.assertFalse(any(path == dep or dep in path.parents for dep in dependencies), name)
        # custom_build evaluates ignore patterns relative to each dependency.
        self.assertIn('tests/', self.build.call_args.kwargs['ignore'])

    def test_unsupported_context_or_emulated_target_fails_before_building(self):
        for architecture, context in [('x86_64', 'kind-lab'), ('aarch64', 'production')]:
            with self.subTest(architecture=architecture, context=context), self.assertRaises(ValueError):
                self.evaluate({'values': '/lab/one.json'}, architecture=architecture, context=context)
            self.build.assert_not_called()
            self.deploy.assert_not_called()

    def test_pair_shares_build_and_preserves_direct_helm_values(self):
        self.evaluate({'targets': '/lab/pair.json', 'namespace': 'chosen'},
                      {'two': '/lab/two.json', 'one': 'one.json'})
        self.build.assert_called_once()
        self.settings.assert_called_once_with(k8s_upsert_timeout_secs=360, max_parallel_updates=1)
        self.assertEqual([call.args[0] for call in self.deploy.call_args_list], ['one', 'two'])
        for call, name in zip(self.deploy.call_args_list, ['one', 'two']):
            self.assertEqual(call.kwargs['namespace'], 'chosen')
            self.assertEqual(call.kwargs['flags'][:2], ['--values', '/lab/' + name + '.json'])
            self.assertEqual(call.kwargs['image_deps'], ['nico-bmc-target'])

    def test_distinct_repositories_keep_their_release_image_binding(self):
        self.values['/lab/two.json']['image'] = {'repository': 'second-target'}
        self.evaluate({'targets': '/lab/pair.json'})
        self.assertEqual([call.args[0] for call in self.build.call_args_list], ['nico-bmc-target', 'second-target'])
        self.assertEqual([call.kwargs['image_deps'] for call in self.deploy.call_args_list],
                         [['nico-bmc-target'], ['second-target']])

    def test_single_target_defaults(self):
        self.evaluate({'values': '/lab/one.json'})
        self.deploy.assert_called_once()
        self.assertEqual(self.deploy.call_args.kwargs['release_name'], 'target-one')
        self.assertEqual(self.deploy.call_args.kwargs['namespace'], 'bmc-lab')

    def test_invalid_pair_stops_before_any_build_or_deploy(self):
        cases = [
            ('mixed', {'targets': '/lab/pair.json', 'values': '/lab/one.json'}, None, None),
            ('mixed-release', {'targets': '/lab/pair.json', 'release': 'one'}, None, None),
            ('bridge', {'targets': '/lab/pair.json'}, None, ('network', {})),
            ('bridge6', {'targets': '/lab/pair.json'}, None, ('network', {'bridgeAddress': '10.84.41.1/24', 'bridgeAddressV6': 'fd42:8124:40::1/64'})),
            ('arity', {'targets': '/lab/pair.json'}, {'one': 'one.json'}, None),
            ('path', {'targets': '/lab/pair.json'}, {'one': 42, 'two': 'two.json'}, None),
            ('release', {'targets': '/lab/pair.json'}, {'1one': 'one.json', 'two': 'two.json'}, None),
            ('release-length', {'targets': '/lab/pair.json'}, {'a' * 54: 'one.json', 'two': 'two.json'}, None),
            ('index-default', {'targets': '/lab/pair.json'}, None, ('target', {})),
            ('port-default', {'targets': '/lab/pair.json'}, None, ('service', {'clusterIP': 'fd00::11'})),
            ('required', {'targets': '/lab/pair.json'}, None, ('storage', {})),
            ('architecture', {'targets': '/lab/pair.json'}, None, ('architecture', 'amd64')),
            ('claim', {'targets': '/lab/pair.json'}, None, ('storage', {'existingClaim': 'one-data'})),
            ('index', {'targets': '/lab/pair.json'}, None, ('target', {'instanceIndex': 1})),
            ('bmc', {'targets': '/lab/pair.json'}, None, ('service', {'clusterIP': 'fd00::10', 'targetPort': 2444})),
            ('port', {'targets': '/lab/pair.json'}, None, ('service', {'clusterIP': 'fd00::11', 'targetPort': 2443})),
        ]
        original = copy.deepcopy(self.values)
        for name, options, plan, change in cases:
            with self.subTest(name=name):
                self.values = copy.deepcopy(original)
                if change:
                    self.values['/lab/two.json'][change[0]] = change[1]
                with self.assertRaises(ValueError):
                    self.evaluate(options, plan)
                self.build.assert_not_called()
                self.deploy.assert_not_called()
