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
    classify_path,
    extract_id_param,
    detect_nested_module_name,
)
from backends.ansible import (
    tag_to_module_name,
    openapi_type_to_ansible,
    schema_to_argument_spec,
    generate,
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
