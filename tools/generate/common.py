# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Shared spec-walking utilities for the unified code generator."""

import re
import yaml


_PATH_PARAM_RE = re.compile(r'\{(\w+)\}')


def load_spec(spec_path):
    with open(spec_path, 'r') as f:
        return yaml.safe_load(f)


def camel_to_snake(name):
    """Convert camelCase to snake_case."""
    s1 = re.sub(r'([A-Z]+)([A-Z][a-z])', r'\1_\2', name)
    s2 = re.sub(r'([a-z0-9])([A-Z])', r'\1_\2', s1)
    return s2.lower()


def resolve_ref(spec, ref):
    """Resolve a $ref string to the referenced object in the spec."""
    if not ref.startswith('#/'):
        return {}
    parts = ref.lstrip('#/').split('/')
    obj = spec
    for part in parts:
        if isinstance(obj, dict):
            obj = obj.get(part, {})
        else:
            return {}
    return obj


def resolve_refs_recursive(spec, obj, depth=0):
    """Recursively resolve all $ref in an object."""
    if depth > 20:
        return obj
    if isinstance(obj, dict):
        if '$ref' in obj:
            referenced = resolve_ref(spec, obj['$ref'])
            merged = dict(resolve_refs_recursive(spec, referenced, depth + 1))
            for k, v in obj.items():
                if k != '$ref':
                    merged[k] = v
            return merged
        return {k: resolve_refs_recursive(spec, v, depth + 1) for k, v in obj.items()}
    if isinstance(obj, list):
        return [resolve_refs_recursive(spec, item, depth + 1) for item in obj]
    return obj


def classify_path(path):
    """Classify a path as 'collection' or 'item'."""
    last_segment = path.rstrip('/').split('/')[-1]
    if last_segment.startswith('{') and last_segment.endswith('}'):
        return 'item'
    return 'collection'


def extract_id_param(path):
    """Extract the ID parameter name from an item path."""
    last_segment = path.rstrip('/').split('/')[-1]
    if last_segment.startswith('{') and last_segment.endswith('}'):
        return last_segment[1:-1]
    return None


def resolve_composition(schema, spec):
    """Merge oneOf/allOf variants into a flat schema with combined properties.

    Pure OpenAPI logic — backend-agnostic.  Handles four cases:
    - All non-null variants are objects → merge their properties.
    - Non-null variants are freeform maps (``additionalProperties`` without
      ``properties``, e.g. ``oneOf: [$ref Labels, null]``) → keep the merged
      ``additionalProperties`` so callers see a map type instead of a scalar.
    - Heterogeneous variants (e.g. object + string) → return ``{'type': 'raw'}``.
    - No object variants → return None (caller falls back to a scalar type).

    ``allOf`` branches are conjunctive, so their ``required`` lists are
    unioned into the merged schema.  ``oneOf`` branches are alternatives:
    flattening must not make a branch-specific field globally required (a
    valid payload provides only one branch's fields), so the merged schema
    keeps only the top-level ``required`` list and records each branch's
    remaining required fields under ``x-oneof-required`` for callers that
    validate the exactly-one-alternative contract.
    """
    variants = []
    for v in schema.get('oneOf', schema.get('allOf', [])):
        if '$ref' in v:
            v = resolve_refs_recursive(spec, v)
        variants.append(v)

    non_null = [v for v in variants if v.get('type') != 'null' and v != {'nullable': True}]
    non_obj = [v for v in non_null if v.get('type') not in (None, 'object') and 'properties' not in v]

    if non_obj:
        return {'type': 'raw'}

    is_oneof = 'oneOf' in schema
    # Seed with the schema's own properties — they carry the documented
    # descriptions — and only add properties that appear solely in a variant.
    merged_properties = dict(schema.get('properties', {}))
    merged_required = list(schema.get('required', []))
    branch_required = []
    merged_additional = None
    for variant in non_null:
        for pname, pschema in variant.get('properties', {}).items():
            merged_properties.setdefault(pname, pschema)
        if is_oneof:
            branch_required.append(list(variant.get('required', [])))
        else:
            merged_required.extend(variant.get('required', []))
        if 'additionalProperties' in variant:
            merged_additional = variant['additionalProperties']
    if not merged_properties and merged_additional is None:
        return None
    merged = {k: v for k, v in schema.items() if k not in ('oneOf', 'allOf')}
    merged['type'] = 'object'
    if merged_properties:
        merged['properties'] = merged_properties
        merged['required'] = list(set(merged_required))
        if is_oneof:
            alternatives = []
            # A branch whose required fields are all top-level required is a
            # valid base alternative: a create omitting every branch-specific
            # field still satisfies the schema.  Emitting the remaining
            # branches would make the module reject that base create, so a
            # schema with such a branch carries no alternatives and leaves
            # the exactly-one-alternative validation to the REST API.
            has_base_branch = False
            for required in branch_required:
                alternative = [p for p in required if p not in merged_required]
                if alternative:
                    alternatives.append(alternative)
                else:
                    has_base_branch = True
            if alternatives and not has_base_branch:
                merged['x-oneof-required'] = alternatives
    if merged_additional is not None:
        merged['additionalProperties'] = merged_additional
    return merged


def detect_nested_resource_key(path):
    """Detect the nested resource key from a path.

    e.g., /v2/org/{org}/nico/allocation/{allocationId}/constraint -> allocation_constraint

    Returns the snake_case resource key (backend applies its own naming convention),
    or None if the path is not a nested resource.
    """
    prefix = '/v2/org/{org}/nico/'
    if not path.startswith(prefix):
        return None
    rest = path[len(prefix):]
    segments = rest.split('/')
    resource_parts = [s for s in segments if not s.startswith('{')]
    resource_parts = [p for p in resource_parts if p != 'current']
    if len(resource_parts) >= 2:
        return '_'.join(camel_to_snake(p).replace('-', '_') for p in resource_parts)
    return None


# Backward-compat alias — prefer detect_nested_resource_key in new code.
detect_nested_module_name = detect_nested_resource_key


def group_paths_by_tag(spec, skip_paths, skip_tags, tag_to_name_fn):
    """Group API paths by their tag, returning a dict of tag -> operations.

    Detects nested resources (e.g., allocation constraint) and splits them
    into separate groups with their own module names.
    """
    groups = {}
    paths = spec.get('paths', {})

    for path, path_item in paths.items():
        if path in skip_paths:
            continue

        shared_params = path_item.get('parameters', [])

        for method in ('get', 'post', 'patch', 'put', 'delete'):
            operation = path_item.get(method)
            if not operation:
                continue

            tags = operation.get('tags', [])
            if not tags:
                continue

            tag = tags[0]
            if tag in skip_tags:
                continue

            base_module = tag_to_name_fn(tag)
            nested_name = detect_nested_resource_key(path)

            if nested_name and nested_name != base_module:
                group_key = nested_name
                if group_key not in groups:
                    groups[group_key] = {
                        'operations': [],
                        'tag': tag,
                        'module_name': nested_name,
                    }
            else:
                group_key = tag
                if group_key not in groups:
                    groups[group_key] = {
                        'operations': [],
                        'tag': tag,
                        'module_name': base_module,
                    }

            groups[group_key]['operations'].append({
                'method': method.upper(),
                'path': path,
                'path_type': classify_path(path),
                'operation': operation,
                'id_param': extract_id_param(path),
                'shared_params': shared_params,
            })

    return groups


def _extract_success_response_schema(operation, spec):
    """Return the JSON body schema for the first 2xx response, or None."""
    for status in ('200', '201', '202'):
        resp = operation.get('responses', {}).get(status, {})
        content = resp.get('content', {}).get('application/json', {})
        schema = content.get('schema', {})
        if schema:
            if '$ref' in schema:
                schema = resolve_refs_recursive(spec, schema)
            return schema
    return None


def analyze_resource(tag, group, spec):
    """Analyze a resource group and return an API-level facts dict.

    Keys: ``has_create``, ``has_update``, ``has_delete``, ``has_list``,
    ``has_get``, ``has_get_by_id``, ``create_method``, ``update_method``,
    ``create_schema``, ``update_schema``, ``delete_schema``,
    ``response_schema``, ``create_response_schema``, ``update_response_schema``,
    ``delete_response_schema``, ``path_parameters``, ``list_query_params``,
    ``item_query_params``, ``delete_query_params``, ``collection_path``,
    ``item_path``, ``id_param``, ``description``.
    """
    module_name = group['module_name']
    operations = group['operations']

    resource_info = {
        'module_name': module_name,
        'tag': tag,
        'is_read_only': False,
        'collection_path': None,
        'item_path': None,
        'id_param': None,
        'has_create': False,
        'create_method': 'POST',
        'has_update': False,
        'has_delete': False,
        'has_list': False,
        'has_get': False,
        'has_get_by_id': False,
        'create_schema': None,
        'update_schema': None,
        'update_method': 'PATCH',
        'update_summary': '',
        'delete_schema': None,
        'response_schema': None,
        'list_query_params': [],
        'item_query_params': [],
        'delete_query_params': [],
        'description': '',
        'create_summary': '',
        'create_response_schema': None,
        'update_response_schema': None,
        'delete_response_schema': None,
        'path_parameters': [],
    }

    for op in operations:
        method = op['method']
        path = op['path']
        path_type = op['path_type']
        operation = op['operation']

        # Collect path parameters from all operations (deduplicated).
        all_params = op.get('shared_params', []) + operation.get('parameters', [])
        for param in all_params:
            if param.get('in') == 'path' and param.get('name') != 'org':
                name = param['name']
                if not any(p['name'] == name for p in resource_info['path_parameters']):
                    resource_info['path_parameters'].append({
                        'name': name,
                        'snake_name': camel_to_snake(name),
                        'required': param.get('required', True),
                        'schema': param.get('schema', {'type': 'string'}),
                    })

        if not resource_info['description']:
            # Prefer the operation's own summary/description for nested resources.
            op_desc = operation.get('summary') or operation.get('description', '')
            if op_desc:
                resource_info['description'] = op_desc.split('\n\n')[0].strip()
            else:
                for t in spec.get('tags', []):
                    if t.get('name') == tag:
                        desc = t.get('description', '')
                        resource_info['description'] = desc.split('\n\n')[0].strip()
                        break

        if path_type == 'collection':
            resource_info['collection_path'] = path

            if method == 'GET':
                resource_info['has_list'] = True
                for param in operation.get('parameters', []):
                    if param.get('in') == 'query' and param.get('name') not in (
                        'pageNumber', 'pageSize', 'orderBy', 'includeRelation',
                    ):
                        resource_info['list_query_params'].append(param)

            elif method in ('POST', 'PUT'):
                # PUT on a collection path is a create/upsert operation.
                resource_info['has_create'] = True
                if method == 'PUT':
                    resource_info['update_method'] = 'PUT'
                    resource_info['create_method'] = 'PUT'
                resource_info['create_summary'] = operation.get('summary') or ''
                body = operation.get('requestBody', {})
                content = body.get('content', {}).get('application/json', {})
                schema = content.get('schema', {})
                if '$ref' in schema:
                    schema = resolve_refs_recursive(spec, schema)
                resource_info['create_schema'] = schema
                resource_info['create_response_schema'] = _extract_success_response_schema(operation, spec)

            elif method == 'PATCH':
                resource_info['has_update'] = True
                resource_info['update_method'] = 'PATCH'
                if not resource_info['update_summary']:
                    resource_info['update_summary'] = operation.get('summary') or ''
                body = operation.get('requestBody', {})
                content = body.get('content', {}).get('application/json', {})
                schema = content.get('schema', {})
                if '$ref' in schema:
                    schema = resolve_refs_recursive(spec, schema)
                resource_info['update_schema'] = schema
                resource_info['update_response_schema'] = _extract_success_response_schema(operation, spec)

            elif method == 'DELETE':
                resource_info['has_delete'] = True
                # DELETE endpoints can declare required query parameters
                # (e.g. expected-machine/all requires siteId); collect them so
                # the delete transport can forward them.
                for param in operation.get('parameters', []):
                    if param.get('in') == 'query' and param.get('name') not in (
                        'pageNumber', 'pageSize', 'orderBy', 'includeRelation',
                    ) and not any(
                        p.get('name') == param.get('name')
                        for p in resource_info['delete_query_params']
                    ):
                        resource_info['delete_query_params'].append(param)
                body = operation.get('requestBody', {})
                if body:
                    content = body.get('content', {}).get('application/json', {})
                    schema = content.get('schema', {})
                    if '$ref' in schema:
                        schema = resolve_refs_recursive(spec, schema)
                    resource_info['delete_schema'] = schema
                resource_info['delete_response_schema'] = _extract_success_response_schema(operation, spec)

        elif path_type == 'item':
            resource_info['item_path'] = path
            resource_info['id_param'] = op['id_param']

            if method == 'GET':
                resource_info['has_get'] = True
                resource_info['has_get_by_id'] = True
                for param in operation.get('parameters', []):
                    if param.get('in') == 'query' and param.get('name') not in (
                        'pageNumber', 'pageSize', 'orderBy', 'includeRelation',
                    ):
                        resource_info['item_query_params'].append(param)
                resp = operation.get('responses', {}).get('200', {})
                content = resp.get('content', {}).get('application/json', {})
                schema = content.get('schema', {})
                if '$ref' in schema:
                    schema = resolve_refs_recursive(spec, schema)
                resource_info['response_schema'] = schema

            elif method in ('PATCH', 'PUT'):
                resource_info['has_update'] = True
                resource_info['update_method'] = method
                if method == 'PATCH' and not resource_info['update_summary']:
                    resource_info['update_summary'] = operation.get('summary') or ''
                body = operation.get('requestBody', {})
                content = body.get('content', {}).get('application/json', {})
                schema = content.get('schema', {})
                if '$ref' in schema:
                    schema = resolve_refs_recursive(spec, schema)
                resource_info['update_schema'] = schema
                resource_info['update_response_schema'] = _extract_success_response_schema(operation, spec)

            elif method == 'DELETE':
                resource_info['has_delete'] = True
                # Item DELETEs can declare required query parameters (e.g.
                # measured-boot trusted-profile requires siteId and selector).
                for param in operation.get('parameters', []):
                    if param.get('in') == 'query' and param.get('name') not in (
                        'pageNumber', 'pageSize', 'orderBy', 'includeRelation',
                    ) and not any(
                        p.get('name') == param.get('name')
                        for p in resource_info['delete_query_params']
                    ):
                        resource_info['delete_query_params'].append(param)
                body = operation.get('requestBody', {})
                if body:
                    content = body.get('content', {}).get('application/json', {})
                    schema = content.get('schema', {})
                    if '$ref' in schema:
                        schema = resolve_refs_recursive(spec, schema)
                    resource_info['delete_schema'] = schema
                resource_info['delete_response_schema'] = _extract_success_response_schema(operation, spec)

    if not resource_info['response_schema'] and resource_info['has_list']:
        for op in operations:
            if op['method'] == 'GET' and op['path_type'] == 'collection':
                resp = op['operation'].get('responses', {}).get('200', {})
                content = resp.get('content', {}).get('application/json', {})
                schema = content.get('schema', {})
                if schema.get('type') == 'array':
                    items = schema.get('items', {})
                    if '$ref' in items:
                        items = resolve_refs_recursive(spec, items)
                    resource_info['response_schema'] = items
                elif '$ref' in schema:
                    resource_info['response_schema'] = resolve_refs_recursive(spec, schema)
                break

    if not resource_info['item_path'] and not resource_info['collection_path']:
        for op in operations:
            if op['method'] == 'GET':
                resource_info['collection_path'] = op['path']
                resource_info['has_list'] = True
                resp = op['operation'].get('responses', {}).get('200', {})
                content = resp.get('content', {}).get('application/json', {})
                schema = content.get('schema', {})
                if '$ref' in schema:
                    schema = resolve_refs_recursive(spec, schema)
                resource_info['response_schema'] = schema
                break

    return resource_info
