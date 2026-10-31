# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

import os
import sys
import pytest
import yaml

# Add the generator to the path
_TOOLS_GENERATE = os.path.join(
    os.path.dirname(__file__), '..', '..', '..', 'tools', 'generate',
)
sys.path.insert(0, _TOOLS_GENERATE)

from common import (
    resolve_ref,
    resolve_refs_recursive,
    resolve_composition,
    classify_path,
    extract_id_param,
    detect_nested_module_name,
)
from backends.ansible import (
    tag_to_module_name,
    openapi_type_to_ansible,
    schema_to_argument_spec,
    generate,
    _is_secret_field,
)


class TestTagToModuleName:
    def test_simple(self):
        assert tag_to_module_name('VPC') == 'vpc'

    def test_two_words(self):
        assert tag_to_module_name('SSH Key Group') == 'ssh_key_group'

    def test_ip_block(self):
        assert tag_to_module_name('IP Block') == 'ip_block'

    def test_infiniband(self):
        assert tag_to_module_name('InfiniBand Partition') == 'infiniband_partition'

    def test_operating_system(self):
        assert tag_to_module_name('Operating System') == 'operating_system'

    def test_instance_type(self):
        assert tag_to_module_name('Instance Type') == 'instance_type'

    def test_unmapped_tag(self):
        result = tag_to_module_name('Site')
        assert result == 'site'

    def test_allocation(self):
        result = tag_to_module_name('Allocation')
        assert result == 'allocation'


class TestResolveRef:
    def test_resolve(self):
        spec = {
            'components': {
                'schemas': {
                    'VPC': {'type': 'object', 'properties': {'name': {'type': 'string'}}}
                }
            }
        }
        result = resolve_ref(spec, '#/components/schemas/VPC')
        assert result['type'] == 'object'
        assert 'name' in result['properties']

    def test_invalid_ref(self):
        spec = {'components': {'schemas': {}}}
        result = resolve_ref(spec, '#/components/schemas/Missing')
        assert result == {}


class TestResolveRefsRecursive:
    def test_nested_refs(self):
        spec = {
            'components': {
                'schemas': {
                    'Inner': {'type': 'string', 'enum': ['A', 'B']},
                    'Outer': {
                        'type': 'object',
                        'properties': {
                            'status': {'$ref': '#/components/schemas/Inner'},
                        },
                    },
                },
            },
        }
        result = resolve_refs_recursive(spec, spec['components']['schemas']['Outer'])
        assert result['properties']['status']['type'] == 'string'
        assert result['properties']['status']['enum'] == ['A', 'B']


class TestOpenApiTypeToAnsible:
    def test_string(self):
        assert openapi_type_to_ansible({'type': 'string'}, {}) == {'type': 'str'}

    def test_integer(self):
        assert openapi_type_to_ansible({'type': 'integer'}, {}) == {'type': 'int'}

    def test_boolean(self):
        assert openapi_type_to_ansible({'type': 'boolean'}, {}) == {'type': 'bool'}

    def test_string_enum(self):
        result = openapi_type_to_ansible({'type': 'string', 'enum': ['A', 'B']}, {})
        assert result == {'type': 'str', 'choices': ['A', 'B']}

    def test_nullable_string(self):
        result = openapi_type_to_ansible({'type': ['string', 'null']}, {})
        assert result == {'type': 'str'}

    def test_array_of_strings(self):
        result = openapi_type_to_ansible(
            {'type': 'array', 'items': {'type': 'string'}}, {}
        )
        assert result == {'type': 'list', 'elements': 'str'}

    def test_array_of_uuids(self):
        result = openapi_type_to_ansible(
            {'type': 'array', 'items': {'type': 'string', 'format': 'uuid'}}, {}
        )
        assert result == {'type': 'list', 'elements': 'str'}

    def test_object_with_additional_properties(self):
        result = openapi_type_to_ansible(
            {'type': 'object', 'additionalProperties': {'type': 'string'}}, {}
        )
        assert result == {'type': 'dict'}

    def test_object_with_properties(self):
        schema = {
            'type': 'object',
            'properties': {
                'subnetId': {'type': 'string'},
                'isPhysical': {'type': 'boolean'},
            },
        }
        result = openapi_type_to_ansible(schema, {})
        assert result['type'] == 'dict'
        assert 'options' in result
        assert 'subnet_id' in result['options']
        assert result['options']['subnet_id'] == {'type': 'str'}
        assert result['options']['is_physical'] == {'type': 'bool'}

    def test_empty_schema(self):
        assert openapi_type_to_ansible({}, {}) == {'type': 'str'}
        assert openapi_type_to_ansible(None, {}) == {'type': 'str'}


class TestSchemaToArgumentSpec:
    def test_basic_schema(self):
        schema = {
            'type': 'object',
            'properties': {
                'name': {'type': 'string', 'description': 'Name'},
                'siteId': {'type': 'string', 'format': 'uuid'},
                'id': {'type': 'string', 'readOnly': True},
                'status': {'type': 'string', 'readOnly': True},
            },
            'required': ['name', 'siteId'],
        }
        result, _field_map, _opaque = schema_to_argument_spec(schema, {})

        assert 'name' in result
        assert result['name']['required'] is True
        assert 'site_id' in result
        assert result['site_id']['required'] is True
        # readOnly fields excluded
        assert 'id' not in result
        assert 'status' not in result

    def test_include_fields(self):
        schema = {
            'type': 'object',
            'properties': {
                'name': {'type': 'string'},
                'description': {'type': 'string'},
                'siteId': {'type': 'string'},
            },
        }
        result, _field_map, _opaque = schema_to_argument_spec(schema, {}, include_fields=['name', 'description'])
        assert 'name' in result
        assert 'description' in result
        assert 'site_id' not in result


class TestResolveComposition:
    def _vpc_prefix_schema(self):
        return {
            'type': 'object',
            'required': ['name', 'vpcId', 'ipBlockId'],
            'properties': {
                'name': {'type': 'string'},
                'vpcId': {'type': 'string'},
                'ipBlockId': {'type': 'string'},
                'prefix': {'type': 'string', 'description': 'Exact CIDR'},
                'prefixLength': {'type': 'integer', 'description': 'Automatic length'},
            },
            'oneOf': [
                {
                    'required': ['name', 'vpcId', 'ipBlockId', 'prefixLength'],
                    'properties': {'name': {'type': 'string'}, 'vpcId': {'type': 'string'},
                                   'ipBlockId': {'type': 'string'}, 'prefixLength': {'type': 'integer'}},
                },
                {
                    'required': ['name', 'vpcId', 'ipBlockId', 'prefix'],
                    'properties': {'name': {'type': 'string'}, 'vpcId': {'type': 'string'},
                                   'ipBlockId': {'type': 'string'}, 'prefix': {'type': 'string'}},
                },
            ],
        }

    def test_oneof_branch_required_not_globally_required(self):
        merged = resolve_composition(self._vpc_prefix_schema(), {})
        assert sorted(merged['required']) == ['ipBlockId', 'name', 'vpcId']

    def test_oneof_records_alternatives(self):
        merged = resolve_composition(self._vpc_prefix_schema(), {})
        assert merged['x-oneof-required'] == [['prefixLength'], ['prefix']]

    def test_oneof_base_branch_suppresses_alternatives(self):
        """A branch covered by top-level required fields is a valid base create.

        Emitting the remaining alternatives would make the module reject a
        base create that omits every branch-specific field, so a schema with
        such a branch carries no alternatives and leaves the validation to
        the REST API.
        """
        schema = {
            'type': 'object',
            'required': ['name'],
            'properties': {'name': {'type': 'string'}},
            'oneOf': [
                {'type': 'object', 'required': ['name'], 'properties': {'name': {'type': 'string'}}},
                {'type': 'object', 'required': ['name', 'extra'],
                 'properties': {'name': {'type': 'string'}, 'extra': {'type': 'string'}}},
            ],
        }
        merged = resolve_composition(schema, {})
        assert 'x-oneof-required' not in merged

    def test_oneof_keeps_documented_top_level_properties(self):
        merged = resolve_composition(self._vpc_prefix_schema(), {})
        # Branch schemas carry no descriptions; the documented top-level
        # definitions must survive the merge.
        assert merged['properties']['prefixLength']['description'] == 'Automatic length'

    def test_allof_unions_required(self):
        schema = {
            'type': 'object',
            'allOf': [
                {'type': 'object', 'required': ['a'], 'properties': {'a': {'type': 'string'}}},
                {'type': 'object', 'required': ['b'], 'properties': {'b': {'type': 'string'}}},
            ],
        }
        merged = resolve_composition(schema, {})
        assert sorted(merged['required']) == ['a', 'b']
        assert 'x-oneof-required' not in merged


class TestIsSecretField:
    def test_identifier_key_names_are_not_secret(self):
        assert not _is_secret_field('public_key')
        assert not _is_secret_field('ssh_key_group_id')
        assert not _is_secret_field('ssh_key_ids')
        assert not _is_secret_field('partition_key')

    def test_secret_names_are_secret(self):
        assert _is_secret_field('private_key')
        assert _is_secret_field('api_key')
        assert _is_secret_field('password')
        assert _is_secret_field('auth_token')

    def test_allowlisted_token_fields_are_not_secret(self):
        # Non-secret configuration values whose names match the secret
        # pattern; no_log=True would mask them in module results.
        assert not _is_secret_field('token_ttl_seconds')
        assert not _is_secret_field('token_endpoint')
        assert not _is_secret_field('id_token_signing_alg_values_supported')

    def test_other_token_names_remain_secret(self):
        assert _is_secret_field('token')
        assert _is_secret_field('refresh_token')


class TestClassifyPath:
    def test_collection(self):
        assert classify_path('/v2/org/{org}/nico/vpc') == 'collection'

    def test_item(self):
        assert classify_path('/v2/org/{org}/nico/vpc/{vpcId}') == 'item'

    def test_nested_collection(self):
        assert classify_path('/v2/org/{org}/nico/allocation/{allocationId}/constraint') == 'collection'

    def test_nested_item(self):
        assert classify_path('/v2/org/{org}/nico/allocation/{allocationId}/constraint/{constraintId}') == 'item'


class TestExtractIdParam:
    def test_item_path(self):
        assert extract_id_param('/v2/org/{org}/nico/vpc/{vpcId}') == 'vpcId'

    def test_collection_path(self):
        assert extract_id_param('/v2/org/{org}/nico/vpc') is None


class TestDetectNestedModuleName:
    def test_not_nested(self):
        assert detect_nested_module_name('/v2/org/{org}/nico/vpc') is None

    def test_nested_constraint(self):
        assert detect_nested_module_name(
            '/v2/org/{org}/nico/allocation/{allocationId}/constraint'
        ) == 'allocation_constraint'

    def test_instance_type(self):
        assert detect_nested_module_name(
            '/v2/org/{org}/nico/instance/type'
        ) == 'instance_type'

    def test_current_skipped(self):
        assert detect_nested_module_name(
            '/v2/org/{org}/nico/infrastructure-provider/current'
        ) is None

    def test_non_nico_path(self):
        assert detect_nested_module_name('/v2/org/{org}/forge/something') is None


class TestFullGeneration:
    """Integration test: run the generator against the real spec."""

    @pytest.fixture
    def spec_path(self):
        # Look for the spec relative to the worktree root (three levels up from this file)
        worktree_root = os.path.normpath(
            os.path.join(os.path.dirname(__file__), '..', '..', '..')
        )
        path = os.path.join(worktree_root, 'rest-api', 'openapi', 'spec.yaml')
        if not os.path.exists(path):
            pytest.skip('OpenAPI spec not found at %s' % path)
        return path

    @pytest.fixture
    def spec(self, spec_path):
        with open(spec_path, 'r') as f:
            import yaml
            return yaml.safe_load(f)

    def test_generates_expected_modules(self, spec, tmp_path):
        generated = generate(spec, str(tmp_path))

        # Check key modules exist
        assert 'vpc.py' in generated
        assert 'vpc_info.py' in generated
        assert 'instance.py' in generated
        assert 'instance_info.py' in generated
        assert 'instance_batch.py' in generated
        assert 'machine.py' in generated
        assert 'machine_info.py' in generated
        assert 'allocation_constraint.py' in generated
        assert 'machine_capability_info.py' in generated

        # Read-only modules
        assert 'service_account_info.py' in generated
        assert 'infrastructure_provider_info.py' in generated
        assert 'tenant_info.py' in generated
        assert 'user_info.py' in generated
        assert 'metadata_info.py' in generated
        assert 'audit_info.py' in generated
        assert 'rack_info.py' in generated
        assert 'sku_info.py' in generated

        # All files should be valid Python
        for filename in generated:
            filepath = os.path.join(str(tmp_path), filename)
            assert os.path.exists(filepath), 'Missing file: %s' % filename
            with open(filepath) as f:
                source = f.read()
            compile(source, filepath, 'exec')

    def test_vpc_module_structure(self, spec, tmp_path):
        generate(spec, str(tmp_path))

        with open(os.path.join(str(tmp_path), 'vpc.py')) as f:
            source = f.read()

        assert 'DOCUMENTATION' in source
        assert 'EXAMPLES' in source
        assert 'RETURN' in source
        assert 'ARGUMENT_SPEC' in source
        assert 'RESOURCE_CONFIG' in source
        assert 'CrudResource' in source
        assert "resource_path': '/v2/org/{org}/nico/vpc'" in source
        assert "'id_param': 'vpcId'" in source

    def test_machine_no_create(self, spec, tmp_path):
        generate(spec, str(tmp_path))

        with open(os.path.join(str(tmp_path), 'machine.py')) as f:
            source = f.read()

        assert "'no_create': True" in source
        assert "'name_field': None" in source

    def test_instance_batch_module(self, spec, tmp_path):
        generate(spec, str(tmp_path))

        with open(os.path.join(str(tmp_path), 'instance_batch.py')) as f:
            source = f.read()

        assert 'BatchResource' in source
        assert 'name_prefix' in source
        assert 'count' in source
        assert 'topology_optimized' in source

    def test_vpc_prefix_module_records_oneof_alternatives(self, spec, tmp_path):
        generate(spec, str(tmp_path))

        with open(os.path.join(str(tmp_path), 'vpc_prefix.py')) as f:
            source = f.read()

        assert "'oneof_alternatives': [" in source
        assert "['prefix_length']" in source
        assert "['prefix']" in source
        # Branch-specific selectors are no longer flagged as create-required.
        assert "'required_create_fields': ['name', 'vpc_id', 'ip_block_id']" in source

    def test_no_wait_module_omits_wait_arguments(self, spec, tmp_path):
        generate(spec, str(tmp_path))

        with open(os.path.join(str(tmp_path), 'task_run_pause.py')) as f:
            source = f.read()

        assert "'no_wait': True" in source
        assert 'wait_timeout' not in source
        assert "'wait'" not in source

    def test_list_only_resource_omits_wait_arguments(self, spec, tmp_path):
        """A collection GET cannot keep a readiness poll alive.

        measured-boot trusted-profile has a collection GET but its item path
        is DELETE-only; _wait_for_ready polls the item path, so waiting must
        stay disabled or every wait fails with "resource disappeared".
        """
        generate(spec, str(tmp_path))

        with open(os.path.join(str(tmp_path), 'measured_boot_trusted_profile.py')) as f:
            source = f.read()

        assert "'no_wait': True" in source
        assert 'wait_timeout' not in source
        assert "'wait'" not in source

    def test_update_only_module_gets_runnable_example(self, spec, tmp_path):
        """Update-only action modules must not ship an empty EXAMPLES block."""
        generate(spec, str(tmp_path))

        with open(os.path.join(str(tmp_path), 'machine_power.py')) as f:
            source = f.read()

        # The PATCH body requires action; the path requires machine_id.
        assert 'action: "{{ action }}"' in source
        assert 'machine_id: "{{ machine_id }}"' in source
        assert 'state: present' in source

        with open(os.path.join(str(tmp_path), 'machine_spectrumx_multiplane.py')) as f:
            source = f.read()

        assert 'maintenance_mode: "{{ maintenance_mode }}"' in source

    def test_instance_example_includes_conditional_create_fields(self, spec, tmp_path):
        generate(spec, str(tmp_path))

        for module in ('instance.py', 'instance_batch.py'):
            with open(os.path.join(str(tmp_path), module)) as f:
                source = f.read()

            assert 'operating_system_id: "{{ operating_system_id }}"' in source
            assert 'subnet_id: "{{ subnet_id }}"' in source
            assert 'is_physical: true' in source

    def test_firmware_config_host_example_includes_ordering(self, spec, tmp_path):
        generate(spec, str(tmp_path))

        with open(os.path.join(str(tmp_path), 'firmware_config_host.py')) as f:
            source = f.read()

        assert 'ordering: "{{ ordering }}"' in source

    def test_instance_info_list_filters(self, spec, tmp_path):
        generate(spec, str(tmp_path))

        with open(os.path.join(str(tmp_path), 'instance_info.py')) as f:
            source = f.read()

        # Repeatable query parameters accept lists.
        for filter_name in ('instance_type_id', 'machine_id', 'ip_address'):
            assert "%s=dict(\n        type='list',\n        elements='str'," % filter_name in source


class TestGeneratedModuleParity:
    """Generate all modules from the real spec and verify correctness properties.

    These tests catch regressions where the generator emits inconsistent
    DOCUMENTATION vs ARGUMENT_SPEC, or EXAMPLES that reference undeclared params.
    """

    # Auth params merged at runtime via get_auth_argument_spec(); not in ARGUMENT_SPEC.
    _AUTH_PARAMS = frozenset({'api_url', 'api_token', 'org', 'api_path_prefix'})
    # Standard Ansible task-level keys, not module parameters.
    _ANSIBLE_BUILTINS = frozenset({'name', 'register', 'when', 'loop', 'become', 'tags'})
    _SKIP_PARAMS = _AUTH_PARAMS | _ANSIBLE_BUILTINS

    @pytest.fixture(scope='class')
    def spec_path(self):
        worktree_root = os.path.normpath(
            os.path.join(os.path.dirname(__file__), '..', '..', '..')
        )
        path = os.path.join(worktree_root, 'rest-api', 'openapi', 'spec.yaml')
        if not os.path.exists(path):
            pytest.skip('OpenAPI spec not found at %s' % path)
        return path

    @pytest.fixture(scope='class')
    def generated_sources(self, spec_path, tmp_path_factory):
        """Generate all modules once and return {module_name: source} mapping."""
        import yaml as _yaml
        tmp = tmp_path_factory.mktemp('parity')
        with open(spec_path) as f:
            spec = _yaml.safe_load(f)
        filenames = generate(spec, str(tmp))
        sources = {}
        for fn in filenames:
            src = open(os.path.join(str(tmp), fn)).read()
            sources[fn[:-3]] = src
        return sources

    def _extract_argument_spec(self, source):
        """Parse ARGUMENT_SPEC from generated module source. Returns dict or None."""
        import re
        m = re.search(r'ARGUMENT_SPEC = (dict\(.*?\))\n\n', source, re.DOTALL)
        if not m:
            return None
        try:
            return eval(m.group(1))
        except Exception:
            return None

    def _extract_documentation_options(self, source):
        """Parse DOCUMENTATION YAML and return options dict or None."""
        import re
        m = re.search(r"DOCUMENTATION = r'''(.*?)'''", source, re.DOTALL)
        if not m:
            return None
        try:
            doc = yaml.safe_load(m.group(1))
            return doc.get('options', {})
        except Exception:
            return None

    def _extract_examples(self, source):
        """Parse EXAMPLES YAML and return list of tasks or None."""
        import re
        m = re.search(r"EXAMPLES = r'''(.*?)'''", source, re.DOTALL)
        if not m:
            return None
        try:
            return yaml.safe_load(m.group(1))
        except Exception:
            return None

    def _extract_resource_config(self, source):
        """Parse RESOURCE_CONFIG from generated module source. Returns dict or None."""
        import re
        m = re.search(r'RESOURCE_CONFIG = (\{.*?\})\n\n', source, re.DOTALL)
        if not m:
            return None
        try:
            return eval(m.group(1))
        except Exception:
            return None

    def test_examples_satisfy_runtime_contract(self, generated_sources):
        """Every example task must satisfy the module's runtime validation.

        - Ansible rejects any task missing an ARGUMENT_SPEC entry marked
          ``required=True`` (required path/query parameters, batch fields).
        - ``CrudResource`` fails ``state: present`` creates missing any
          ``required_create_fields`` entry.
        - ``ActionResource`` fails tasks missing any ``required_body_fields``
          entry.
        """
        failures = []
        for modname, source in generated_sources.items():
            arg_spec = self._extract_argument_spec(source)
            config = self._extract_resource_config(source)
            examples = self._extract_examples(source)
            if arg_spec is None or config is None or not isinstance(examples, list):
                continue

            required_by_ansible = {
                key for key, entry in arg_spec.items() if entry.get('required')
            }
            required_create = set(config.get('required_create_fields') or [])
            required_body = set(config.get('required_body_fields') or [])

            for task in examples:
                if not isinstance(task, dict):
                    continue
                for task_key, params in task.items():
                    if not isinstance(params, dict):
                        continue
                    provided = set(params)
                    missing = required_by_ansible - provided
                    if missing:
                        failures.append('%s: task %r misses required argument(s) %s' % (
                            modname, task_key, ', '.join(sorted(missing))))
                    if params.get('state') == 'present':
                        missing = required_create - provided
                        if missing:
                            failures.append('%s: create task %r misses required create field(s) %s' % (
                                modname, task_key, ', '.join(sorted(missing))))
                    if 'state' not in params and required_body:
                        missing = required_body - provided
                        if missing:
                            failures.append('%s: action task %r misses required body field(s) %s' % (
                                modname, task_key, ', '.join(sorted(missing))))
        assert not failures, 'Examples violate the module runtime contract:\n' + '\n'.join(sorted(set(failures)))

    def test_documentation_matches_argument_spec_defaults(self, generated_sources):
        """DOCUMENTATION options must have the same default as ARGUMENT_SPEC."""
        failures = []
        for modname, source in generated_sources.items():
            arg_spec = self._extract_argument_spec(source)
            doc_opts = self._extract_documentation_options(source)
            if arg_spec is None or doc_opts is None:
                continue
            for key, entry in arg_spec.items():
                if 'default' not in entry:
                    continue
                opt = doc_opts.get(key, {})
                if 'default' not in opt:
                    failures.append('%s.%s: default=%r missing from DOCUMENTATION' % (
                        modname, key, entry['default']))
                elif opt['default'] != entry['default']:
                    failures.append('%s.%s: DOCUMENTATION default=%r but ARGUMENT_SPEC default=%r' % (
                        modname, key, opt['default'], entry['default']))
        assert not failures, 'DOCUMENTATION/ARGUMENT_SPEC default mismatch:\n' + '\n'.join(failures)

    def test_documentation_matches_argument_spec_choices(self, generated_sources):
        """DOCUMENTATION options must list the same choices as ARGUMENT_SPEC."""
        failures = []
        for modname, source in generated_sources.items():
            arg_spec = self._extract_argument_spec(source)
            doc_opts = self._extract_documentation_options(source)
            if arg_spec is None or doc_opts is None:
                continue
            for key, entry in arg_spec.items():
                if 'choices' not in entry:
                    continue
                opt = doc_opts.get(key, {})
                if 'choices' not in opt:
                    failures.append('%s.%s: choices missing from DOCUMENTATION' % (modname, key))
                elif sorted(str(c) for c in opt['choices']) != sorted(str(c) for c in entry['choices']):
                    failures.append('%s.%s: DOCUMENTATION choices=%r but ARGUMENT_SPEC choices=%r' % (
                        modname, key, opt['choices'], entry['choices']))
        assert not failures, 'DOCUMENTATION/ARGUMENT_SPEC choices mismatch:\n' + '\n'.join(failures)

    def test_examples_only_use_declared_parameters(self, generated_sources):
        """Every parameter in EXAMPLES must be declared in ARGUMENT_SPEC (or be an auth/builtin param)."""
        failures = []
        for modname, source in generated_sources.items():
            arg_spec = self._extract_argument_spec(source)
            examples = self._extract_examples(source)
            if arg_spec is None or not isinstance(examples, list):
                continue
            valid_params = set(arg_spec.keys()) | self._SKIP_PARAMS
            for task in examples:
                if not isinstance(task, dict):
                    continue
                for task_key, task_val in task.items():
                    if not isinstance(task_val, dict):
                        continue
                    for param in task_val:
                        if param not in valid_params:
                            failures.append('%s: example uses undeclared param %r' % (modname, param))
        assert not failures, 'Examples use undeclared parameters:\n' + '\n'.join(sorted(set(failures)))
