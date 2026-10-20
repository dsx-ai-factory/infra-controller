# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Ansible collection backend for the unified code generator."""

from __future__ import absolute_import, division, print_function
import glob
import os
import re
import sys
import textwrap
import yaml

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from common import (
    camel_to_snake, resolve_ref, resolve_refs_recursive,
    classify_path, extract_id_param, detect_nested_resource_key,
    group_paths_by_tag, analyze_resource, resolve_composition,
)
detect_nested_module_name = detect_nested_resource_key  # compat alias used below
from backends.ansible_config import (
    RESOURCE_OVERRIDES, READ_ONLY_TAGS, SKIP_TAGS,
    TAG_TO_MODULE, SKIP_PATHS, BATCH_MODULES, INFO_ONLY_MODULES, ACTION_MODULES,
)

_PATH_PARAM_RE = re.compile(r'\{(\w+)\}')


def tag_to_module_name(tag):
    """Convert an OpenAPI tag name to a snake_case Ansible module name.

    Strips non-identifier characters so the result is always a valid Python
    identifier (required for importable module files).
    """
    if tag in TAG_TO_MODULE:
        return TAG_TO_MODULE[tag]
    name = camel_to_snake(tag.replace(' ', ''))
    # Replace any character that is not a letter, digit, or underscore.
    import re as _re
    name = _re.sub(r'[^a-z0-9_]', '_', name).strip('_')
    return name


# ---------------------------------------------------------------------------
# OpenAPI schema -> Ansible argument_spec conversion
# ---------------------------------------------------------------------------

# Private alias kept for internal call sites; resolve_composition is the public name.
_resolve_composition = resolve_composition


def openapi_type_to_ansible(schema, spec):
    """Convert an OpenAPI property schema to an Ansible argument_spec entry."""
    if not schema:
        return {'type': 'str'}

    if '$ref' in schema:
        schema = resolve_refs_recursive(spec, schema)

    # Resolve oneOf/allOf composition by merging all variants' properties.
    if 'oneOf' in schema or 'allOf' in schema:
        merged = _resolve_composition(schema, spec)
        if merged:
            return openapi_type_to_ansible(merged, spec)

    schema_type = schema.get('type', 'string')
    if isinstance(schema_type, list):
        non_null = [t for t in schema_type if t != 'null']
        schema_type = non_null[0] if non_null else 'string'

    if schema_type == 'string' and 'enum' in schema:
        # Coerce all enum values to strings. JSON booleans (true/false) become
        # lowercase strings to match what the API expects over the wire.
        choices = [str(v).lower() if isinstance(v, bool) else str(v) if not isinstance(v, str) else v
                   for v in schema['enum']]
        return {'type': 'str', 'choices': choices}

    type_map = {
        'string': 'str',
        'integer': 'int',
        'boolean': 'bool',
        'number': 'float',
    }

    if schema_type in type_map:
        return {'type': type_map[schema_type]}

    if schema_type == 'object' and 'additionalProperties' in schema:
        return {'type': 'dict'}

    if schema_type == 'object' and 'properties' in schema:
        suboptions = {}
        required_fields = schema.get('required', [])
        for prop_name, prop_schema in schema.get('properties', {}).items():
            if prop_schema.get('readOnly'):
                continue
            entry = openapi_type_to_ansible(prop_schema, spec)
            entry_snake = camel_to_snake(prop_name)
            # Skip argument names reserved by Ansible Core Engine.  The list
            # lives in ansible-test's validate-modules/main.py; we replicate
            # it here so the generator never produces a module that fails sanity.
            if entry_snake in ('message', 'syslog_facility'):
                continue
            if prop_name in required_fields:
                entry['required'] = True
            if prop_schema.get('writeOnly') or _is_secret_field(entry_snake):
                entry['no_log'] = True
            suboptions[entry_snake] = entry
        if suboptions:
            return {'type': 'dict', 'options': suboptions}
        return {'type': 'dict'}

    if schema_type == 'array':
        items_schema = schema.get('items', {})
        if '$ref' in items_schema:
            items_schema = resolve_refs_recursive(spec, items_schema)

        items_type = items_schema.get('type', 'string')
        if isinstance(items_type, list):
            non_null = [t for t in items_type if t != 'null']
            items_type = non_null[0] if non_null else 'string'

        if items_type in ('string', 'integer', 'boolean', 'number'):
            return {'type': 'list', 'elements': type_map.get(items_type, 'str')}

        if items_type == 'object' or 'properties' in items_schema:
            sub = openapi_type_to_ansible(items_schema, spec)
            result = {'type': 'list', 'elements': 'dict'}
            if 'options' in sub:
                result['options'] = sub['options']
            return result

        return {'type': 'list', 'elements': 'str'}

    return {'type': 'str'}


_SECRET_FIELD_PATTERNS = re.compile(
    r'(^|_)(password|passwd|secret|token|api_key|auth_token|private_key|key)(_|$)',
    re.IGNORECASE,
)


def _is_secret_field(snake_name):
    """Return True if the field name indicates a secret value that should not be logged."""
    return bool(_SECRET_FIELD_PATTERNS.search(snake_name))


def _collect_nested_opaque_names(schema, spec=None, _depth=0):
    """Return snake_case names of properties at any nesting level that are
    themselves freeform maps (have additionalProperties).  Resolves $ref so
    that ref-wrapped additionalProperties schemas are correctly detected."""
    if not isinstance(schema, dict) or _depth > 8:
        return set()
    names = set()
    for fname, fschema in schema.get('properties', {}).items():
        if not isinstance(fschema, dict):
            continue
        if '$ref' in fschema and spec:
            fschema = resolve_refs_recursive(spec, fschema)
        if 'additionalProperties' in fschema:
            names.add(camel_to_snake(fname))
        names.update(_collect_nested_opaque_names(fschema, spec, _depth + 1))
        items = fschema.get('items')
        if isinstance(items, dict):
            names.update(_collect_nested_opaque_names(items, spec, _depth + 1))
    return names


def schema_to_argument_spec(schema, spec, include_fields=None):
    """Convert an OpenAPI request schema to an Ansible argument_spec dict.

    Returns (arg_spec, field_map, opaque_fields) where:
    - field_map maps snake_case arg names to original OpenAPI camelCase names
      for acronym-heavy fields (e.g. fallbackDPUSerialNumbers).
    - opaque_fields is a set of snake_case field names whose values are
      additionalProperties maps with user-defined keys that must not be
      key-converted (e.g. labels, annotations, values).
    """
    if not schema:
        return {}, {}, set()

    # Resolve top-level oneOf/allOf composition before extracting properties.
    if 'oneOf' in schema or 'allOf' in schema:
        schema = _resolve_composition(schema, spec)
        if not schema:
            return {}, {}, set()

    if 'properties' not in schema:
        return {}, {}, set()

    result = {}
    field_map = {}
    opaque_fields = set()
    required_fields = schema.get('required', [])

    for prop_name, prop_schema in schema.get('properties', {}).items():
        if prop_schema.get('readOnly'):
            continue

        snake_name = camel_to_snake(prop_name)
        if include_fields and snake_name not in include_fields:
            continue
        if snake_name in ('message', 'syslog_facility'):
            continue

        # Record original camelCase name when snake_to_camel round-trip would
        # produce the wrong spelling (e.g. acronym fields like DPU, IB, SSH).
        if snake_name != prop_name:
            field_map[snake_name] = prop_name

        entry = openapi_type_to_ansible(prop_schema, spec)

        if prop_name in required_fields:
            entry['required'] = True

        if prop_schema.get('writeOnly') or _is_secret_field(snake_name):
            entry['no_log'] = True

        desc = prop_schema.get('description', '')
        if desc:
            entry['description'] = desc

        # Mark the field itself if it's a direct freeform map.
        if 'additionalProperties' in prop_schema:
            opaque_fields.add(snake_name)
        # Collect names of nested freeform maps so _to_api_body preserves
        # their child keys without marking the ancestor opaque.
        opaque_fields.update(_collect_nested_opaque_names(prop_schema, spec))

        result[snake_name] = entry

    return result, field_map, opaque_fields


# ---------------------------------------------------------------------------
# Module code generation
# ---------------------------------------------------------------------------

_MAX_LINE = 120


def format_argument_spec(arg_spec, indent=0):
    """Format an argument_spec dict as Python source code.

    Indentation scheme (all absolute column numbers):
      entry name  : (indent+1)*4 spaces
      key=value   : (indent+2)*4 spaces
      entry close : (indent+1)*4 spaces
      dict close  : indent*4 spaces  (matches the line that opens options=dict()
    """
    lines = []
    entry_pad = '    ' * (indent + 1)   # indent for entry names and their closing )
    kv_pad = '    ' * (indent + 2)      # indent for key=value lines inside each entry

    for name, entry in sorted(arg_spec.items()):
        kv_lines = []
        for key in ('type', 'default', 'required', 'no_log', 'elements'):
            if key not in entry:
                continue
            val = entry[key]
            if isinstance(val, bool):
                kv_lines.append('%s%s=%s,' % (kv_pad, key, val))
            else:
                kv_lines.append("%s%s='%s'," % (kv_pad, key, val))
        if 'choices' in entry:
            choices = entry['choices']
            inline = '%schoices=%r,' % (kv_pad, choices)
            if len(inline) <= _MAX_LINE:
                kv_lines.append(inline)
            else:
                choice_pad = kv_pad + '    '
                kv_lines.append('%schoices=[' % kv_pad)
                for c in choices:
                    kv_lines.append('%s%r,' % (choice_pad, c))
                kv_lines.append('%s],' % kv_pad)
        if 'options' in entry:
            # Suboptions: indent+2 so their entries land at (indent+3)*4 spaces,
            # and the closing ) of the suboptions dict lands at (indent+2)*4 spaces,
            # matching the kv_pad level where options= sits.
            sub_str = format_argument_spec(entry['options'], indent + 2)
            kv_lines.append('%soptions=%s,' % (kv_pad, sub_str))

        lines.append('%s%s=dict(' % (entry_pad, name))
        lines.extend(kv_lines)
        lines.append('%s),' % entry_pad)

    # The closing ) of the dict sits at indent*4 spaces, matching the line
    # that contains the opening dict( (e.g. options=dict( or ARGUMENT_SPEC = dict().
    dict_close = '    ' * indent
    if indent == 0:
        return 'dict(\n%s\n)' % '\n'.join(lines)
    else:
        return 'dict(\n%s\n%s)' % ('\n'.join(lines), dict_close)


def format_resource_config(config, indent=0):
    """Format a RESOURCE_CONFIG dict as Python source code.

    Long lists and dicts are wrapped onto multiple lines to stay within
    pep8's 160-character line limit.
    """
    lines = []
    prefix = '    '
    item_prefix = '        '  # 8 spaces for wrapped items

    for key, value in config.items():
        if isinstance(value, dict):
            if not value:
                lines.append("%s'%s': {}," % (prefix, key))
            else:
                inline = ', '.join("'%s': '%s'" % (k, v) for k, v in sorted(value.items()))
                candidate = "%s'%s': {%s}," % (prefix, key, inline)
                if len(candidate) <= _MAX_LINE:
                    lines.append(candidate)
                else:
                    lines.append("%s'%s': {" % (prefix, key))
                    for k, v in sorted(value.items()):
                        lines.append("%s'%s': '%s'," % (item_prefix, k, v))
                    lines.append("%s}," % prefix)
        elif isinstance(value, list):
            if not value:
                lines.append("%s'%s': []," % (prefix, key))
            else:
                items_str = ', '.join("'%s'" % v for v in value)
                candidate = "%s'%s': [%s]," % (prefix, key, items_str)
                if len(candidate) <= _MAX_LINE:
                    lines.append(candidate)
                else:
                    lines.append("%s'%s': [" % (prefix, key))
                    for v in value:
                        lines.append("%s'%s'," % (item_prefix, v))
                    lines.append("%s]," % prefix)
        elif isinstance(value, bool):
            lines.append("%s'%s': %s," % (prefix, key, value))
        elif value is None:
            lines.append("%s'%s': None," % (prefix, key))
        else:
            lines.append("%s'%s': '%s'," % (prefix, key, value))

    return '{\n%s\n}' % '\n'.join(lines)


def _arg_spec_entry_to_doc_opt(name, entry):
    """Recursively convert a single arg_spec entry to a DOCUMENTATION option dict."""
    opt = {'type': entry.get('type', 'str')}
    desc = entry.get('description', '')
    opt['description'] = [desc] if desc else ['%s parameter.' % name]
    if entry.get('required'):
        opt['required'] = True
    if 'default' in entry:
        opt['default'] = entry['default']
    if 'choices' in entry:
        opt['choices'] = entry['choices']
    if 'elements' in entry:
        opt['elements'] = entry['elements']
    if 'options' in entry:
        opt['suboptions'] = {
            sub_name: _arg_spec_entry_to_doc_opt(sub_name, sub_entry)
            for sub_name, sub_entry in sorted(entry['options'].items())
        }
    return opt


def generate_doc_string(module_name, tag, description, arg_spec, is_info=False, is_batch=False):
    """Generate DOCUMENTATION YAML string."""
    # Use the operation's own summary/description when available (set for nested
    # resources like vpc_routing_profile whose tag is the parent resource).
    if description:
        short_desc = description
    elif is_info:
        short_desc = 'Retrieve %s information' % tag
    elif is_batch:
        short_desc = 'Batch create %s resources' % tag
    else:
        short_desc = 'Manage %s resources' % tag

    _auth_params = {'api_url', 'api_token', 'org', 'api_path_prefix'}
    doc = {
        'module': module_name,
        'short_description': short_desc,
        'description': [description or short_desc],
        'version_added': '1.0.0',
        'author': ['NVIDIA CORPORATION (@nvidia)'],
        'extends_documentation_fragment': ['nvidia.infra_controller.auth'],
        'options': {
            name: _arg_spec_entry_to_doc_opt(name, entry)
            for name, entry in sorted(arg_spec.items())
            if name not in _auth_params
        },
    }

    return yaml.dump(doc, default_flow_style=False, sort_keys=False, width=100)


def _required_example_args(arg_spec):
    """Return sorted list of required arg names (excluding auth and state)."""
    _skip = {'api_url', 'api_token', 'org', 'api_path_prefix', 'state', 'id', 'name', 'wait', 'wait_timeout'}
    return sorted(
        name for name, entry in (arg_spec or {}).items()
        if entry.get('required') and name not in _skip
    )


def generate_examples(module_name, tag, is_info=False, is_batch=False, has_create=True, has_delete=True, arg_spec=None):
    """Generate EXAMPLES string.

    Pass ``arg_spec`` so the examples only include parameters the module actually
    declares and include placeholders for every required non-auth argument.
    """
    lines = []
    full_name = 'nvidia.infra_controller.%s' % module_name
    has_name = arg_spec is not None and 'name' in arg_spec
    required_extra = _required_example_args(arg_spec)

    def _auth_lines(indent='    '):
        return [
            '%sapi_url: "{{ api_url }}"' % indent,
            '%sapi_token: "{{ api_token }}"' % indent,
            '%sorg: "{{ org }}"' % indent,
        ]

    def _required_extra_lines(indent='    '):
        return ['%s%s: "{{ %s }}"' % (indent, a, a) for a in required_extra]

    if is_info:
        lines.append('- name: List all %s resources' % tag)
        lines.append('  %s:' % full_name)
        lines.extend(_auth_lines())
        lines.extend(_required_extra_lines())
        if arg_spec is not None and 'id' in arg_spec:
            lines.append('')
            lines.append('- name: Get a specific %s by ID' % tag)
            lines.append('  %s:' % full_name)
            lines.extend(_auth_lines())
            lines.extend(_required_extra_lines())
            lines.append('    id: "{{ resource_id }}"')
    elif is_batch:
        lines.append('- name: Batch create %s resources' % tag)
        lines.append('  %s:' % full_name)
        lines.extend(_auth_lines())
        lines.extend(_required_extra_lines())
    else:
        if has_create:
            lines.append('- name: Create a %s' % tag)
            lines.append('  %s:' % full_name)
            lines.extend(_auth_lines())
            lines.append('    state: present')
            if has_name:
                lines.append('    name: "my-%s"' % module_name.replace('_', '-'))
            lines.extend(_required_extra_lines())
            lines.append('')

        if has_delete:
            lines.append('- name: Delete a %s' % tag)
            lines.append('  %s:' % full_name)
            lines.extend(_auth_lines())
            lines.append('    state: absent')
            if has_create and has_name:
                lines.append('    name: "my-%s"' % module_name.replace('_', '-'))
            else:
                lines.append('    id: "{{ resource_id }}"')
            lines.extend(_required_extra_lines())

    return '\n'.join(lines)


def generate_return_doc(response_schema, spec, is_info=False, is_batch=False):
    """Generate RETURN documentation string."""
    if is_info:
        return textwrap.dedent('''\
            resources:
                description: List of resources.
                type: list
                returned: when no id is specified
                elements: dict
            resource:
                description: Single resource details.
                type: dict
                returned: when id is specified''')
    if is_batch:
        return textwrap.dedent('''\
            resources:
                description: List of created resources.
                type: list
                returned: always
                elements: dict''')
    return textwrap.dedent('''\
        resource:
            description: The resource details.
            type: dict
            returned: when state is present''')


def _add_path_params_to_arg_spec(arg_spec, collection_path, item_path, id_param):
    """Add required arg_spec entries for URL path parameters.

    Skips {org} (handled by auth) and the item id_param (supplied via 'id').
    Marks each remaining placeholder required=True and adds no_log when the
    name matches a secret pattern.
    """
    for path in (collection_path, item_path):
        for match in _PATH_PARAM_RE.findall(path):
            if match == 'org':
                continue
            snake_param = camel_to_snake(match)
            if id_param and camel_to_snake(id_param) == snake_param:
                continue
            no_log = _is_secret_field(snake_param)
            if snake_param not in arg_spec:
                entry = {
                    'type': 'str',
                    'required': True,
                    'description': 'ID path parameter: %s.' % snake_param,
                }
                if no_log:
                    entry['no_log'] = True
                arg_spec[snake_param] = entry
            else:
                arg_spec[snake_param] = dict(arg_spec[snake_param])
                arg_spec[snake_param]['required'] = True
                if no_log:
                    arg_spec[snake_param]['no_log'] = True


def generate_crud_module(resource_info, spec, overrides):
    """Generate the Python source for a CRUD module."""
    module_name = resource_info['module_name']
    tag = resource_info['tag']

    arg_spec = {}

    has_delete = resource_info.get('has_delete', False)
    state_choices = ['present', 'absent'] if has_delete else ['present']

    arg_spec['id'] = {'type': 'str', 'description': 'ID of the resource. Used for lookup.'}
    arg_spec['state'] = {
        'type': 'str',
        'default': 'present',
        'choices': state_choices,
        'description': 'Desired state of the resource.',
    }
    arg_spec['wait'] = {'type': 'bool', 'description': 'Wait for the resource to reach the desired state.'}
    arg_spec['wait_timeout'] = {'type': 'int', 'description': 'Timeout in seconds for wait operations.'}

    create_fields = []
    required_create_fields = []
    update_fields = []
    field_map = {}
    opaque_fields = set()

    if resource_info['create_schema']:
        create_spec, create_fm, create_of = schema_to_argument_spec(resource_info['create_schema'], spec)
        field_map.update(create_fm)
        opaque_fields.update(create_of)
        for k, v in create_spec.items():
            if k not in arg_spec:
                v = dict(v)
                if v.pop('required', False):
                    required_create_fields.append(k)
                arg_spec[k] = v
            create_fields.append(k)

    if resource_info['update_schema']:
        update_spec, update_fm, update_of = schema_to_argument_spec(resource_info['update_schema'], spec)
        field_map.update(update_fm)
        opaque_fields.update(update_of)
        for k, v in update_spec.items():
            if k not in arg_spec:
                v = dict(v)
                v.pop('required', None)
                arg_spec[k] = v
            update_fields.append(k)

    delete_body_fields = overrides.get('delete_body_fields', [])
    if resource_info.get('delete_schema'):
        delete_spec, delete_fm, delete_of = schema_to_argument_spec(resource_info['delete_schema'], spec)
        field_map.update(delete_fm)
        opaque_fields.update(delete_of)
        for k, v in delete_spec.items():
            if k not in arg_spec:
                v = dict(v)
                v.pop('required', None)
                arg_spec[k] = v
            if k not in delete_body_fields:
                delete_body_fields.append(k)

    name_field_override = overrides.get('name_field', 'UNSET')
    if name_field_override != 'UNSET':
        name_field = name_field_override
    elif 'name' in create_fields or 'name' in update_fields:
        name_field = 'name'
    else:
        name_field = None

    scope_fields = overrides.get('scope_fields', [])
    for field in scope_fields:
        if field not in arg_spec:
            arg_spec[field] = {
                'type': 'str',
                'description': 'Scope filter: %s.' % field,
            }

    collection_path = resource_info['collection_path'] or ''
    item_path = resource_info['item_path'] or ''
    _add_path_params_to_arg_spec(
        arg_spec, collection_path, item_path, resource_info.get('id_param'),
    )

    version_field = None
    if resource_info.get('update_schema'):
        update_required = resource_info['update_schema'].get('required', [])
        if 'version' in update_required:
            version_field = 'version'

    resource_config = {
        'resource_path': resource_info['collection_path'] or '',
        'resource_item_path': resource_info['item_path'] or '',
        'id_param': resource_info['id_param'] or 'id',
        'name_field': name_field,
        'create_schema_fields': create_fields,
        'update_schema_fields': update_fields,
        'update_method': resource_info.get('update_method', 'PATCH'),
        'scope_fields': scope_fields,
        'ready_statuses': overrides.get('ready_statuses', ['Ready']),
        'error_statuses': overrides.get('error_statuses', ['Error']),
        # Auto-disable create for update-only endpoints (no POST/PUT on collection path).
        'no_create': overrides.get('no_create', not resource_info.get('has_create', True)),
        # Auto-disable wait when there is no GET endpoint to poll for status.
        'no_wait': overrides.get('no_wait', not resource_info.get('has_get', False) and not resource_info.get('has_list', False)),
        'create_method': resource_info.get('create_method', 'POST'),
        'has_get_by_id': resource_info.get('has_get_by_id', True),
        'delete_body_fields': delete_body_fields,
        'version_field': version_field,
        'field_map': field_map,
        'required_create_fields': required_create_fields,
        'opaque_fields': sorted(opaque_fields),
    }

    has_create = resource_info['has_create'] and not overrides.get('no_create', False)

    # Annotate create-required fields in descriptions so the reference page
    # reflects the real API contract. Cannot mark as required=True in both
    # DOCUMENTATION and ARGUMENT_SPEC when the field is optional on update —
    # ansible-test validate-modules enforces they agree.
    arg_spec_for_docs = {}
    for k, v in arg_spec.items():
        entry = dict(v)
        if k in required_create_fields:
            existing_desc = entry.get('description', '')
            if existing_desc and not existing_desc.endswith('.'):
                existing_desc += '.'
            entry['description'] = (existing_desc + ' Required when creating a new resource.').strip()
        arg_spec_for_docs[k] = entry

    arg_spec_str = format_argument_spec(arg_spec)
    resource_config_str = format_resource_config(resource_config)
    doc_str = generate_doc_string(module_name, tag, resource_info['description'], arg_spec_for_docs)
    examples_str = generate_examples(module_name, tag, has_create=has_create, has_delete=has_delete, arg_spec=arg_spec_for_docs)
    return_str = generate_return_doc(resource_info.get('response_schema'), spec)

    code = '''\
#!/usr/bin/python
# -*- coding: utf-8 -*-
# This file is auto-generated by tools/generate/generate.py. Do not edit manually.

# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from __future__ import absolute_import, division, print_function
__metaclass__ = type

DOCUMENTATION = r\'\'\'
---
%s\'\'\'

EXAMPLES = r\'\'\'
---
%s
\'\'\'

RETURN = r\'\'\'
---
%s
\'\'\'

from ansible.module_utils.basic import AnsibleModule
from ansible_collections.nvidia.infra_controller.plugins.module_utils.common import get_auth_argument_spec
from ansible_collections.nvidia.infra_controller.plugins.module_utils.resource import CrudResource


ARGUMENT_SPEC = %s

RESOURCE_CONFIG = %s


def main():
    auth_spec = get_auth_argument_spec()
    auth_spec.update(ARGUMENT_SPEC)
    module = AnsibleModule(argument_spec=auth_spec, supports_check_mode=True)
    CrudResource(module, RESOURCE_CONFIG).run()


if __name__ == "__main__":
    main()
''' % (doc_str, examples_str, return_str, arg_spec_str, resource_config_str)

    return code, arg_spec, examples_str


def generate_info_module(resource_info, spec, overrides):
    """Generate the Python source for an _info module."""
    module_name = resource_info['module_name'] + '_info'
    tag = resource_info['tag']

    arg_spec = {}

    if resource_info['item_path']:
        arg_spec['id'] = {'type': 'str', 'description': 'ID of the resource to retrieve.'}

    filter_fields = []
    for param in resource_info.get('list_query_params', []):
        param_name = param.get('name', '')
        snake_name = camel_to_snake(param_name)
        param_schema = param.get('schema', {})

        entry = openapi_type_to_ansible(param_schema, spec)
        desc = param.get('description', '')
        if desc:
            entry['description'] = desc
        if param.get('required'):
            entry['required'] = True
        if _is_secret_field(snake_name):
            entry['no_log'] = True

        if snake_name not in arg_spec:
            arg_spec[snake_name] = entry
        elif param.get('required'):
            arg_spec[snake_name] = dict(arg_spec[snake_name])
            arg_spec[snake_name]['required'] = True
        else:
            arg_spec[snake_name] = entry
        filter_fields.append(snake_name)

    collection_path = resource_info['collection_path'] or ''
    item_path = resource_info['item_path'] or ''
    _add_path_params_to_arg_spec(
        arg_spec, collection_path, item_path, resource_info.get('id_param'),
    )

    resource_config = {
        'resource_path': collection_path,
        'resource_item_path': item_path,
        'id_param': resource_info['id_param'] or 'id',
        'filter_fields': filter_fields,
    }

    arg_spec_str = format_argument_spec(arg_spec)
    resource_config_str = format_resource_config(resource_config)
    doc_str = generate_doc_string(module_name, tag, resource_info['description'], arg_spec, is_info=True)
    examples_str = generate_examples(module_name, tag, is_info=True, arg_spec=arg_spec)
    return_str = generate_return_doc(resource_info.get('response_schema'), spec, is_info=True)

    code = '''\
#!/usr/bin/python
# -*- coding: utf-8 -*-
# This file is auto-generated by tools/generate/generate.py. Do not edit manually.

# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from __future__ import absolute_import, division, print_function
__metaclass__ = type

DOCUMENTATION = r\'\'\'
---
%s\'\'\'

EXAMPLES = r\'\'\'
---
%s
\'\'\'

RETURN = r\'\'\'
---
%s
\'\'\'

from ansible.module_utils.basic import AnsibleModule
from ansible_collections.nvidia.infra_controller.plugins.module_utils.common import get_auth_argument_spec
from ansible_collections.nvidia.infra_controller.plugins.module_utils.resource import InfoResource


ARGUMENT_SPEC = %s

RESOURCE_CONFIG = %s


def main():
    auth_spec = get_auth_argument_spec()
    auth_spec.update(ARGUMENT_SPEC)
    module = AnsibleModule(argument_spec=auth_spec, supports_check_mode=True)
    InfoResource(module, RESOURCE_CONFIG).run()


if __name__ == "__main__":
    main()
''' % (doc_str, examples_str, return_str, arg_spec_str, resource_config_str)

    return code, arg_spec, examples_str


def generate_batch_module(resource_info, spec, overrides):
    """Generate the Python source for a batch-create module."""
    module_name = resource_info['module_name']
    tag = resource_info['tag']

    arg_spec = {}
    create_fields = []
    field_map = {}

    opaque_fields = set()

    if resource_info['create_schema']:
        create_spec, create_fm, create_of = schema_to_argument_spec(resource_info['create_schema'], spec)
        field_map.update(create_fm)
        opaque_fields.update(create_of)
        for k, v in create_spec.items():
            arg_spec[k] = v
            create_fields.append(k)

    resource_config = {
        'resource_path': resource_info['collection_path'] or '',
        'create_schema_fields': create_fields,
        'field_map': field_map,
        'opaque_fields': sorted(opaque_fields),
    }

    arg_spec_str = format_argument_spec(arg_spec)
    resource_config_str = format_resource_config(resource_config)
    doc_str = generate_doc_string(module_name, tag, resource_info['description'], arg_spec, is_batch=True)
    examples_str = generate_examples(module_name, tag, is_batch=True, arg_spec=arg_spec)
    return_str = generate_return_doc(None, spec, is_batch=True)

    code = '''\
#!/usr/bin/python
# -*- coding: utf-8 -*-
# This file is auto-generated by tools/generate/generate.py. Do not edit manually.

# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from __future__ import absolute_import, division, print_function
__metaclass__ = type

DOCUMENTATION = r\'\'\'
---
%s\'\'\'

EXAMPLES = r\'\'\'
---
%s
\'\'\'

RETURN = r\'\'\'
---
%s
\'\'\'

from ansible.module_utils.basic import AnsibleModule
from ansible_collections.nvidia.infra_controller.plugins.module_utils.common import get_auth_argument_spec
from ansible_collections.nvidia.infra_controller.plugins.module_utils.resource import BatchResource


ARGUMENT_SPEC = %s

RESOURCE_CONFIG = %s


def main():
    auth_spec = get_auth_argument_spec()
    auth_spec.update(ARGUMENT_SPEC)
    module = AnsibleModule(argument_spec=auth_spec, supports_check_mode=True)
    BatchResource(module, RESOURCE_CONFIG).run()


if __name__ == "__main__":
    main()
''' % (doc_str, examples_str, return_str, arg_spec_str, resource_config_str)

    return code, arg_spec, examples_str


def generate_action_module(module_name, action_config, spec):
    """Generate the Python source for an action module (validation, power, firmware)."""
    tag = action_config['tag']
    method = action_config['method']
    collection_path = action_config['collection_path']
    item_path = action_config.get('item_path')

    arg_spec = {}
    body_fields = []
    query_fields = []

    arg_spec['id'] = {'type': 'str', 'description': 'ID of the resource. When provided, targets a single resource.'}

    field_map = {}
    opaque_fields = set()
    required_body_fields = []

    if method == 'GET':
        _skip_params = {'pageNumber', 'pageSize', 'orderBy', 'includeRelation'}
        for path_key in filter(None, [collection_path, item_path]):
            for param in spec.get('paths', {}).get(path_key, {}).get('get', {}).get('parameters', []):
                if param.get('in') != 'query' or param.get('name') in _skip_params:
                    continue
                pname = param.get('name', '')
                snake = camel_to_snake(pname)
                if snake not in arg_spec:
                    entry = {'type': 'str', 'description': param.get('description', '')}
                    if param.get('required'):
                        entry['required'] = True
                    if snake != pname:
                        field_map[snake] = pname
                    arg_spec[snake] = entry
                if snake not in query_fields:
                    query_fields.append(snake)

    if method != 'GET':
        item_ref = action_config.get('item_request_schema_ref')
        if item_ref:
            item_schema = resolve_ref(spec, item_ref)
            item_schema = resolve_refs_recursive(spec, item_schema)
            item_spec, item_fm, item_of = schema_to_argument_spec(item_schema, spec)
            field_map.update(item_fm)
            opaque_fields.update(item_of)
            for k, v in item_spec.items():
                if k not in arg_spec:
                    v = dict(v)
                    if v.pop('required', False):
                        required_body_fields.append(k)
                    arg_spec[k] = v
                body_fields.append(k)

        collection_ref = action_config.get('collection_request_schema_ref')
        if collection_ref:
            coll_schema = resolve_ref(spec, collection_ref)
            coll_schema = resolve_refs_recursive(spec, coll_schema)
            coll_spec, coll_fm, coll_of = schema_to_argument_spec(coll_schema, spec)
            field_map.update(coll_fm)
            opaque_fields.update(coll_of)
            for k, v in coll_spec.items():
                if k not in arg_spec:
                    v = dict(v)
                    if v.pop('required', False):
                        required_body_fields.append(k)
                    arg_spec[k] = v
                if k not in body_fields:
                    body_fields.append(k)

    resource_config = {
        'resource_path': collection_path,
        'resource_item_path': item_path or '',
        'method': method,
        'body_fields': body_fields,
        'query_fields': query_fields,
        'field_map': field_map,
        'opaque_fields': sorted(opaque_fields),
        'required_body_fields': required_body_fields,
    }

    description = ''
    for t in spec.get('tags', []):
        if t.get('name') == tag:
            description = t.get('description', '').split('\n\n')[0].strip()
            break

    action_type = module_name.split('_')[-1]
    short_desc = '%s %s for %s resources' % (
        action_type.capitalize(),
        'check' if method == 'GET' else 'action',
        tag,
    )

    arg_spec_str = format_argument_spec(arg_spec)
    resource_config_str = format_resource_config(resource_config)
    arg_spec_for_docs = {}
    for k, v in arg_spec.items():
        entry = dict(v)
        if k in required_body_fields:
            existing_desc = entry.get('description', '')
            if existing_desc and not existing_desc.endswith('.'):
                existing_desc += '.'
            entry['description'] = (existing_desc + ' Required for this action.').strip()
        arg_spec_for_docs[k] = entry
    doc_str = generate_doc_string(module_name, tag, description or short_desc, arg_spec_for_docs)
    return_str = textwrap.dedent('''\
        result:
            description: The action result.
            type: dict
            returned: always''')

    full_name = 'nvidia.infra_controller.%s' % module_name
    req_body_lines = ['    %s: "{{ %s }}"' % (f, f) for f in sorted(required_body_fields)]
    examples_lines = []
    examples_lines.append('- name: Run %s on all %s resources' % (action_type, tag.lower()))
    examples_lines.append('  %s:' % full_name)
    examples_lines.append('    api_url: "{{ api_url }}"')
    examples_lines.append('    api_token: "{{ api_token }}"')
    examples_lines.append('    org: "{{ org }}"')
    examples_lines.extend(req_body_lines)
    examples_lines.append('')
    examples_lines.append('- name: Run %s on a specific %s' % (action_type, tag.lower()))
    examples_lines.append('  %s:' % full_name)
    examples_lines.append('    api_url: "{{ api_url }}"')
    examples_lines.append('    api_token: "{{ api_token }}"')
    examples_lines.append('    org: "{{ org }}"')
    examples_lines.append('    id: "{{ resource_id }}"')
    examples_lines.extend(req_body_lines)
    examples_str = '\n'.join(examples_lines)

    supports_check = 'True'

    code = '''\
#!/usr/bin/python
# -*- coding: utf-8 -*-
# This file is auto-generated by tools/generate/generate.py. Do not edit manually.

# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from __future__ import absolute_import, division, print_function
__metaclass__ = type

DOCUMENTATION = r\'\'\'
---
%s\'\'\'

EXAMPLES = r\'\'\'
---
%s
\'\'\'

RETURN = r\'\'\'
---
%s
\'\'\'

from ansible.module_utils.basic import AnsibleModule
from ansible_collections.nvidia.infra_controller.plugins.module_utils.common import get_auth_argument_spec
from ansible_collections.nvidia.infra_controller.plugins.module_utils.resource import ActionResource


ARGUMENT_SPEC = %s

RESOURCE_CONFIG = %s


def main():
    auth_spec = get_auth_argument_spec()
    auth_spec.update(ARGUMENT_SPEC)
    module = AnsibleModule(argument_spec=auth_spec, supports_check_mode=%s)
    ActionResource(module, RESOURCE_CONFIG).run()


if __name__ == "__main__":
    main()
''' % (doc_str, examples_str, return_str, arg_spec_str, resource_config_str, supports_check)

    return code, arg_spec, examples_str


# ---------------------------------------------------------------------------
# Markdown doc generation (for NICo docs site via Fern)
# ---------------------------------------------------------------------------

def _arg_spec_to_md_table(arg_spec):
    """Render an argument_spec dict as a Markdown parameter table."""
    rows = []
    for name in sorted(arg_spec):
        entry = arg_spec[name]
        if name in ('api_url', 'api_token', 'org'):
            continue
        typ = entry.get('type', 'str')
        required = 'Yes' if entry.get('required') else 'No'
        desc = entry.get('description', '')
        if isinstance(desc, list):
            desc = ' '.join(desc)
        # Normalize whitespace and escape Markdown table delimiters
        desc = ' '.join(desc.split()).replace('|', '\\|')
        choices = entry.get('choices')
        if choices:
            desc = ('%s Choices: `%s`.' % (desc, '`, `'.join(str(c) for c in choices))).strip()
        rows.append('| `%s` | `%s` | %s | %s |' % (name, typ, required, desc))
    if not rows:
        return ''
    header = '| Parameter | Type | Required | Description |\n|-----------|------|----------|-------------|'
    return header + '\n' + '\n'.join(rows)


def generate_module_doc_md(module_name, tag, description, arg_spec, examples, is_info=False, is_batch=False):
    """Render a module's documentation as a Markdown page for the NICo docs site."""
    full_name = 'nvidia.infra_controller.%s' % module_name
    if is_info:
        title = '%s – query %s' % (full_name, tag)
    elif is_batch:
        title = '%s – batch create %s' % (full_name, tag)
    else:
        title = '%s – manage %s' % (full_name, tag)

    params_table = _arg_spec_to_md_table(arg_spec)

    lines = [
        '# %s' % title,
        '',
        description or ('Manage %s resources.' % tag),
        '',
        '## Parameters',
        '',
    ]
    if params_table:
        lines += [params_table, '']
    else:
        lines += ['No additional parameters beyond the [authentication fragment](../getting-started.md).', '']

    lines += [
        '## Examples',
        '',
        '```yaml',
        examples,
        '```',
        '',
    ]
    return '\n'.join(lines)


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------

def generate(spec, output_dir, dry_run=False, docs_output=None, root=None, **kwargs):
    """Generate all Ansible modules from the OpenAPI spec.

    When ``root`` is provided (the repository root path), spec_version in
    ansible/galaxy.yml is kept in sync with the OpenAPI spec version.
    """
    spec_version = spec.get('info', {}).get('version', 'unknown')
    print('Spec version: %s' % spec_version)

    # Update spec_version in ansible/galaxy.yml only when root is explicitly supplied.
    if root and not dry_run:
        galaxy_path = os.path.join(root, 'ansible', 'galaxy.yml')
        if os.path.exists(galaxy_path):
            with open(galaxy_path, 'r') as f:
                galaxy_content = f.read()
            if 'spec_version:' in galaxy_content:
                galaxy_content = re.sub(
                    r'spec_version:.*',
                    'spec_version: "%s"' % spec_version,
                    galaxy_content,
                )
            else:
                galaxy_content = galaxy_content.rstrip() + '\nspec_version: "%s"\n' % spec_version
            with open(galaxy_path, 'w') as f:
                f.write(galaxy_content)
            print('Updated galaxy.yml spec_version to %s' % spec_version)

    groups = group_paths_by_tag(spec, SKIP_PATHS, SKIP_TAGS, tag_to_module_name)

    if not dry_run:
        os.makedirs(output_dir, exist_ok=True)
        # Remove modules left over from prior generation runs — prevents the
        # collection build from picking up deleted or renamed modules.
        for stale in glob.glob(os.path.join(output_dir, '*.py')):
            os.remove(stale)
        if docs_output:
            os.makedirs(docs_output, exist_ok=True)

    generated = []
    # module_name -> (arg_spec, tag, description, is_info, is_batch, examples_str)
    module_meta = {}

    for _group_key, group in sorted(groups.items()):
        tag = group['tag']
        module_name = group['module_name']
        overrides = RESOURCE_OVERRIDES.get(module_name, {})
        is_read_only = tag in READ_ONLY_TAGS

        resource_info = analyze_resource(tag, group, spec)
        desc = resource_info.get('description', '')
        module_type = overrides.get('module_type', None)

        if module_type == 'batch':
            code, arg_spec, examples_str = generate_batch_module(resource_info, spec, overrides)
            filename = '%s.py' % module_name
            if dry_run:
                print('  [batch] %s' % filename)
            else:
                filepath = os.path.join(output_dir, filename)
                with open(filepath, 'w') as f:
                    f.write(code)
                print('  Generated %s' % filepath)
            generated.append(filename)
            module_meta[module_name] = (arg_spec, tag, desc, False, True, examples_str)

        elif is_read_only:
            code, arg_spec, examples_str = generate_info_module(resource_info, spec, overrides)
            filename = '%s_info.py' % module_name
            if dry_run:
                print('  [info-only] %s' % filename)
            else:
                filepath = os.path.join(output_dir, filename)
                with open(filepath, 'w') as f:
                    f.write(code)
                print('  Generated %s' % filepath)
            generated.append(filename)
            module_meta[module_name + '_info'] = (arg_spec, tag, desc, True, False, examples_str)

        else:
            if resource_info['has_create'] or resource_info['has_update'] or resource_info['has_delete']:
                code, arg_spec, examples_str = generate_crud_module(resource_info, spec, overrides)
                filename = '%s.py' % module_name
                if dry_run:
                    print('  [crud] %s' % filename)
                else:
                    filepath = os.path.join(output_dir, filename)
                    with open(filepath, 'w') as f:
                        f.write(code)
                    print('  Generated %s' % filepath)
                generated.append(filename)
                module_meta[module_name] = (arg_spec, tag, desc, False, False, examples_str)

            if resource_info['has_list'] or resource_info['has_get']:
                code, arg_spec, examples_str = generate_info_module(resource_info, spec, overrides)
                filename = '%s_info.py' % module_name
                if dry_run:
                    print('  [info] %s' % filename)
                else:
                    filepath = os.path.join(output_dir, filename)
                    with open(filepath, 'w') as f:
                        f.write(code)
                    print('  Generated %s' % filepath)
                generated.append(filename)
                module_meta[module_name + '_info'] = (arg_spec, tag, desc, True, False, examples_str)

    # Generate batch modules (special cases not covered by tag grouping)
    for batch_module_name, batch_config in BATCH_MODULES.items():
        batch_path = batch_config['path']
        batch_tag = batch_config['tag']
        schema_ref = batch_config['schema_ref']

        batch_schema = resolve_ref(spec, schema_ref)
        batch_schema = resolve_refs_recursive(spec, batch_schema)

        batch_desc = ''
        for t in spec.get('tags', []):
            if t.get('name') == batch_tag:
                batch_desc = t.get('description', '').split('\n\n')[0].strip()
                break

        batch_resource_info = {
            'module_name': batch_module_name,
            'tag': batch_tag,
            'collection_path': batch_path,
            'create_schema': batch_schema,
            'description': 'Batch create %s' % batch_tag,
        }

        overrides = RESOURCE_OVERRIDES.get(batch_module_name, {})
        code, arg_spec, examples_str = generate_batch_module(batch_resource_info, spec, overrides)
        filename = '%s.py' % batch_module_name
        if dry_run:
            print('  [batch] %s' % filename)
        else:
            filepath = os.path.join(output_dir, filename)
            with open(filepath, 'w') as f:
                f.write(code)
            print('  Generated %s' % filepath)
        generated.append(filename)
        module_meta[batch_module_name] = (arg_spec, batch_tag, batch_desc, False, True, examples_str)

    # Generate info-only modules for mis-tagged or special endpoints
    for info_module_name, info_config in INFO_ONLY_MODULES.items():
        info_tag = info_config['tag']
        collection_path = info_config['collection_path']
        response_ref = info_config.get('response_schema_ref')

        info_desc = ''
        for t in spec.get('tags', []):
            if t.get('name') == info_tag:
                info_desc = t.get('description', '').split('\n\n')[0].strip()
                break

        response_schema = None
        if response_ref:
            response_schema = resolve_ref(spec, response_ref)
            response_schema = resolve_refs_recursive(spec, response_schema)

        info_resource_info = {
            'module_name': info_module_name,
            'tag': info_tag,
            'collection_path': collection_path,
            'item_path': info_config.get('item_path'),
            'id_param': info_config.get('id_param'),
            'has_list': True,
            'has_get': bool(info_config.get('item_path')),
            'list_query_params': [],
            'response_schema': response_schema,
            'description': info_desc or '%s operations' % info_tag,
        }

        overrides = RESOURCE_OVERRIDES.get(info_module_name, {})
        code, arg_spec, examples_str = generate_info_module(info_resource_info, spec, overrides)
        filename = '%s_info.py' % info_module_name
        if dry_run:
            print('  [info-special] %s' % filename)
        else:
            filepath = os.path.join(output_dir, filename)
            with open(filepath, 'w') as f:
                f.write(code)
            print('  Generated %s' % filepath)
        generated.append(filename)
        module_meta[info_module_name + '_info'] = (arg_spec, info_tag, info_desc, True, False, examples_str)

    # Generate action modules (validation, power, firmware)
    for action_module_name, action_config in ACTION_MODULES.items():
        code, arg_spec, examples_str = generate_action_module(action_module_name, action_config, spec)
        filename = '%s.py' % action_module_name
        if dry_run:
            print('  [action] %s' % filename)
        else:
            filepath = os.path.join(output_dir, filename)
            with open(filepath, 'w') as f:
                f.write(code)
            print('  Generated %s' % filepath)
        generated.append(filename)
        module_meta[action_module_name] = (arg_spec, action_config['tag'], '', False, False, examples_str)

    # Collect doc pages — use the arg_spec captured during module generation so
    # docs cannot drift from what the modules actually expose.
    doc_pages = []
    for mod_name, (arg_spec, tag, desc, is_info, is_batch, examples_str) in sorted(module_meta.items()):
        md = generate_module_doc_md(mod_name, tag, desc, arg_spec, examples_str, is_info=is_info, is_batch=is_batch)
        doc_pages.append((mod_name, md))

    if docs_output and not dry_run:
        for stale in glob.glob(os.path.join(docs_output, '*.md')):
            if os.path.basename(stale) != 'index.md':
                os.remove(stale)
        for mod_name, md_content in doc_pages:
            doc_path = os.path.join(docs_output, '%s.md' % mod_name)
            with open(doc_path, 'w') as f:
                f.write(md_content)
        print('\nGenerated %d doc pages in %s.' % (len(doc_pages), docs_output))

    # Update action_groups.all in runtime.yml to match generated modules.
    # Use text substitution to preserve the SPDX header and all formatting.
    if root and not dry_run:
        runtime_path = os.path.join(root, 'ansible', 'meta', 'runtime.yml')
        if os.path.exists(runtime_path):
            with open(runtime_path, 'r') as f:
                content = f.read()
            module_names = sorted(os.path.splitext(g)[0] for g in generated)
            all_lines = ['    all:'] + ['      - %s' % n for n in module_names]
            new_all_block = '\n'.join(all_lines)
            matched = [False]

            def _replace_all_block(m):
                matched[0] = True
                return new_all_block

            new_content = re.sub(
                r'(?<=action_groups:\n)(\s*)all:.*?(?=\n\S|\Z)',
                _replace_all_block,
                content,
                flags=re.DOTALL,
            )
            if not matched[0]:
                # Block not present — append it rather than relying on content equality.
                new_content = content.rstrip('\n') + '\naction_groups:\n' + new_all_block + '\n'
            if new_content != content:
                with open(runtime_path, 'w') as f:
                    f.write(new_content)
                print('Updated action_groups in %s (%d modules).' % (runtime_path, len(module_names)))

    # Generate tests/sanity/ignore.txt so it stays in sync with
    # the generated module set.  The file uses no comments or blank lines.
    if root and not dry_run:
        ignore_path = os.path.join(root, 'ansible', 'tests', 'sanity', 'ignore.txt')
        lines = []
        for filename in sorted(generated):
            lines.append('plugins/modules/%s validate-modules:missing-gplv3-license' % filename)
        with open(ignore_path, 'w') as f:
            f.write('\n'.join(sorted(lines)) + '\n')
        print('Updated %s (%d entries).' % (ignore_path, len(lines)))

    print('\nGenerated %d module files.' % len(generated))
    return generated
