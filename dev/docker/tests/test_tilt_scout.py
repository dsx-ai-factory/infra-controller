# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import yaml

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib

ROOT = Path(__file__).resolve().parents[3]
from tilt_test_support import TiltResult, baseline

TILT = ROOT / 'dev/deployment/tilt/Tiltfile'


class ScoutWiringTest(unittest.TestCase):
    def profile(self, bmc, arch='aarch64', ipv6_endpoints=False, ipv6_boot=False):
        result = TiltResult({'bmc-lab': bmc, 'persistent-vault': bmc,
                             'boot-artifact-image': 'native-artifacts:test' if bmc else '',
                             'dhcpv6': ipv6_endpoints, 'ipv6-endpoints': ipv6_endpoints, 'ipv6-boot': ipv6_boot},
                            architecture=arch)
        values = result.env['all_values']
        calls = []
        if bmc:
            calls.append(dict(result.resources['nico-pxe'], values={'global': values['global'], **values['nico-pxe']}))
        return calls, values

    def test_real_scout_profile_renders_firmware_and_metadata_endpoints(self):
        for arch in ('aarch64', 'x86_64'):
            with self.subTest(arch=arch):
                calls, values = self.profile(True, arch)
                pxe = calls[0]
                self.assertEqual(pxe['resource_deps'], ['nico-api'])
                self.assertEqual(pxe['image_deps'], ['nico-pxe'])
                settings = pxe['values']
                settings['global'] = values['global']
                result = subprocess.run(['helm', 'template', 'nico-pxe', str(ROOT / 'helm/charts/nico-pxe'),
                                         '-n', 'nico-system', '-f', '-'], input=yaml.safe_dump(settings),
                                        capture_output=True, text=True, check=True, timeout=30)
                resources = list(yaml.safe_load_all(result.stdout))
                service = next(x for x in resources if x['kind'] == 'Service' and x['metadata']['name'] == 'nico-pxe')
                dhcp = values['nico-dhcp']['config']['kea']['hookParameters']
                self.assertEqual(service['spec']['clusterIP'], dhcp['provisioningServer'])
                self.assertEqual([p['port'] for p in service['spec']['ports']], [8080, 80])
                config = next(x for x in resources if x['kind'] == 'ConfigMap')['data']
                self.assertEqual(config['CARBIDE_API_URL'], 'https://nico-api.nico-system.svc.cluster.local:1079')
                deployment = next(x for x in resources if x['kind'] == 'Deployment')
                init = deployment['spec']['template']['spec']['initContainers'][0]
                self.assertEqual(init['image'], 'native-artifacts:test')
                self.assertIn('/' + arch, init['command'][-1])
                core = values['nico-api']
                self.assertEqual(core['extraEnv'], [])
                auth = tomllib.loads(core['siteConfig']['nicoApiSiteConfig'])['auth']
                self.assertFalse(auth['permissive_mode'])
                self.assertFalse(auth['allow_machineatron_scout_stream'])
                self.assertEqual(init['imagePullPolicy'], 'IfNotPresent')

    def test_direct_helm_renderer_uses_the_same_profile_as_tilt(self):
        with tempfile.TemporaryDirectory() as directory:
            command = [sys.executable, str(ROOT / 'dev/deployment/lab/lab_profile.py'),
                       '--architecture', 'x86_64', '--boot-artifact-image', 'native-artifacts:test', '--dhcpv6']
            images = {name: 'registry.test:5000/' + name + ':test' for name in
                      ('nico-api', 'nico-bmc-proxy', 'nico-dhcp', 'nico-pxe')}
            for name, image in images.items():
                command += ['--image', name + '=' + image]
            subprocess.run(command + ['--output', directory], check=True, capture_output=True, text=True)
            tilt = TiltResult({'bmc-lab': True, 'persistent-vault': True, 'boot-artifact-image': 'native-artifacts:test',
                               'dhcpv6': True}, architecture='x86_64')
            values = tilt.env['all_values']
            for name in images:
                actual = json.loads((Path(directory) / (name + '.json')).read_text())
                expected = {'global': values['global'], **values[name]}
                image = actual['image'] if name in ('nico-bmc-proxy', 'nico-pxe') else actual['global']['image']
                self.assertEqual(image['repository'] + ':' + image['tag'], images[name])
                # Image identity belongs to the frontend; all deployment settings agree.
                if name in ('nico-bmc-proxy', 'nico-pxe'):
                    actual.pop('image')
                    expected.pop('image', None)
                else:
                    actual['global']['image'] = values['global']['image']
                self.assertEqual(actual, expected)
            self.assertEqual(json.loads((Path(directory) / 'postgres.json').read_text())['cluster']['roles'], [])
            dependencies = json.loads((Path(directory) / 'dependencies.json').read_text())
            for name, resource in [('cert-manager', 'cert-manager'), ('cnpg-operator', 'cnpg-operator'),
                                   ('vault', 'vault'), ('postgres', 'nico-postgres')]:
                chart = dependencies['charts'][name]
                self.assertEqual(tilt.resources[resource]['chart'], chart['chart'])
                self.assertIn('--version=' + chart['version'], tilt.resources[resource]['flags'])
            for name, url in dependencies['repositories'].items():
                if name in tilt.repositories:
                    self.assertEqual(tilt.repositories[name]['url'], url)
            self.assertEqual((Path(directory) / 'images.txt').read_text().splitlines(),
                             list(images.values()) + ['native-artifacts:test'])
            self.assertIn(str(ROOT / 'dev/deployment/lab/lab_profile.py'), tilt.watches)
            self.assertIn(str(ROOT / 'dev/deployment/stack/bmc-lab.toml'), tilt.watches)

    def test_renderer_forwards_namespace_and_rejects_incomplete_image_plan_before_writing(self):
        command = [sys.executable, str(ROOT / 'dev/deployment/lab/lab_profile.py'),
                   '--architecture', 'aarch64', '--namespace', 'chosen', '--boot-artifact-image', 'test:latest']
        values = json.loads(subprocess.check_output(command, text=True))
        self.assertEqual(values['nico-pxe']['config']['extraEnvData'], {
            'CARBIDE_API_URL': 'https://nico-api.chosen.svc.cluster.local:1079',
            'CARBIDE_PXE_URL': 'http://nico-pxe.chosen.svc.cluster.local'})
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(command + ['--output', directory, '--image', 'nico-api=api:test'], capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('requires one --image', result.stderr)
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_default_mat_configuration_is_preserved(self):
        calls, values = self.profile(False)
        self.assertEqual(calls, [])
        self.assertEqual(values['nico-api']['extraEnv'][0]['value'], '1')
        self.assertEqual(values['nico-dhcp']['config']['kea']['hookParameters']['provisioningServer'], '127.0.0.1')

    def test_incomplete_bmc_mode_rejected_before_deployment(self):
        for bmc, vault, artifact in [(True, False, 'image'), (True, True, ''), (False, True, 'image')]:
            with self.subTest(bmc=bmc, vault=vault, artifact=artifact), self.assertRaises(ValueError):
                TiltResult({'bmc-lab': bmc, 'persistent-vault': vault, 'boot-artifact-image': artifact})

    def test_dhcpv6_chart_enablement_requires_bmc_and_a_dual_stack_node(self):
        for bmc, cidrs, error, initial in [(False, ['10.244.0.0/24', 'fd00:10::/64'], '--bmc-lab', {}),
                                          (True, ['10.244.0.0/24'], 'dual-stack', {}),
                                          (True, ['10.244.0.0/24', 'fd00:10::/64'], '', {}),
                                          (True, ['fd00:10::/64'], '', {'dhcp': None})]:
            values = baseline()
            values['nico-dhcp'].update(initial)
            with self.subTest(bmc=bmc, cidrs=cidrs):
                options = {'dhcpv6': True, 'bmc-lab': bmc, 'persistent-vault': True, 'boot-artifact-image': 'test:latest'}
                if error:
                    with self.assertRaisesRegex(ValueError, error):
                        TiltResult(options, values=values, cidrs=cidrs)
                else:
                    result = TiltResult(options, values=values, cidrs=cidrs)
                    self.assertEqual(result.env['all_values']['nico-dhcp']['dhcp'],
                                     {'v6Enabled': True, 'v6ServerIdentifier': '020000000006'})

    def test_dhcpv6_enablement_preserves_configured_resolver_and_lease_timing(self):
        values = baseline()
        values['nico-dhcp']['dhcp'] = {
            'v6Enabled': False, 'v6DnsServers': 'fd00:84::1',
            'v6ServerIdentifier': '020000001234',
            'v6LeaseTimers': {'preferredLifetime': 120, 'validLifetime': 180,
                              'renewTimer': 30, 'rebindTimer': 60},
        }
        values = TiltResult({'dhcpv6': True, 'bmc-lab': True, 'persistent-vault': True,
                             'boot-artifact-image': 'test:latest'}, values=values).env['all_values']
        self.assertEqual(values['nico-dhcp']['dhcp'], {
            'v6Enabled': True, 'v6DnsServers': 'fd00:84::1',
            'v6ServerIdentifier': '020000001234',
            'v6LeaseTimers': {'preferredLifetime': 120, 'validLifetime': 180,
                              'renewTimer': 30, 'rebindTimer': 60},
        })

    def test_ipv6_endpoints_render_existing_service_contracts_and_enable_dns(self):
        result = TiltResult({'bmc-lab': True, 'persistent-vault': True, 'boot-artifact-image': 'test:latest',
                             'dhcpv6': True, 'ipv6-endpoints': True})
        values = result.env['all_values']
        self.assertIn('nico-lab-dns', result.enabled)
        self.assertEqual(result.resources['nico-lab-dns']['resource_deps'], ['nico-api', 'nico-pxe'])
        pxe_calls, _ = self.profile(True, ipv6_endpoints=True)
        env_values = pxe_calls[0]['values']['config']['extraEnvData']
        self.assertEqual(env_values['CARBIDE_API_URL'], 'https://nico-api.nico-system.svc.cluster.local:1079')
        self.assertEqual(env_values['CARBIDE_PXE_URL'], 'http://nico-pxe.nico-system.svc.cluster.local')
        boot_calls, _ = self.profile(True, ipv6_endpoints=True, ipv6_boot=True)
        for component, settings, port in [('nico-api', {'global': values['global'], **values['nico-api']}, 1079),
                                           ('nico-pxe', {**pxe_calls[0]['values'], 'global': values['global']}, 80),
                                           ('nico-pxe', {**boot_calls[0]['values'], 'global': values['global']}, 8080)]:
            with self.subTest(component=component, port=port):
                settings.pop('global_values', None)
                result = subprocess.run(
                    ['helm', 'template', component, str(ROOT / 'helm/charts' / component),
                     '--namespace', 'nico-system', '--values', '-', '--show-only', 'templates/external-service.yaml'],
                    input=yaml.safe_dump(settings), capture_output=True, text=True, timeout=30)
                self.assertEqual(result.returncode, 0, result.stderr)
                resources = list(yaml.safe_load_all(result.stdout))
                self.assertEqual(len(resources), 1)
                service = resources[0]['spec']
                self.assertEqual(service['type'], 'ClusterIP')
                self.assertEqual(service['ipFamilies'], ['IPv6'])
                self.assertEqual(service['ports'][0]['port'], port)
                self.assertNotIn('externalTrafficPolicy', service)

    def test_artifact_init_checks_profile_before_copying_payloads(self):
        cases = [
            (False, None, None, True),
            (False, 'default', 'default', True),
            (True, None, None, False),
            (True, 'ipv6', 'ipv6', True),
            (True, 'ipv6', 'default', False),
            (False, 'ipv6', 'ipv6', False),
        ]
        for ipv6, scout, imager, success in cases:
            with self.subTest(ipv6=ipv6, scout=scout, imager=imager), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                payload = root / 'aarch64'
                payload.mkdir()
                destination = root / 'destination'
                destination.mkdir()
                for name, profile in [('scout', scout), ('qcow-imager', imager)]:
                    (payload / (name + '.efi')).write_text('fixture')
                    if profile is not None:
                        (payload / (name + '.network-profile')).write_text(profile + '\n')
                calls, _ = self.profile(True, ipv6_endpoints=ipv6, ipv6_boot=ipv6)
                command = calls[0]['values']['bootArtifactContainers'][0]['command']
                command[-1] = command[-1].replace('/aarch64', str(payload)).replace('/forge-boot-artifacts/blobs/internal/', str(destination) + '/')
                result = subprocess.run(command, capture_output=True, text=True, timeout=10)
                self.assertEqual(result.returncode == 0, success, result.stderr)
                self.assertEqual((destination / 'aarch64/scout.efi').exists(), success)
                if not success:
                    self.assertIn('boot artifact network profile mismatch', result.stderr)

    def test_packaging_retains_both_boot_profile_markers(self):
        rules = (ROOT / 'dev/deployment/lab/Dockerfile.boot-artifacts.dockerignore').read_text().splitlines()
        for arch in ('aarch64', 'x86_64'):
            self.assertLess(rules.index('!' + arch), rules.index(arch + '/**'))
            for name in ('scout', 'qcow-imager'):
                with self.subTest(architecture=arch, payload=name):
                    self.assertIn('!' + arch + '/' + name + '.network-profile', rules)

    def test_ipv6_boot_is_explicit_and_requires_companion_endpoints(self):
        calls, _ = self.profile(True, ipv6_endpoints=True, ipv6_boot=True)
        values = calls[0]['values']
        self.assertEqual(values['config']['extraEnvData']['CARBIDE_API_URL'], 'https://carbide-api.forge:1079')
        self.assertEqual(values['config']['extraEnvData']['CARBIDE_PXE_URL'], 'http://carbide-pxe.forge:8080')
        with self.assertRaisesRegex(ValueError, 'requires --ipv6-endpoints'):
            TiltResult({'ipv6-boot': True})

    def test_ipv6_endpoints_require_managed_dhcpv6(self):
        with self.assertRaisesRegex(ValueError, 'requires --dhcpv6'):
            TiltResult({'ipv6-endpoints': True})

    def test_ipv6_boot_exports_live_services_before_dhcp_without_flag_overrides(self):
        for enabled in (False, True):
            with self.subTest(ipv6_boot=enabled):
                values = baseline()
                values['nico-dhcp']['dhcp'] = {'v6DnsServers': 'old', 'v6ProvisioningServer': 'old'}
                result = TiltResult({'bmc-lab': True, 'persistent-vault': True, 'boot-artifact-image': 'test:latest',
                                     'dhcpv6': True, 'ipv6-endpoints': enabled, 'ipv6-boot': enabled}, values=values)
                dhcp = result.resources['nico-dhcp']
                if not enabled:
                    self.assertEqual(dhcp['resource_deps'], ['nico-api'])
                    self.assertNotIn('--values', dhcp['flags'])
                    self.assertNotIn('ipv6-boot-endpoints', result.resources)
                    continue
                self.assertEqual(dhcp['resource_deps'], ['nico-api', 'ipv6-boot-endpoints'])
                self.assertNotIn('v6DnsServers', result.env['all_values']['nico-dhcp']['dhcp'])
                self.assertNotIn('v6ProvisioningServer', result.env['all_values']['nico-dhcp']['dhcp'])
                endpoints = result.resources['ipv6-boot-endpoints']
                self.assertEqual(endpoints['resource_deps'], ['nico-lab-dns', 'nico-pxe'])
                self.assertEqual(endpoints['command'][-1], dhcp['flags'][dhcp['flags'].index('--values') + 1])

    def test_bmc_mode_does_not_enable_mat_or_its_readiness_resource(self):
        for bmc in (False, True):
            with self.subTest(bmc=bmc):
                result = TiltResult({'bmc-lab': bmc, 'persistent-vault': bmc,
                                     'boot-artifact-image': 'test:latest' if bmc else ''})
                names = result.enabled
                self.assertEqual('nico-pxe' in names, bmc)
                self.assertIn('local-site-mode', names)
                for name in ('nico-machine-a-tron', 'nico-ssh-console-rs', 'stack-ready'):
                    self.assertEqual(name in names, not bmc)

    def test_mode_is_claimed_before_core_starts(self):
        for bmc in (False, True):
            with self.subTest(bmc=bmc):
                result = TiltResult({'bmc-lab': bmc, 'persistent-vault': bmc,
                                     'boot-artifact-image': 'test:latest' if bmc else ''})
                self.assertIn('local-site-mode', result.resources['nico-api']['resource_deps'])
                expected = ['python3', str(ROOT / 'dev/deployment/stack/site-mode.py'), '--context', 'kind-test',
                            '--namespace', 'nico-system', '--mode', 'bmc' if bmc else 'mat']
                self.assertIn(expected + ['check'], result.commands)
                self.assertEqual(result.resources['local-site-mode']['command'], expected + ['claim'])
