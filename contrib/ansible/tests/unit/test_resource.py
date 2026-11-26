# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import sys
import os
import pytest
from unittest.mock import MagicMock, patch

sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..', '..', 'plugins'))

from module_utils.resource import CrudResource, InfoResource, BatchResource, ActionResource, _resolve_path


def make_module(argument_spec=None, **params):
    """Create a mock Ansible module with the given params."""
    defaults = {
        'api_url': 'https://api.example.com',
        'api_token': 'test-token',
        'org': 'test-org',
        'api_path_prefix': None,
        'state': 'present',
        'id': None,
        'name': None,
        'wait': False,
        'wait_timeout': 600,
    }
    defaults.update(params)

    module = MagicMock()
    module.params = defaults
    module.check_mode = False
    module.argument_spec = argument_spec if argument_spec is not None else {}
    return module


def _make_config(**overrides):
    """Return a RESOURCE_CONFIG dict with sensible defaults, overridable per-test."""
    cfg = {
        'resource_path': '/v2/org/{org}/nico/resource',
        'resource_item_path': '/v2/org/{org}/nico/resource/{resourceId}',
        'id_param': 'resourceId',
        'name_field': 'name',
        'create_schema_fields': [],
        'update_schema_fields': [],
        'scope_fields': [],
        'ready_statuses': ['Ready'],
        'error_statuses': ['Error'],
        'no_create': False,
        'delete_body_fields': [],
        'version_field': None,
        'update_method': 'PATCH',
        'field_map': {},
        'required_create_fields': [],
        'opaque_fields': [],
    }
    cfg.update(overrides)
    return cfg


VPC_CONFIG = _make_config(
    resource_path='/v2/org/{org}/nico/vpc',
    resource_item_path='/v2/org/{org}/nico/vpc/{vpcId}',
    id_param='vpcId',
    create_schema_fields=['name', 'description', 'site_id'],
    update_schema_fields=['name', 'description'],
    scope_fields=['site_id'],
)


class TestCrudResourceFindExisting:
    @patch('module_utils.resource.NicoClient')
    def test_find_by_id(self, MockClient):
        module = make_module(id='vpc-123')
        client = MockClient.return_value
        client.get.return_value = {'id': 'vpc-123', 'name': 'test-vpc', 'status': 'Ready'}

        resource = CrudResource(module, VPC_CONFIG)
        result = resource._find_existing()

        assert result is not None
        assert result['id'] == 'vpc-123'
        get_path = client.get.call_args[0][0]
        assert 'vpc-123' in get_path
        assert '{vpcId}' not in get_path

    @patch('module_utils.resource.NicoClient')
    def test_find_by_name(self, MockClient):
        module = make_module(name='my-vpc', site_id='site-1')
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'other-vpc', 'siteId': 'site-1'},
            {'id': 'vpc-2', 'name': 'my-vpc', 'siteId': 'site-1'},
        ]

        resource = CrudResource(module, VPC_CONFIG)
        result = resource._find_existing()

        assert result is not None
        assert result['id'] == 'vpc-2'

    @patch('module_utils.resource.NicoClient')
    def test_find_by_name_not_found(self, MockClient):
        module = make_module(name='nonexistent', site_id='site-1')
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'other-vpc', 'siteId': 'site-1'},
        ]

        resource = CrudResource(module, VPC_CONFIG)
        result = resource._find_existing()

        assert result is None

    @patch('module_utils.resource.NicoClient')
    def test_find_ambiguous_fails(self, MockClient):
        module = make_module(name='dup-vpc')
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'dup-vpc'},
            {'id': 'vpc-2', 'name': 'dup-vpc'},
        ]

        resource = CrudResource(module, VPC_CONFIG)
        resource._find_existing()

        module.fail_json.assert_called_once()
        assert 'Found 2 resources' in module.fail_json.call_args[1]['msg']

    @patch('module_utils.resource.NicoClient')
    def test_find_by_name_passes_scope_keys_as_allow_missing(self, MockClient):
        """Scope filters may be absent from response schemas; list_all must
        tolerate their absence in proxy mode instead of dropping matches."""
        module = make_module(name='my-vpc', site_id='site-1')
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-2', 'name': 'my-vpc', 'siteId': 'site-1'},
        ]

        resource = CrudResource(module, VPC_CONFIG)
        resource._find_existing()

        call_kwargs = client.list_all.call_args[1]
        assert call_kwargs.get('allow_missing') == {'siteId'}
        assert call_kwargs.get('params') == {'siteId': 'site-1'}


class TestCrudResourcePresent:
    @patch('module_utils.resource.NicoClient')
    def test_create_new(self, MockClient):
        module = make_module(
            name='new-vpc', description='A VPC', site_id='site-1',
        )
        client = MockClient.return_value
        client.list_all.return_value = []
        client.create.return_value = {'id': 'vpc-new', 'name': 'new-vpc', 'status': 'Ready'}

        resource = CrudResource(module, VPC_CONFIG)
        resource.run()

        client.create.assert_called_once()
        module.exit_json.assert_called_once()
        call_kwargs = module.exit_json.call_args[1]
        assert call_kwargs['changed'] is True
        assert call_kwargs['resource']['name'] == 'new-vpc'
        assert call_kwargs['diff']['before'] == {}
        assert call_kwargs['diff']['after']['name'] == 'new-vpc'

    @patch('module_utils.resource.NicoClient')
    def test_update_existing_with_changes(self, MockClient):
        module = make_module(
            name='my-vpc', description='Updated desc',
        )
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'my-vpc', 'description': 'Old desc', 'status': 'Ready'},
        ]
        client.update.return_value = {'id': 'vpc-1', 'name': 'my-vpc', 'description': 'Updated desc', 'status': 'Ready'}

        resource = CrudResource(module, VPC_CONFIG)
        resource.run()

        client.update.assert_called_once()
        module.exit_json.assert_called_once()
        kw = module.exit_json.call_args[1]
        assert kw['changed'] is True
        assert kw['diff']['before']['description'] == 'Old desc'
        assert kw['diff']['after']['description'] == 'Updated desc'

    @patch('module_utils.resource.NicoClient')
    def test_no_change(self, MockClient):
        module = make_module(
            name='my-vpc', description='Same desc',
        )
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'my-vpc', 'description': 'Same desc', 'status': 'Ready'},
        ]

        resource = CrudResource(module, VPC_CONFIG)
        resource.run()

        client.update.assert_not_called()
        module.exit_json.assert_called_once()
        assert module.exit_json.call_args[1]['changed'] is False

    @patch('module_utils.resource.NicoClient')
    def test_check_mode_create(self, MockClient):
        module = make_module(name='new-vpc', site_id='site-1')
        module.check_mode = True
        client = MockClient.return_value
        client.list_all.return_value = []

        resource = CrudResource(module, VPC_CONFIG)
        resource.run()

        client.create.assert_not_called()
        module.exit_json.assert_called_once()
        assert module.exit_json.call_args[1]['changed'] is True


class TestCrudResourceIdempotence:
    """Tests for correct idempotence behavior — nested structures, labels, etc."""

    INSTANCE_CONFIG = _make_config(
        resource_path='/v2/org/{org}/nico/instance',
        resource_item_path='/v2/org/{org}/nico/instance/{instanceId}',
        id_param='instanceId',
        create_schema_fields=['name', 'vpc_id', 'tenant_id', 'interfaces', 'labels'],
        update_schema_fields=['name', 'description', 'labels', 'interfaces'],
        opaque_fields=['labels'],
    )

    @patch('module_utils.resource.NicoClient')
    def test_no_change_with_nested_interfaces(self, MockClient):
        """When interfaces match, should report changed=False."""
        module = make_module(
            name='my-instance',
            interfaces=[{'subnet_id': 'sub-1', 'is_physical': True}],
        )
        client = MockClient.return_value
        client.list_all.return_value = [{
            'id': 'inst-1',
            'name': 'my-instance',
            'interfaces': [{'subnetId': 'sub-1', 'isPhysical': True}],
            'status': 'Ready',
        }]

        resource = CrudResource(module, self.INSTANCE_CONFIG)
        resource.run()

        client.update.assert_not_called()
        assert module.exit_json.call_args[1]['changed'] is False

    @patch('module_utils.resource.NicoClient')
    def test_change_detected_in_nested_interfaces(self, MockClient):
        """When interfaces differ, should detect the change and update."""
        module = make_module(
            name='my-instance',
            interfaces=[{'subnet_id': 'sub-2', 'is_physical': False}],
        )
        client = MockClient.return_value
        client.list_all.return_value = [{
            'id': 'inst-1',
            'name': 'my-instance',
            'interfaces': [{'subnetId': 'sub-1', 'isPhysical': True}],
            'status': 'Ready',
        }]
        client.update.return_value = {
            'id': 'inst-1',
            'name': 'my-instance',
            'interfaces': [{'subnetId': 'sub-2', 'isPhysical': False}],
            'status': 'Ready',
        }

        resource = CrudResource(module, self.INSTANCE_CONFIG)
        resource.run()

        client.update.assert_called_once()
        # Verify the payload sent to the API has camelCase keys
        update_payload = client.update.call_args[0][1]
        assert 'interfaces' in update_payload
        assert update_payload['interfaces'][0].get('subnetId') == 'sub-2'
        assert update_payload['interfaces'][0].get('isPhysical') is False

    @patch('module_utils.resource.NicoClient')
    def test_no_change_with_labels(self, MockClient):
        """Labels should compare without key conversion."""
        module = make_module(
            name='my-instance',
            labels={'RackIdentifier': 'GVX11F01C02', 'env': 'prod'},
        )
        client = MockClient.return_value
        client.list_all.return_value = [{
            'id': 'inst-1',
            'name': 'my-instance',
            'labels': {'RackIdentifier': 'GVX11F01C02', 'env': 'prod'},
            'status': 'Ready',
        }]

        resource = CrudResource(module, self.INSTANCE_CONFIG)
        resource.run()

        client.update.assert_not_called()
        assert module.exit_json.call_args[1]['changed'] is False

    @patch('module_utils.resource.NicoClient')
    def test_label_change_detected(self, MockClient):
        """A label value change should trigger an update."""
        module = make_module(
            name='my-instance',
            labels={'env': 'staging'},
        )
        client = MockClient.return_value
        client.list_all.return_value = [{
            'id': 'inst-1',
            'name': 'my-instance',
            'labels': {'env': 'prod'},
            'status': 'Ready',
        }]
        client.update.return_value = {
            'id': 'inst-1', 'name': 'my-instance',
            'labels': {'env': 'staging'}, 'status': 'Ready',
        }

        resource = CrudResource(module, self.INSTANCE_CONFIG)
        resource.run()

        client.update.assert_called_once()
        # Verify labels sent to API have user-defined keys preserved
        update_payload = client.update.call_args[0][1]
        assert update_payload['labels'] == {'env': 'staging'}

    @patch('module_utils.resource.NicoClient')
    def test_create_converts_nested_keys_to_camel(self, MockClient):
        """Create payload should have camelCase keys at all nesting levels."""
        module = make_module(
            name='new-instance',
            vpc_id='vpc-1',
            tenant_id='t-1',
            interfaces=[{'subnet_id': 'sub-1', 'is_physical': True}],
            labels={'env': 'dev'},
        )
        client = MockClient.return_value
        client.list_all.return_value = []
        client.create.return_value = {'id': 'inst-new', 'name': 'new-instance', 'status': 'Ready'}

        resource = CrudResource(module, self.INSTANCE_CONFIG)
        resource.run()

        create_payload = client.create.call_args[0][1]
        assert 'vpcId' in create_payload
        assert 'tenantId' in create_payload
        assert create_payload['interfaces'][0].get('subnetId') == 'sub-1'
        assert create_payload['interfaces'][0].get('isPhysical') is True
        assert create_payload['labels'] == {'env': 'dev'}


class TestCrudResourceAbsent:
    @patch('module_utils.resource.NicoClient')
    def test_create_with_id_fails_when_item_path_has_no_get(self, MockClient):
        """id targets an existing resource; without an item GET it cannot resolve.

        Creating anyway would silently add a new resource on every run
        (measured-boot trusted-profile), so the create branch fails fast.
        """
        config = _make_config(
            resource_path='/v2/org/{org}/nico/measured-boot/trusted-profile',
            resource_item_path='/v2/org/{org}/nico/measured-boot/trusted-profile/{id}',
            id_param='id',
            name_field=None,
            create_schema_fields=['site_id', 'profile_id', 'approval_type'],
            required_create_fields=['site_id', 'profile_id', 'approval_type'],
            has_get_by_id=False,
        )
        module = make_module(state='present', id='approval-1', site_id='site-1',
                             profile_id='profile-1', approval_type='Persist')
        client = MockClient.return_value

        CrudResource(module, config).run()

        module.fail_json.assert_called_once()
        assert 'refusing to create' in module.fail_json.call_args[1]['msg']
        client.create.assert_not_called()
        client.put.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_create_with_id_fails_when_lookup_found_nothing(self, MockClient):
        """With an item GET, a 404 on the id lookup must not become a create.

        Creating would ignore the id and add a new resource on every run.
        """
        config = _make_config(
            resource_path='/v2/org/{org}/nico/instance',
            resource_item_path='/v2/org/{org}/nico/instance/{instanceId}',
            id_param='instanceId',
            name_field=None,
            create_schema_fields=['vpc_id'],
            has_get_by_id=True,
        )
        module = make_module(state='present', id='inst-1', vpc_id='vpc-1')
        client = MockClient.return_value
        client.get.return_value = None  # lookup by id 404s

        CrudResource(module, config).run()

        module.fail_json.assert_called_once()
        assert 'refusing to create' in module.fail_json.call_args[1]['msg']
        client.create.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_create_with_id_proceeds_when_create_accepts_id(self, MockClient):
        """VPC-style creates accept a user-specified UUID: id + not-found creates.

        The created resource carries the supplied id, so retries are
        idempotent — the getting-started example relies on this.
        """
        config = _make_config(
            resource_path='/v2/org/{org}/nico/vpc',
            resource_item_path='/v2/org/{org}/nico/vpc/{vpcId}',
            id_param='vpcId',
            name_field=None,
            create_schema_fields=['id', 'name', 'site_id'],
            required_create_fields=['name', 'site_id'],
            has_get_by_id=True,
        )
        module = make_module(state='present', id='vpc-uuid-1', name='my-vpc',
                             site_id='site-1')
        client = MockClient.return_value
        client.get.return_value = None  # lookup by id 404s
        client.create.return_value = {'id': 'vpc-uuid-1', 'name': 'my-vpc'}

        CrudResource(module, config).run()

        client.create.assert_called_once()
        assert client.create.call_args[0][1]['id'] == 'vpc-uuid-1'
        assert module.exit_json.call_args[1]['changed'] is True

    @patch('module_utils.resource.NicoClient')
    def test_create_without_id_proceeds_when_item_path_has_no_get(self, MockClient):
        """Omitting id still creates: the guard only protects id-targeted runs."""
        config = _make_config(
            resource_path='/v2/org/{org}/nico/measured-boot/trusted-profile',
            resource_item_path='/v2/org/{org}/nico/measured-boot/trusted-profile/{id}',
            id_param='id',
            name_field=None,
            create_schema_fields=['site_id', 'profile_id', 'approval_type'],
            required_create_fields=['site_id', 'profile_id', 'approval_type'],
            has_get_by_id=False,
        )
        module = make_module(state='present', site_id='site-1',
                             profile_id='profile-1', approval_type='Persist')
        client = MockClient.return_value
        client.create.return_value = {'id': 'approval-1'}

        CrudResource(module, config).run()

        client.create.assert_called_once()
        assert module.exit_json.call_args[1]['changed'] is True

    @patch('module_utils.resource.NicoClient')
    def test_update_detects_removal_in_opaque_dict_field(self, MockClient):
        """Opaque dict fields are the complete desired state, compared exactly.

        A subset comparison would treat labels {a: 1} as matching an existing
        {a: 1, b: 2} and never remove b.
        """
        config = _make_config(
            update_schema_fields=['name', 'labels'],
            opaque_fields=['labels'],
        )
        module = make_module(state='present', name='my-vpc', labels={'a': '1'})
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'my-vpc', 'labels': {'a': '1', 'b': '2'},
             'status': 'Ready'},
        ]

        CrudResource(module, config).run()

        client.update.assert_called_once()
        assert client.update.call_args[0][1] == {'labels': {'a': '1'}}

    @patch('module_utils.resource.NicoClient')
    def test_update_no_change_when_opaque_dict_field_matches(self, MockClient):
        config = _make_config(
            update_schema_fields=['name', 'labels'],
            opaque_fields=['labels'],
        )
        module = make_module(state='present', name='my-vpc', labels={'a': '1'})
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'my-vpc', 'labels': {'a': '1'},
             'status': 'Ready'},
        ]

        CrudResource(module, config).run()

        client.update.assert_not_called()
        assert module.exit_json.call_args[1]['changed'] is False

    @patch('module_utils.resource.NicoClient')
    def test_put_update_sends_full_desired_state(self, MockClient):
        """PUT replaces the representation; unchanged fields are preserved.

        A partial PUT could reset omitted fields or be rejected for omitting
        unchanged required ones, so the payload carries the full desired
        state, not just the diff.
        """
        config = _make_config(
            resource_path='/v2/org/{org}/nico/firmware-config/host',
            resource_item_path='',
            id_param='id',
            name_field=None,
            no_create=True,
            create_schema_fields=['site_id', 'vendor', 'model', 'components'],
            update_schema_fields=['site_id', 'vendor', 'model', 'components'],
            update_method='PUT',
            field_map={'site_id': 'siteId'},
        )
        # Existing config has all fields; the user changes only components.
        module = make_module(state='present', site_id='site-1', vendor='Nvidia',
                             model='DGXH100', components=[{'name': 'bmc'}])
        client = MockClient.return_value
        client.get.return_value = {
            'siteId': 'site-1', 'vendor': 'Nvidia', 'model': 'DGXH100',
            'components': [{'name': 'gpu'}], 'status': 'Ready',
        }

        CrudResource(module, config).run()

        client.put.assert_called_once()
        assert client.put.call_args[0][1] == {
            'siteId': 'site-1', 'vendor': 'Nvidia', 'model': 'DGXH100',
            'components': [{'name': 'bmc'}],
        }

    @patch('module_utils.resource.NicoClient')
    def test_patch_update_sends_only_changes(self, MockClient):
        """PATCH carries only the differing fields."""
        config = _make_config(
            update_schema_fields=['name', 'description'],
        )
        module = make_module(state='present', name='my-vpc', description='new-desc')
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'my-vpc', 'description': 'old-desc', 'status': 'Ready'},
        ]

        CrudResource(module, config).run()

        client.update.assert_called_once()
        assert client.update.call_args[0][1] == {'description': 'new-desc'}

    @patch('module_utils.resource.NicoClient')
    def test_delete_existing_without_item_path_targets_collection(self, MockClient):
        """Singletons and name-found collection-only resources delete via the collection path.

        Resolving an empty item path would send DELETE to the bare API URL
        and report changed=True without deleting anything.
        """
        config = _make_config(
            resource_path='/v2/org/{org}/nico/firmware-config/host',
            resource_item_path='',
            id_param='id',
            name_field=None,
            no_create=True,
        )
        module = make_module(state='absent')
        client = MockClient.return_value
        # Singleton lookup through the collection GET.
        client.get.return_value = {'vendor': 'Nvidia', 'model': 'DGXH100'}
        client.delete.return_value = (None, 204)

        CrudResource(module, config).run()

        client.delete.assert_called_once()
        path = client.delete.call_args[0][0]
        assert path.endswith('/firmware-config/host')
        assert path != ''

    @patch('module_utils.resource.NicoClient')
    def test_absent_on_missing_singleton_is_idempotent(self, MockClient):
        """A singleton probed by collection GET reports changed=False when absent.

        The 404 from the probe means the resource does not exist; issuing a
        collection DELETE anyway would report changed=True on every run.
        """
        config = _make_config(
            resource_path='/v2/org/{org}/nico/firmware-config/host',
            resource_item_path='',
            id_param='id',
            name_field=None,
            no_create=True,
        )
        module = make_module(state='absent')
        client = MockClient.return_value
        client.get.return_value = None  # singleton probe 404s

        CrudResource(module, config).run()

        client.delete.assert_not_called()
        assert module.exit_json.call_args[1]['changed'] is False

    @patch('module_utils.resource.NicoClient')
    def test_delete_existing(self, MockClient):
        module = make_module(state='absent', name='my-vpc')
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'my-vpc', 'status': 'Ready'},
        ]
        client.delete.return_value = (None, 204)
        # For wait_for_deleted, return 404
        client.get.return_value = None

        resource = CrudResource(module, VPC_CONFIG)
        resource.run()

        client.delete.assert_called_once()
        module.exit_json.assert_called_once()
        kw = module.exit_json.call_args[1]
        assert kw['changed'] is True
        assert kw['diff']['before']['name'] == 'my-vpc'
        assert kw['diff']['after'] == {}

    @patch('module_utils.resource.NicoClient')
    def test_delete_prefers_supplied_id_when_response_has_none(self, MockClient):
        """Resources keyed by a non-ID placeholder have no id in responses.

        machine_health_report is keyed by {source}; the user supplies that
        value as id, so the delete path must use it instead of
        existing.get('id').
        """
        config = _make_config(
            resource_path='/v2/org/{org}/nico/machine/{machineId}/health-report',
            resource_item_path='/v2/org/{org}/nico/machine/{machineId}/health-report/{source}',
            id_param='source',
            name_field=None,
            field_map={'machine_id': 'machineId'},
        )
        module = make_module(state='absent', id='bmc', machine_id='m-1')
        client = MockClient.return_value
        # Found by name-less singleton GET; the response carries no id field.
        client.get.return_value = {'source': 'bmc', 'successes': 1}

        CrudResource(module, config).run()

        client.delete.assert_called_once()
        assert '/health-report/bmc' in client.delete.call_args[0][0]

    @patch('module_utils.resource.NicoClient')
    def test_already_absent(self, MockClient):
        module = make_module(state='absent', name='nonexistent')
        client = MockClient.return_value
        client.list_all.return_value = []

        resource = CrudResource(module, VPC_CONFIG)
        resource.run()

        client.delete.assert_not_called()
        module.exit_json.assert_called_once()
        assert module.exit_json.call_args[1]['changed'] is False

    @patch('module_utils.resource.NicoClient')
    def test_delete_by_id_appends_required_query_params(self, MockClient):
        """Item DELETEs keyed by query parameters forward them (trusted-profile)."""
        config = _make_config(
            resource_path='/v2/org/{org}/nico/measured-boot/trusted-profile',
            resource_item_path='/v2/org/{org}/nico/measured-boot/trusted-profile/{id}',
            id_param='id',
            name_field=None,
            no_create=True,
            has_get_by_id=False,
            field_map={'site_id': 'siteId', 'selector': 'selector'},
            delete_query_fields=['site_id', 'selector'],
            required_delete_fields=['site_id', 'selector'],
        )
        module = make_module(state='absent', id='approval-1', site_id='site-1', selector='ApprovalId')
        client = MockClient.return_value
        client.delete.return_value = (None, 204)

        CrudResource(module, config).run()

        client.delete.assert_called_once()
        path, kwargs = client.delete.call_args[0][0], client.delete.call_args[1]
        assert path.split('?')[0].endswith('/trusted-profile/approval-1')
        assert 'siteId=site-1' in path
        assert 'selector=ApprovalId' in path
        assert kwargs.get('data') is None

    @patch('module_utils.resource.NicoClient')
    def test_collection_delete_is_keyed_by_query_params(self, MockClient):
        """Collection DELETEs without an item path are keyed by query params."""
        config = _make_config(
            resource_path='/v2/org/{org}/nico/expected-machine/all',
            resource_item_path='',
            id_param='id',
            name_field=None,
            has_get_by_id=False,
            field_map={'site_id': 'siteId'},
            delete_query_fields=['site_id'],
            required_delete_fields=['site_id'],
        )
        module = make_module(state='absent', site_id='site-1')
        client = MockClient.return_value
        client.delete.return_value = (None, 204)

        CrudResource(module, config).run()

        client.delete.assert_called_once()
        assert client.delete.call_args[0][0].endswith('/expected-machine/all?siteId=site-1')

    @patch('module_utils.resource.NicoClient')
    def test_delete_without_required_query_param_fails(self, MockClient):
        config = _make_config(
            resource_path='/v2/org/{org}/nico/expected-machine/all',
            resource_item_path='',
            id_param='id',
            name_field=None,
            has_get_by_id=False,
            field_map={'site_id': 'siteId'},
            delete_query_fields=['site_id'],
            required_delete_fields=['site_id'],
        )
        module = make_module(state='absent')
        client = MockClient.return_value

        CrudResource(module, config).run()

        module.fail_json.assert_called_once()
        assert 'site_id' in module.fail_json.call_args[1]['msg']
        client.delete.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_collection_delete_sends_required_body(self, MockClient):
        """Body-keyed DELETEs send their body even without a GET to find existing."""
        config = _make_config(
            resource_path='/v2/org/{org}/nico/firmware-config/host',
            resource_item_path='',
            id_param='id',
            name_field=None,
            has_get_by_id=False,
            field_map={'site_id': 'siteId'},
            delete_body_fields=['site_id', 'vendor', 'model'],
            required_delete_fields=['site_id', 'vendor', 'model'],
        )
        module = make_module(state='absent', site_id='site-1', vendor='Nvidia', model='DGXH100')
        client = MockClient.return_value
        client.delete.return_value = (None, 204)

        CrudResource(module, config).run()

        client.delete.assert_called_once()
        assert client.delete.call_args[1]['data'] == {
            'siteId': 'site-1', 'vendor': 'Nvidia', 'model': 'DGXH100',
        }

    @patch('module_utils.resource.NicoClient')
    def test_delete_without_required_body_field_fails(self, MockClient):
        config = _make_config(
            resource_path='/v2/org/{org}/nico/firmware-config/host',
            resource_item_path='',
            id_param='id',
            name_field=None,
            has_get_by_id=False,
            field_map={'site_id': 'siteId'},
            delete_body_fields=['site_id', 'vendor', 'model'],
            required_delete_fields=['site_id', 'vendor', 'model'],
        )
        module = make_module(state='absent', site_id='site-1')
        client = MockClient.return_value

        CrudResource(module, config).run()

        module.fail_json.assert_called_once()
        msg = module.fail_json.call_args[1]['msg']
        assert 'vendor' in msg and 'model' in msg
        client.delete.assert_not_called()


class TestCrudResourceDiffMode:
    """Ansible diff mode — before/after values returned on every mutating operation."""

    @patch('module_utils.resource.NicoClient')
    def test_update_diff_shows_before_and_after(self, MockClient):
        module = make_module(name='my-vpc', description='New desc')
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'v1', 'name': 'my-vpc', 'description': 'Old desc', 'status': 'Ready'},
        ]
        client.update.return_value = {'id': 'v1', 'name': 'my-vpc', 'description': 'New desc', 'status': 'Ready'}

        CrudResource(module, VPC_CONFIG).run()

        diff = module.exit_json.call_args[1]['diff']
        assert diff['before']['description'] == 'Old desc'
        assert diff['after']['description'] == 'New desc'

    @patch('module_utils.resource.NicoClient')
    def test_check_mode_update_diff_projects_changes(self, MockClient):
        module = make_module(name='my-vpc', description='Future desc')
        module.check_mode = True
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'v1', 'name': 'my-vpc', 'description': 'Current desc', 'status': 'Ready'},
        ]

        CrudResource(module, VPC_CONFIG).run()

        client.update.assert_not_called()
        diff = module.exit_json.call_args[1]['diff']
        assert diff['before']['description'] == 'Current desc'
        assert diff['after']['description'] == 'Future desc'

    @patch('module_utils.resource.NicoClient')
    def test_no_change_diff_before_equals_after(self, MockClient):
        module = make_module(name='my-vpc', description='Same')
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'v1', 'name': 'my-vpc', 'description': 'Same', 'status': 'Ready'},
        ]

        CrudResource(module, VPC_CONFIG).run()

        diff = module.exit_json.call_args[1]['diff']
        assert diff['before'] == diff['after']

    @patch('module_utils.resource.NicoClient')
    def test_delete_diff_shows_before_empty_after(self, MockClient):
        module = make_module(state='absent', name='my-vpc')
        client = MockClient.return_value
        client.list_all.return_value = [{'id': 'v1', 'name': 'my-vpc', 'status': 'Ready'}]
        client.delete.return_value = (None, 204)
        client.get.return_value = None

        CrudResource(module, VPC_CONFIG).run()

        diff = module.exit_json.call_args[1]['diff']
        assert diff['before']['name'] == 'my-vpc'
        assert diff['after'] == {}


class TestCrudResourceRequiredFields:
    @patch('module_utils.resource.NicoClient')
    def test_missing_required_create_field_fails_with_clear_message(self, MockClient):
        """required_create_fields are validated before the API call, not by a 422."""
        config = dict(VPC_CONFIG)
        config['required_create_fields'] = ['name', 'site_id']

        # 'name' and 'site_id' are not supplied
        module = make_module(id=None, name=None, site_id=None)
        client = MockClient.return_value
        client.get.return_value = None  # resource does not exist → create path

        resource = CrudResource(module, config)
        resource.run()

        module.fail_json.assert_called_once()
        msg = module.fail_json.call_args[1]['msg']
        assert 'name' in msg or 'site_id' in msg
        assert 'required' in msg.lower()
        client.create.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_required_field_present_does_not_fail(self, MockClient):
        """When required fields are provided, creation proceeds normally."""
        config = dict(VPC_CONFIG)
        config['required_create_fields'] = ['name']

        module = make_module(id=None, name='my-vpc', site_id='site-1', wait=False)
        client = MockClient.return_value
        client.list_all.return_value = []
        client.create.return_value = {'id': 'vpc-new', 'name': 'my-vpc', 'status': 'Ready'}

        resource = CrudResource(module, config)
        resource.run()

        client.create.assert_called_once()
        module.fail_json.assert_not_called()


class TestCrudResourceNoCreate:
    @patch('module_utils.resource.NicoClient')
    def test_no_create_fails(self, MockClient):
        config = dict(VPC_CONFIG)
        config['no_create'] = True
        config['name_field'] = None

        module = make_module(id=None, name=None)
        client = MockClient.return_value

        resource = CrudResource(module, config)
        resource.run()

        module.fail_json.assert_called_once()
        assert 'does not support creation' in module.fail_json.call_args[1]['msg']

    @patch('module_utils.resource.NicoClient')
    def test_update_only_with_id_targets_item_endpoint(self, MockClient):
        """no_create + no_wait + id + item path PATCHes the item endpoint.

        Resources like allocation_constraint have a PATCHable item path but no
        GET on it, so existence cannot be confirmed — the update must still
        target the item, not the collection.
        """
        cfg = _make_config(
            resource_path='/v2/org/{org}/nico/allocation/{allocation_id}/constraint',
            resource_item_path='/v2/org/{org}/nico/allocation/{allocation_id}/constraint/{allocation_constraint_id}',
            id_param='allocationConstraintId',
            name_field=None,
            no_create=True,
            no_wait=True,
            has_get_by_id=False,
            update_schema_fields=['max_tokens'],
        )
        module = make_module(allocation_id='alloc-1', id='constraint-9', max_tokens=5)
        client = MockClient.return_value
        client.get.return_value = None
        client.update.return_value = {'id': 'constraint-9'}

        CrudResource(module, cfg).run()

        path = client.update.call_args[0][0]
        assert 'alloc-1' in path
        assert 'constraint-9' in path
        assert '{allocationConstraintId}' not in path
        module.fail_json.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_update_only_id_addressing_independent_of_no_wait(self, MockClient):
        """id + item path targets the item endpoint even when wait is enabled.

        A listable update-only resource (no_wait=False) must not fall through
        to the "does not support creation" failure when the user supplies id.
        """
        cfg = _make_config(
            resource_path='/v2/org/{org}/nico/allocation/{allocation_id}/constraint',
            resource_item_path='/v2/org/{org}/nico/allocation/{allocation_id}/constraint/{allocation_constraint_id}',
            id_param='allocationConstraintId',
            name_field=None,
            no_create=True,
            no_wait=False,
            has_get_by_id=False,
            update_schema_fields=['max_tokens'],
        )
        module = make_module(allocation_id='alloc-1', id='constraint-9', max_tokens=5)
        client = MockClient.return_value
        client.update.return_value = {'id': 'constraint-9'}

        CrudResource(module, cfg).run()

        path = client.update.call_args[0][0]
        assert 'constraint-9' in path
        module.fail_json.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_absent_without_existing_with_id_targets_item_endpoint(self, MockClient):
        """state: absent with id but no GET-by-id DELETEs the resolved item path."""
        cfg = _make_config(
            resource_item_path='/v2/org/{org}/nico/allocation/{allocation_id}/constraint/{allocation_constraint_id}',
            id_param='allocationConstraintId',
            name_field=None,
            has_get_by_id=False,
        )
        module = make_module(state='absent', allocation_id='alloc-1', id='constraint-9')
        client = MockClient.return_value
        client.delete.return_value = (None, 204)
        client.get.return_value = None
        client.list_all.return_value = []

        CrudResource(module, cfg).run()

        path = client.delete.call_args[0][0]
        assert 'constraint-9' in path
        assert '{allocationConstraintId}' not in path


class TestCrudResourceOneOfAlternatives:
    """Exactly-one-alternative validation for flattened oneOf create schemas."""

    CONFIG = _make_config(
        create_schema_fields=['name', 'vpc_id', 'ip_block_id', 'prefix', 'prefix_length'],
        required_create_fields=['name', 'vpc_id', 'ip_block_id'],
        oneof_alternatives=[['prefix_length'], ['prefix']],
    )

    @patch('module_utils.resource.NicoClient')
    def test_create_with_one_alternative_succeeds(self, MockClient):
        """A payload providing only one branch's selector must not be rejected."""
        module = make_module(
            name='pfx', vpc_id='v-1', ip_block_id='ip-1', prefix_length=20,
        )
        client = MockClient.return_value
        client.list_all.return_value = []
        client.create.return_value = {'id': 'pfx-1', 'status': 'Ready'}

        CrudResource(module, self.CONFIG).run()

        client.create.assert_called_once()
        module.fail_json.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_create_with_no_alternative_fails(self, MockClient):
        module = make_module(name='pfx', vpc_id='v-1', ip_block_id='ip-1')
        client = MockClient.return_value
        client.list_all.return_value = []

        CrudResource(module, self.CONFIG).run()

        module.fail_json.assert_called_once()
        msg = module.fail_json.call_args[1]['msg']
        assert 'prefix_length' in msg and 'prefix' in msg
        client.create.assert_not_called()


class TestInfoResource:
    @patch('module_utils.resource.NicoClient')
    def test_get_by_id(self, MockClient):
        config = {
            'resource_path': '/v2/org/{org}/nico/vpc',
            'resource_item_path': '/v2/org/{org}/nico/vpc/{vpcId}',
            'id_param': 'vpcId',
            'filter_fields': ['site_id', 'status'],
        }
        module = make_module(id='vpc-123')
        client = MockClient.return_value
        client.get.return_value = {'id': 'vpc-123', 'name': 'my-vpc', 'siteId': 'site-1'}

        info = InfoResource(module, config)
        info.run()

        module.exit_json.assert_called_once()
        call_kwargs = module.exit_json.call_args[1]
        assert call_kwargs['changed'] is False
        assert call_kwargs['resource']['name'] == 'my-vpc'
        assert call_kwargs['resource']['site_id'] == 'site-1'

    @patch('module_utils.resource.NicoClient')
    def test_list_all(self, MockClient):
        config = {
            'resource_path': '/v2/org/{org}/nico/vpc',
            'resource_item_path': '/v2/org/{org}/nico/vpc/{vpcId}',
            'id_param': 'vpcId',
            'filter_fields': ['site_id'],
        }
        module = make_module(site_id='site-1')
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'vpc-a', 'siteId': 'site-1'},
            {'id': 'vpc-2', 'name': 'vpc-b', 'siteId': 'site-1'},
        ]

        info = InfoResource(module, config)
        info.run()

        module.exit_json.assert_called_once()
        call_kwargs = module.exit_json.call_args[1]
        assert call_kwargs['changed'] is False
        assert len(call_kwargs['resources']) == 2
        # Keys should be snake_case
        assert call_kwargs['resources'][0]['site_id'] == 'site-1'

    @patch('module_utils.resource.NicoClient')
    def test_get_not_found(self, MockClient):
        config = {
            'resource_path': '/v2/org/{org}/nico/vpc',
            'resource_item_path': '/v2/org/{org}/nico/vpc/{vpcId}',
            'id_param': 'vpcId',
            'filter_fields': [],
        }
        module = make_module(id='nonexistent')
        client = MockClient.return_value
        client.get.return_value = None

        info = InfoResource(module, config)
        info.run()

        module.fail_json.assert_called_once()

    @patch('module_utils.resource.NicoClient')
    def test_get_by_id_forwards_item_filter_as_repeated_params(self, MockClient):
        """List-valued item filters on item GETs become repeated query parameters."""
        config = {
            'resource_path': '/v2/org/{org}/nico/instance',
            'resource_item_path': '/v2/org/{org}/nico/instance/{instanceId}',
            'id_param': 'instanceId',
            'filter_fields': ['instance_type_id'],
            'item_filter_fields': ['instance_type_id'],
        }
        module = make_module(id='inst-1', instance_type_id=['it-a', 'it-b'])
        client = MockClient.return_value
        client.get.return_value = {'id': 'inst-1'}

        InfoResource(module, config).run()

        path = client.get.call_args[0][0]
        assert 'instanceTypeId=it-a' in path
        assert 'instanceTypeId=it-b' in path

    @patch('module_utils.resource.NicoClient')
    def test_get_by_id_rejects_collection_only_filter(self, MockClient):
        """A collection-only filter combined with id fails before any request.

        The item endpoint does not consume collection filters, so forwarding
        one could not enforce the caller's intent.
        """
        config = {
            'resource_path': '/v2/org/{org}/nico/instance',
            'resource_item_path': '/v2/org/{org}/nico/instance/{instanceId}',
            'id_param': 'instanceId',
            'filter_fields': ['instance_type_id'],
            'item_filter_fields': [],
        }
        module = make_module(id='inst-1', instance_type_id='it-a')
        client = MockClient.return_value

        InfoResource(module, config).run()

        module.fail_json.assert_called_once()
        msg = module.fail_json.call_args[1]['msg']
        assert 'instance_type_id' in msg
        client.get.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_get_by_id_drops_required_collection_only_filter(self, MockClient):
        """A required collection filter cannot be omitted, so it is dropped."""
        config = {
            'resource_path': '/v2/org/{org}/nico/sku',
            'resource_item_path': '/v2/org/{org}/nico/sku/{skuId}',
            'id_param': 'skuId',
            'filter_fields': ['site_id'],
            'item_filter_fields': [],
        }
        module = make_module(
            id='sku-1', site_id='site-1',
            argument_spec={'site_id': {'required': True}},
        )
        client = MockClient.return_value
        client.get.return_value = {'id': 'sku-1'}

        InfoResource(module, config).run()

        module.exit_json.assert_called_once()
        path = client.get.call_args[0][0]
        assert path.endswith('/nico/sku/sku-1')
        assert '?' not in path

    @patch('module_utils.resource.NicoClient')
    def test_get_by_id_treats_id_filter_as_lookup(self, MockClient):
        """An ``id`` collection filter doubles as the lookup parameter."""
        config = {
            'resource_path': '/v2/org/{org}/nico/allocation',
            'resource_item_path': '/v2/org/{org}/nico/allocation/{allocationId}',
            'id_param': 'allocationId',
            'filter_fields': ['id', 'status'],
            'item_filter_fields': [],
        }
        module = make_module(id='alloc-1')
        client = MockClient.return_value
        client.get.return_value = {'id': 'alloc-1'}

        InfoResource(module, config).run()

        module.exit_json.assert_called_once()
        path = client.get.call_args[0][0]
        assert path.endswith('/nico/allocation/alloc-1')

    @patch('module_utils.resource.NicoClient')
    def test_get_by_id_allows_filter_that_resolves_path_placeholder(self, MockClient):
        """A collection filter consumed by the item path is not rejected."""
        config = {
            'resource_path': '/v2/org/{org}/nico/site/{siteId}/sshKey',
            'resource_item_path': '/v2/org/{org}/nico/site/{siteId}/sshKey/{sshKeyId}',
            'id_param': 'sshKeyId',
            'filter_fields': ['site_id'],
            'item_filter_fields': [],
        }
        module = make_module(id='key-1', site_id='site-1')
        client = MockClient.return_value
        client.get.return_value = {'id': 'key-1'}

        InfoResource(module, config).run()

        module.exit_json.assert_called_once()
        path = client.get.call_args[0][0]
        assert '/nico/site/site-1/sshKey/key-1' in path
        assert '?' not in path


class TestBatchResource:
    @patch('module_utils.resource.NicoClient')
    def test_batch_create(self, MockClient):
        config = {
            'resource_path': '/v2/org/{org}/nico/instance/batch',
            'create_schema_fields': ['name_prefix', 'count', 'tenant_id', 'vpc_id'],
        }
        module = make_module(
            name_prefix='worker', count=4,
            tenant_id='t-1', vpc_id='vpc-1',
        )
        client = MockClient.return_value
        client.create.return_value = [
            {'id': 'i-1', 'name': 'worker-abc123'},
            {'id': 'i-2', 'name': 'worker-def456'},
        ]

        batch = BatchResource(module, config)
        batch.run()

        client.create.assert_called_once()
        module.exit_json.assert_called_once()
        call_kwargs = module.exit_json.call_args[1]
        assert call_kwargs['changed'] is True
        assert len(call_kwargs['resources']) == 2

    @patch('module_utils.resource.NicoClient')
    def test_batch_check_mode(self, MockClient):
        config = {
            'resource_path': '/v2/org/{org}/nico/instance/batch',
            'create_schema_fields': ['name_prefix', 'count'],
        }
        module = make_module(name_prefix='worker', count=2)
        module.check_mode = True
        client = MockClient.return_value

        batch = BatchResource(module, config)
        batch.run()

        client.create.assert_not_called()
        module.exit_json.assert_called_once()
        assert module.exit_json.call_args[1]['changed'] is True

    @patch('module_utils.resource.NicoClient')
    def test_batch_prunes_unset_suboptions(self, MockClient):
        """AnsibleModule sets omitted suboption keys to None; nulls are not sent."""
        config = {
            'resource_path': '/v2/org/{org}/nico/instance/batch',
            'create_schema_fields': ['interfaces'],
            'field_map': {},
        }
        module = make_module(
            interfaces=[{'subnet_id': 'subnet-1', 'is_physical': None}],
        )
        client = MockClient.return_value
        client.create.return_value = []

        BatchResource(module, config).run()

        client.create.assert_called_once()
        assert client.create.call_args[0][1] == {
            'interfaces': [{'subnetId': 'subnet-1'}],
        }


class TestResolvePath:
    def test_org_substitution(self):
        result = _resolve_path('/v2/org/{org}/nico/vpc', {}, 'my-org')
        assert result == '/v2/org/my-org/nico/vpc'

    def test_parent_id_substitution(self):
        params = {'allocation_id': 'alloc-123'}
        result = _resolve_path(
            '/v2/org/{org}/nico/allocation/{allocationId}/constraint',
            params, 'my-org',
        )
        assert result == '/v2/org/my-org/nico/allocation/alloc-123/constraint'

    def test_item_id_substitution(self):
        params = {
            'allocation_id': 'alloc-123',
            'allocation_constraint_id': 'const-456',
        }
        result = _resolve_path(
            '/v2/org/{org}/nico/allocation/{allocationId}/constraint/{allocationConstraintId}',
            params, 'my-org',
        )
        assert result == '/v2/org/my-org/nico/allocation/alloc-123/constraint/const-456'

    def test_missing_param_left_unresolved(self):
        result = _resolve_path(
            '/v2/org/{org}/nico/allocation/{allocationId}/constraint',
            {}, 'my-org',
        )
        assert '{allocationId}' in result

    def test_simple_path(self):
        result = _resolve_path('/v2/org/{org}/nico/vpc/{vpcId}', {'vpc_id': 'v-1'}, 'o')
        assert result == '/v2/org/o/nico/vpc/v-1'


class TestWaitDefaults:
    @patch('module_utils.resource.NicoClient')
    def test_wait_defaults_to_true_when_none(self, MockClient):
        """When wait param is None (not specified), should default to True."""
        module = make_module(state='absent', name='my-vpc', wait=None, wait_timeout=None)
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'my-vpc', 'status': 'Ready'},
        ]
        client.delete.return_value = (None, 204)
        client.get.return_value = None  # 404 for wait_for_deleted

        resource = CrudResource(module, VPC_CONFIG)
        resource.run()

        # delete was called
        client.delete.assert_called_once()
        # wait_for_deleted was called (get was called to poll)
        client.get.assert_called()

    @patch('module_utils.resource.NicoClient')
    def test_wait_false_skips_polling(self, MockClient):
        """When wait=False, should not poll for deletion."""
        module = make_module(state='absent', name='my-vpc', wait=False)
        client = MockClient.return_value
        client.list_all.return_value = [
            {'id': 'vpc-1', 'name': 'my-vpc', 'status': 'Ready'},
        ]
        client.delete.return_value = (None, 204)

        resource = CrudResource(module, VPC_CONFIG)
        resource.run()

        client.delete.assert_called_once()
        # get should NOT have been called for polling
        client.get.assert_not_called()


class TestNestedResourcePaths:
    CONSTRAINT_CONFIG = _make_config(
        resource_path='/v2/org/{org}/nico/allocation/{allocationId}/constraint',
        resource_item_path='/v2/org/{org}/nico/allocation/{allocationId}/constraint/{allocationConstraintId}',
        id_param='allocationConstraintId',
        create_schema_fields=['resource_type', 'constraint_type', 'constraint_value'],
        update_schema_fields=['constraint_value'],
        scope_fields=['allocation_id'],
    )

    @patch('module_utils.resource.NicoClient')
    def test_find_by_id_resolves_parent_path(self, MockClient):
        module = make_module(id='const-1', allocation_id='alloc-1')
        client = MockClient.return_value
        client.get.return_value = {'id': 'const-1', 'constraintType': 'Reserved'}

        resource = CrudResource(module, self.CONSTRAINT_CONFIG)
        result = resource._find_existing()

        # Verify the path has allocationId resolved
        get_path = client.get.call_args[0][0]
        assert 'alloc-1' in get_path
        assert '{allocationId}' not in get_path

    @patch('module_utils.resource.NicoClient')
    def test_list_resolves_parent_path(self, MockClient):
        module = make_module(name='my-constraint', allocation_id='alloc-1')
        client = MockClient.return_value
        client.list_all.return_value = []

        resource = CrudResource(module, self.CONSTRAINT_CONFIG)
        resource._find_existing()

        list_path = client.list_all.call_args[0][0]
        assert 'alloc-1' in list_path
        assert '{allocationId}' not in list_path


# ---------------------------------------------------------------------------
# ActionResource
# ---------------------------------------------------------------------------

_ACTION_CFG = {
    'resource_path': '/v2/org/{org}/nico/rack/{rack_id}/power',
    'resource_item_path': '/v2/org/{org}/nico/rack/{rack_id}/power',
    'method': 'PATCH',
    'body_fields': ['action'],
    'query_fields': [],
    'field_map': {},
    'opaque_fields': [],
    'required_body_fields': [],
    'id_param': 'id',
}

_GET_ACTION_CFG = {
    'resource_path': '/v2/org/{org}/nico/rack/validation',
    'resource_item_path': '/v2/org/{org}/nico/rack/{rack_id}/validation',
    'method': 'GET',
    'body_fields': [],
    'query_fields': ['site_id'],
    'field_map': {},
    'opaque_fields': [],
    'required_body_fields': [],
    'id_param': 'id',
}


class TestActionResource:
    @patch('module_utils.resource.NicoClient')
    def test_get_action_no_id_calls_collection(self, MockClient):
        module = make_module(rack_id='r-1', site_id='s-1')
        client = MockClient.return_value
        client.get.return_value = {'validationResult': 'ok'}

        ActionResource(module, _GET_ACTION_CFG).run()

        client.get.assert_called_once()
        path = client.get.call_args[0][0]
        assert 'site_id=s-1' in path or 'siteId=s-1' in path
        assert module.exit_json.call_args[1]['changed'] is False

    @patch('module_utils.resource.NicoClient')
    def test_get_action_fails_on_404(self, MockClient):
        """A GET action against a missing target fails instead of reporting success."""
        module = make_module(id='rack-1', rack_id='rack-1')
        client = MockClient.return_value
        client.get.return_value = None

        ActionResource(module, _GET_ACTION_CFG).run()

        module.fail_json.assert_called_once()
        assert '404' in module.fail_json.call_args[1]['msg']

    @patch('module_utils.resource.NicoClient')
    def test_get_action_fails_on_unresolved_path(self, MockClient):
        """A GET action with an unresolved path placeholder fails clearly."""
        module = make_module(id='rack-1')  # no rack_id
        # A real AnsibleModule.fail_json raises SystemExit.
        module.fail_json.side_effect = SystemExit
        client = MockClient.return_value

        with pytest.raises(SystemExit):
            ActionResource(module, _GET_ACTION_CFG).run()

        assert 'rack_id' in module.fail_json.call_args[1]['msg']
        client.get.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_get_action_with_id_calls_item_path(self, MockClient):
        module = make_module(id='rack-1', rack_id='rack-1')
        client = MockClient.return_value
        client.get.return_value = {'status': 'valid'}

        ActionResource(module, _GET_ACTION_CFG).run()

        path = client.get.call_args[0][0]
        assert 'rack-1' in path
        assert '{rack_id}' not in path

    @patch('module_utils.resource.NicoClient')
    def test_patch_action_no_id_calls_collection(self, MockClient):
        module = make_module(rack_id='r-1', action='GracefulRestart')
        client = MockClient.return_value
        client.update.return_value = {'message': 'accepted'}

        ActionResource(module, _ACTION_CFG).run()

        client.update.assert_called_once()
        path = client.update.call_args[0][0]
        assert 'r-1' in path
        assert module.exit_json.call_args[1]['changed'] is True

    @patch('module_utils.resource.NicoClient')
    def test_patch_action_with_id_calls_item_path(self, MockClient):
        module = make_module(id='rack-1', rack_id='rack-1', action='ForceOff')
        client = MockClient.return_value
        client.update.return_value = None

        ActionResource(module, _ACTION_CFG).run()

        path = client.update.call_args[0][0]
        assert 'rack-1' in path
        assert '{rack_id}' not in path

    @patch('module_utils.resource.NicoClient')
    def test_patch_action_prunes_unset_suboptions(self, MockClient):
        """AnsibleModule sets omitted suboption keys to None; nulls are not sent."""
        config = dict(
            _ACTION_CFG,
            body_fields=['firmware'],
            field_map={},
        )
        module = make_module(
            id='rack-1', rack_id='rack-1',
            firmware={'version': '1.0', 'force': None},
        )
        client = MockClient.return_value
        client.update.return_value = {}

        ActionResource(module, config).run()

        client.update.assert_called_once()
        assert client.update.call_args[0][1] == {'firmware': {'version': '1.0'}}

    @patch('module_utils.resource.NicoClient')
    def test_check_mode_does_not_call_update(self, MockClient):
        module = make_module(rack_id='r-1', action='ForceOff')
        module.check_mode = True
        client = MockClient.return_value

        ActionResource(module, _ACTION_CFG).run()

        client.update.assert_not_called()
        assert module.exit_json.call_args[1]['changed'] is True

    @patch('module_utils.resource.NicoClient')
    def test_missing_required_body_field_fails(self, MockClient):
        cfg = dict(_ACTION_CFG, required_body_fields=['site_id'])
        module = make_module(rack_id='r-1', action='ForceOff')
        client = MockClient.return_value

        ActionResource(module, cfg).run()

        module.fail_json.assert_called_once()
        assert 'site_id' in module.fail_json.call_args[1]['msg']
        client.update.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_required_body_field_present_proceeds(self, MockClient):
        cfg = dict(_ACTION_CFG, required_body_fields=['action'])
        module = make_module(rack_id='r-1', action='ForceOff')
        client = MockClient.return_value
        client.update.return_value = None

        ActionResource(module, cfg).run()

        client.update.assert_called_once()
        module.fail_json.assert_not_called()

    @patch('module_utils.resource.NicoClient')
    def test_fire_and_forget_crud_dispatches_patch(self, MockClient):
        """no_create=True + no_wait=True routes state:present to a direct PATCH."""
        cfg = _make_config(
            resource_path='/v2/org/{org}/nico/machine/{machine_id}/power',
            resource_item_path='',
            no_create=True,
            no_wait=True,
            update_schema_fields=['action'],
        )
        module = make_module(machine_id='m-1', action='ForceOff')
        client = MockClient.return_value
        client.update.return_value = {'message': 'accepted'}

        CrudResource(module, cfg).run()

        client.update.assert_called_once()
        module.fail_json.assert_not_called()
        assert module.exit_json.call_args[1]['changed'] is True
