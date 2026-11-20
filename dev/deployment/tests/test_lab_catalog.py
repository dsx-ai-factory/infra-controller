# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
from deployment_test_support import load_script
import contextlib
import copy
import io
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1] / 'lab/acceptance'
CAT = load_script('lab_catalog', ROOT / 'catalog.py')
OS_ID = '81240036-0000-4000-8000-000000000001'
VPC_ID = '81240036-0000-4000-8000-000000000002'


class CatalogTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.directory = Path(temporary.name)
        self.state = dict(version=1, context='kind-test', namespace='nico-system', cluster='cluster', owner='lab-test',
                          os_id=OS_ID, artifact={'sha256': '0' * 64, 'architecture': 'arm64'}, url='http://pxe/image')
        self.image = {'attributes': dict(id={'value': OS_ID}, source_url='http://pxe/image', digest='0' * 64,
                                        tenant_organization_id='lab-test', name='lab-test-os', description='lab-test',
                                        boot_disk='/dev/vda', create_volume=False), 'status': CAT.IMAGE_READY}
        self.vpc = dict(id=VPC_ID, metadata=dict(name='lab-test-vpc', description='lab-test'), deleted=None,
                        config=dict(tenant_organization_id='lab-test', network_virtualization_type=CAT.FLAT, slaac_enabled=False))

    def test_new_catalog_then_resume_reuses_exact_resources_without_repeating_writes(self):
        lab = CAT.Catalog(self.directory, self.state)
        lab.verify_cluster = Mock()
        images, vpcs, writes = [], [], []
        def admin(*args, read=False, operation=None):
            if read:
                return images if args[0] == 'os-image' else {'vpcs': vpcs}
            saved = json.loads((self.directory / 'state.json').read_text())
            self.assertTrue(saved[('os' if args[0] == 'os-image' else 'vpc') + '_intent'])
            writes.append(args)
            (images if args[0] == 'os-image' else vpcs).append(copy.deepcopy(self.image if args[0] == 'os-image' else self.vpc))
        lab.admin = Mock(side_effect=admin)
        with contextlib.redirect_stdout(io.StringIO()):
            lab.apply()
            lab.apply()
        self.assertEqual(len(writes), 2)
        self.assertEqual(self.state['vpc_id'], VPC_ID)
        self.assertTrue(self.state['complete'])
        self.assertIn('--boot-disk', writes[0])
        self.assertIn('flat', writes[1])

    def test_lost_vpc_response_is_recovered_only_when_unique_result_is_visible(self):
        for visible in (False, True):
            with self.subTest(visible=visible):
                lab = CAT.Catalog(self.directory, copy.deepcopy(self.state))
                resource = Mock(return_value=None)
                lab.admin = Mock(side_effect=RuntimeError('lost response'))
                with self.assertRaisesRegex(RuntimeError, 'lost response'):
                    lab.ensure('vpc', resource, ['vpc', 'create'])
                self.assertTrue(json.loads((self.directory / 'state.json').read_text())['vpc_intent'])
                lab.admin.reset_mock()
                resource.return_value = VPC_ID if visible else None
                if visible:
                    self.assertEqual(lab.ensure('vpc', resource, []), VPC_ID)
                else:
                    with self.assertRaisesRegex(ValueError, 'unknown outcome'):
                        lab.ensure('vpc', resource, [])
                lab.admin.assert_not_called()

    def test_unowned_duplicate_or_changed_vpc_cannot_be_adopted(self):
        lab = CAT.Catalog(self.directory, dict(self.state, vpc_id=VPC_ID))
        changed = copy.deepcopy(self.vpc)
        changed['config']['network_security_group_id'] = 'changed'
        for values in ([self.vpc, self.vpc], [dict(self.vpc, id=OS_ID)], [changed]):
            with self.subTest(values=values):
                lab.admin = Mock(return_value={'vpcs': values})
                with self.assertRaises(ValueError): lab.find_vpc()
        with self.assertRaisesRegex(ValueError, 'without this journal'):
            lab.ensure('vpc', Mock(return_value=VPC_ID), [])

    def test_changed_or_disabled_os_image_is_not_reused(self):
        lab = CAT.Catalog(self.directory, self.state)
        for changes in ({'digest': '1' * 64}, {'boot_disk': '/dev/vdb'}, {'auth_token': 'unexpected'}):
            image = copy.deepcopy(self.image)
            image['attributes'].update(changes)
            lab.admin = Mock(return_value=[image])
            with self.subTest(changes=changes), self.assertRaises(ValueError): lab.find_os()
        lab.admin = Mock(return_value=[dict(self.image, status=2)])
        with self.assertRaisesRegex(ValueError, 'not Ready'): lab.find_os()

    def test_changed_cluster_or_architecture_blocks_api_mutations(self):
        for uid, architecture in [('different', 'arm64'), ('cluster', 'amd64')]:
            lab = CAT.Catalog(self.directory, self.state)
            lab.admin = Mock()
            outputs = [json.dumps({'metadata': {'uid': uid}}), json.dumps({'items': [{'status': {'nodeInfo': {'architecture': architecture}}}]})]
            with self.subTest(uid=uid, architecture=architecture), patch('core_journal.run', side_effect=outputs), self.assertRaises(ValueError):
                lab.apply()
            lab.admin.assert_not_called()

    def test_admin_wrapper_preserves_global_arguments_and_creation_logs(self):
        lab = CAT.Catalog(self.directory, self.state)
        with patch('core_journal.run', return_value='[]') as command:
            self.assertEqual(lab.admin('os-image', 'show', read=True), [])
            self.assertEqual(command.call_args.args[0], ['bash', str(ROOT.parents[1] / 'core-admin.sh'), '-f', 'json', 'os-image', 'show'])
            lab.admin('vpc', 'create', '--org-id', 'lab-test', operation='vpc-create')
            self.assertEqual(command.call_args.args[0], ['bash', str(ROOT.parents[1] / 'core-admin.sh'), '--cloud-unsafe-op',
                             'Initialize owned native lab resources', 'vpc', 'create', '--org-id', 'lab-test'])
            self.assertEqual(command.call_args.kwargs['log_path'], self.directory / 'vpc-create.log')
            self.assertEqual(command.call_args.kwargs['env']['LOCAL_DEV_KUBE_CONTEXT'], 'kind-test')

    def test_public_resume_options_and_non_kind_context_fail_before_journal_creation(self):
        cases = [['--context', 'production', '--artifact-manifest', '/missing', '--pxe-origin', 'http://pxe']]
        cases += [['--resume', flag, value] for flag, value in [('--context', 'kind-test'), ('--namespace', 'other'), ('--core-namespace', 'other'),
                                                              ('--artifact-manifest', '/missing'), ('--pxe-origin', 'http://pxe')]]
        for options in cases:
            destination = self.directory / 'not-created'
            with self.subTest(options=options), patch.object(sys, 'argv',
                    ['catalog.py', '--state-dir', str(destination), *options]), contextlib.redirect_stderr(io.StringIO()), self.assertRaises(SystemExit):
                CAT.main()
            self.assertFalse(destination.exists())

    def test_core_namespace_and_compatibility_alias_reach_the_journal(self):
        for option in ('--core-namespace', '--namespace'):
            with self.subTest(option=option):
                destination = self.directory / option.removeprefix('--')
                manifest = self.directory / 'manifest.json'
                manifest.write_text('{}')
                args = ['catalog.py', '--state-dir', str(destination), '--context', 'kind-test',
                        '--artifact-manifest', str(manifest), '--pxe-origin', 'http://pxe', option, 'other-core']
                with patch.object(sys, 'argv', args), patch.object(CAT, 'artifact', return_value={'pxe_path': '/image'}), \
                        patch.object(CAT, 'run', return_value='{"metadata":{"uid":"cluster"}}'), \
                        patch.object(CAT.Catalog, 'verify_cluster'), patch.object(CAT.Catalog, 'apply'):
                    CAT.main()
                self.assertEqual(json.loads((destination / 'state.json').read_text())['namespace'], 'other-core')

    def test_cluster_preflight_failure_does_not_consume_journal_path(self):
        destination = self.directory / 'not-created'
        manifest = self.directory / 'manifest.json'
        manifest.write_text('{}')
        args = ['catalog.py', '--state-dir', str(destination), '--context', 'kind-test',
                '--artifact-manifest', str(manifest), '--pxe-origin', 'http://pxe']
        with patch.object(sys, 'argv', args), patch.object(CAT, 'artifact', return_value={'pxe_path': '/image'}), \
                patch.object(CAT, 'run', return_value='{"metadata":{"uid":"cluster"}}'), \
                patch.object(CAT.Catalog, 'verify_cluster', side_effect=ValueError('wrong architecture')), \
                self.assertRaisesRegex(ValueError, 'wrong architecture'):
            CAT.main()
        self.assertFalse(destination.exists())

    def test_manifest_preserves_verified_image_identity(self):
        manifest = dict(architecture='arm64', artifact_directory='aarch64', pxe_path='/public/blobs/internal/aarch64/ubuntu.img',
                        sha256='0' * 64, image_id='output', boot_artifact_image_id='parent')
        self.assertEqual(CAT.artifact(manifest), manifest)
        for change in ({'pxe_path': '/other'}, {'architecture': 'amd64'}, {'sha256': 'not-a-digest'}):
            with self.subTest(change=change), self.assertRaises(ValueError): CAT.artifact(dict(manifest, **change))
    def test_origin_rejects_credentials_paths_and_invalid_ports(self):
        for value in ('https://user:password@host', 'http://host/path', 'http://host?token=x', 'http://pxe?', 'http://pxe/#', 'http://@pxe', 'http://pxe:abc', 'http://pxe:99999', 'http://pxe:0', 'http://pxe\n'):
            with self.subTest(value=value), self.assertRaises(CAT.argparse.ArgumentTypeError): CAT.origin(value)
        self.assertEqual(CAT.origin('http://[fd00::1]:8080/'), 'http://[fd00::1]:8080')


if __name__ == '__main__':
    unittest.main()
