# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Terraform provider Go file generator backend.

Generates resource_*.go, data_source_*.go, resources_registry.go, and
examples/ for the dsx-ai-factory/terraform-provider-nvidia-infra-controller
repository from the NICo OpenAPI spec.
"""

import json
import os
import re
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from common import (
    camel_to_snake,
    resolve_ref,
    resolve_refs_recursive,
    classify_path,
    extract_id_param,
    detect_nested_module_name as detect_nested_name,
    group_paths_by_tag,
    analyze_resource,
)
from backends.terraform_config import (
    RESOURCE_OVERRIDES,
    READ_ONLY_TAGS,
    SKIP_TAGS,
    TAG_TO_RESOURCE,
    SKIP_PATHS,
)

_PATH_PARAM_RE = re.compile(r'\{(\w+)\}')

_RESERVED_TF_NAMES = frozenset({
    'count', 'depends_on', 'for_each', 'lifecycle',
    'provider', 'provisioner', 'connection',
})


# ---------------------------------------------------------------------------
# Name helpers
# ---------------------------------------------------------------------------

def snake_to_pascal(name):
    return ''.join(word.capitalize() for word in name.split('_'))


def tag_to_resource_name(tag):
    if tag in TAG_TO_RESOURCE:
        return TAG_TO_RESOURCE[tag]
    return camel_to_snake(tag.replace(' ', ''))


def safe_tf_name(name):
    return (name + '_value') if name in _RESERVED_TF_NAMES else name


def get_schema_type(schema):
    t = schema.get('type', 'string')
    if isinstance(t, list):
        non_null = [x for x in t if x != 'null']
        return non_null[0] if non_null else 'string'
    return t


def safe_format(template, **kwargs):
    """Substitute {key} placeholders without interpreting Go-style {} braces."""
    result = template
    for k in sorted(kwargs.keys(), key=lambda x: -len(x)):
        result = result.replace('{' + k + '}', str(kwargs[k]))
    result = result.replace('{{', '{').replace('}}', '}')
    return result


def go_str(s):
    """Escape a string for use inside a Go interpreted string literal.

    json.dumps produces a valid JSON string; its content is also a valid Go
    interpreted string, so stripping the outer quotes gives us a correctly
    escaped Go string body (backslashes, quotes, control characters).
    """
    return json.dumps(' '.join(s.split()), ensure_ascii=False)[1:-1]


# ---------------------------------------------------------------------------
# Field analysis
# ---------------------------------------------------------------------------

def analyze_field(prop_name, prop_schema, spec, resource_pascal, required_fields):
    snake_name = safe_tf_name(camel_to_snake(prop_name))
    pascal_name = snake_to_pascal(snake_name)
    schema_type = get_schema_type(prop_schema)
    description = go_str(prop_schema.get('description', '%s attribute.' % snake_name))

    base = {
        'snake_name': snake_name,
        'pascal_name': pascal_name,
        'json_name': prop_name,
        'required': prop_name in required_fields,
        'read_only': prop_schema.get('readOnly', False),
        'description': description,
        'enum': prop_schema.get('enum'),
        'sub_fields': [],
        'nested_type_name': None,
        'element_go_type': None,
        'tf_attr_type': None,
        'go_type': None,
    }

    if schema_type == 'string':
        base.update({'go_type': 'types.String', 'tf_attr_type': 'string'})
    elif schema_type == 'integer':
        base.update({'go_type': 'types.Int64', 'tf_attr_type': 'int64'})
    elif schema_type == 'boolean':
        base.update({'go_type': 'types.Bool', 'tf_attr_type': 'bool'})
    elif schema_type == 'number':
        base.update({'go_type': 'types.Float64', 'tf_attr_type': 'float64'})
    elif schema_type == 'object' and 'additionalProperties' in prop_schema:
        base.update({'go_type': 'types.Map', 'tf_attr_type': 'map_string'})
    elif schema_type == 'object' and 'properties' in prop_schema:
        nested_name = resource_pascal + pascal_name
        sub_fields = analyze_schema_fields(prop_schema, spec, nested_name)
        base.update({
            'go_type': 'types.Object',
            'tf_attr_type': 'single_nested',
            'nested_type_name': nested_name,
            'sub_fields': sub_fields,
        })
    elif schema_type == 'array':
        items_schema = prop_schema.get('items', {})
        if '$ref' in items_schema:
            items_schema = resolve_refs_recursive(spec, items_schema)
        items_type = get_schema_type(items_schema)

        if items_type == 'string':
            base.update({'go_type': 'types.List', 'tf_attr_type': 'list_string', 'element_go_type': 'types.StringType'})
        elif items_type == 'integer':
            base.update({'go_type': 'types.List', 'tf_attr_type': 'list_int64', 'element_go_type': 'types.Int64Type'})
        elif items_type == 'boolean':
            base.update({'go_type': 'types.List', 'tf_attr_type': 'list_bool', 'element_go_type': 'types.BoolType'})
        elif items_type in ('object',) or 'properties' in items_schema:
            nested_name = resource_pascal + pascal_name + 'Item'
            sub_fields = analyze_schema_fields(items_schema, spec, nested_name)
            base.update({
                'go_type': 'types.List',
                'tf_attr_type': 'list_nested',
                'nested_type_name': nested_name,
                'sub_fields': sub_fields,
            })
        else:
            base.update({'go_type': 'types.List', 'tf_attr_type': 'list_string', 'element_go_type': 'types.StringType'})
    else:
        base.update({'go_type': 'types.String', 'tf_attr_type': 'string'})

    return base


def analyze_schema_fields(schema, spec, resource_pascal):
    if not schema or 'properties' not in schema:
        return []
    required_fields = schema.get('required', [])
    return [
        analyze_field(prop_name, prop_schema, spec, resource_pascal, required_fields)
        for prop_name, prop_schema in schema.get('properties', {}).items()
    ]


def merge_resource_fields(resource_info, spec):
    resource_name = resource_info['resource_name']
    resource_pascal = snake_to_pascal(resource_name)
    field_map = {}

    def add_fields(schema, flag):
        if not schema or 'properties' not in schema:
            return
        required_fields = schema.get('required', [])
        for prop_name, prop_schema in schema.get('properties', {}).items():
            snake_name = camel_to_snake(prop_name)
            if snake_name not in field_map:
                f = analyze_field(prop_name, prop_schema, spec, resource_pascal, required_fields)
                f['in_create'] = False
                f['in_update'] = False
                f['in_response'] = False
                field_map[snake_name] = f
            field_map[snake_name][flag] = True
            if flag == 'in_create' and prop_name in required_fields:
                field_map[snake_name]['required'] = True

    add_fields(resource_info.get('create_schema'), 'in_create')
    add_fields(resource_info.get('update_schema'), 'in_update')
    add_fields(resource_info.get('response_schema'), 'in_response')

    path_params = set()
    for path in (resource_info.get('collection_path') or '', resource_info.get('item_path') or ''):
        for match in _PATH_PARAM_RE.findall(path):
            if match != 'org':
                path_params.add(safe_tf_name(camel_to_snake(match)))

    return list(field_map.values()), path_params


# ---------------------------------------------------------------------------
# Go code generation helpers
# ---------------------------------------------------------------------------

def go_struct_field(field):
    return '\t%s %s `tfsdk:"%s"`' % (field['pascal_name'], field['go_type'], field['snake_name'])


def _tf_attr_type_expr(f):
    """Return the Go attr.Type expression for a field."""
    t = f['tf_attr_type']
    if t == 'string':
        return 'types.StringType'
    elif t == 'int64':
        return 'types.Int64Type'
    elif t == 'bool':
        return 'types.BoolType'
    elif t == 'float64':
        return 'types.Float64Type'
    elif t == 'map_string':
        return 'types.MapType{ElemType: types.StringType}'
    elif t in ('list_string', 'list_int64', 'list_bool'):
        return 'types.ListType{ElemType: %s}' % (f['element_go_type'] or 'types.StringType')
    elif t == 'single_nested':
        return 'types.ObjectType{AttrTypes: %sAttrTypes()}' % f['nested_type_name']
    elif t == 'list_nested':
        return 'types.ListType{ElemType: types.ObjectType{AttrTypes: %sAttrTypes()}}' % f['nested_type_name']
    return 'types.StringType'


def go_nested_structs(fields, depth=0):
    """Emit AttrTypes() functions for nested object/list types."""
    lines = []
    for f in fields:
        if f['tf_attr_type'] in ('list_nested', 'single_nested') and f['sub_fields']:
            type_name = f['nested_type_name']
            lines.append('func %sAttrTypes() map[string]attr.Type {' % type_name)
            lines.append('\treturn map[string]attr.Type{')
            for sf in f['sub_fields']:
                lines.append('\t\t"%s": %s,' % (sf['snake_name'], _tf_attr_type_expr(sf)))
            lines.append('\t}')
            lines.append('}')
            lines.append('')
            lines.extend(go_nested_structs(f['sub_fields'], depth + 1))
    return lines


def go_schema_attribute(field, required=False, optional=False, computed=False):
    desc = go_str(field['description'])
    req_str = 'true' if required else 'false'
    opt_str = 'true' if optional else 'false'
    comp_str = 'true' if computed else 'false'
    t = field['tf_attr_type']

    if t in ('string', 'int64', 'bool', 'float64'):
        attr_type = {'string': 'schema.StringAttribute', 'int64': 'schema.Int64Attribute',
                     'bool': 'schema.BoolAttribute', 'float64': 'schema.Float64Attribute'}[t]
        return ['"%s": %s{' % (field['snake_name'], attr_type),
                '\tRequired:    %s,' % req_str, '\tOptional:    %s,' % opt_str,
                '\tComputed:    %s,' % comp_str, '\tDescription: "%s",' % desc, '},']
    elif t == 'map_string':
        return ['"%s": schema.MapAttribute{' % field['snake_name'],
                '\tElementType: types.StringType,',
                '\tRequired:    %s,' % req_str, '\tOptional:    %s,' % opt_str,
                '\tComputed:    %s,' % comp_str, '\tDescription: "%s",' % desc, '},']
    elif t in ('list_string', 'list_int64', 'list_bool'):
        elem_type = field['element_go_type'] or 'types.StringType'
        return ['"%s": schema.ListAttribute{' % field['snake_name'],
                '\tElementType: %s,' % elem_type,
                '\tRequired:    %s,' % req_str, '\tOptional:    %s,' % opt_str,
                '\tComputed:    %s,' % comp_str, '\tDescription: "%s",' % desc, '},']
    elif t == 'list_nested':
        nested_attrs = go_schema_attrs_block(field['sub_fields'], computed_only=computed)
        lines = ['"%s": schema.ListNestedAttribute{' % field['snake_name'],
                 '\tRequired:    %s,' % req_str, '\tOptional:    %s,' % opt_str,
                 '\tComputed:    %s,' % comp_str, '\tDescription: "%s",' % desc,
                 '\tNestedObject: schema.NestedAttributeObject{',
                 '\t\tAttributes: map[string]schema.Attribute{']
        for line in nested_attrs:
            lines.append('\t\t\t' + line)
        lines += ['\t\t},', '\t},', '},']
        return lines
    elif t == 'single_nested':
        nested_attrs = go_schema_attrs_block(field['sub_fields'], computed_only=computed)
        lines = ['"%s": schema.SingleNestedAttribute{' % field['snake_name'],
                 '\tRequired:    %s,' % req_str, '\tOptional:    %s,' % opt_str,
                 '\tComputed:    %s,' % comp_str, '\tDescription: "%s",' % desc,
                 '\tAttributes: map[string]schema.Attribute{']
        for line in nested_attrs:
            lines.append('\t\t' + line)
        lines += ['\t},', '},']
        return lines
    return ['"%s": schema.StringAttribute{Optional: true, Computed: true, Description: "%s"},' % (field['snake_name'], desc)]


def go_schema_attrs_block(fields, computed_only=False):
    lines = []
    for f in fields:
        if f['read_only'] or computed_only:
            lines.extend(go_schema_attribute(f, required=False, optional=False, computed=True))
        elif f['required']:
            lines.extend(go_schema_attribute(f, required=True, optional=False, computed=False))
        else:
            lines.extend(go_schema_attribute(f, required=False, optional=True, computed=True))
    return lines


def go_populate_model_field(field, source_var='result'):
    t = field['tf_attr_type']
    sn = field['snake_name']
    pn = field['pascal_name']
    jn = field['json_name']

    if t == 'string':
        return ['data.%s = StringFromAPI(%s["%s"])' % (pn, source_var, jn)]
    elif t == 'int64':
        return ['data.%s = Int64FromAPI(%s["%s"])' % (pn, source_var, jn)]
    elif t == 'bool':
        return ['data.%s = BoolFromAPI(%s["%s"])' % (pn, source_var, jn)]
    elif t == 'float64':
        return ['data.%s = Float64FromAPI(%s["%s"])' % (pn, source_var, jn)]
    elif t == 'map_string':
        return [
            'if rawMap_%s := StringMapFromAPI(%s["%s"]); rawMap_%s != nil {' % (sn, source_var, jn, sn),
            '\tmv, d := types.MapValueFrom(ctx, types.StringType, rawMap_%s)' % sn,
            '\tdiags.Append(d...)',
            '\tdata.%s = mv' % pn,
            '} else {',
            '\tdata.%s = types.MapNull(types.StringType)' % pn,
            '}',
        ]
    elif t in ('list_string', 'list_int64', 'list_bool'):
        elem_type = field['element_go_type'] or 'types.StringType'
        slice_helper = {'list_string': 'StringSliceFromAPI', 'list_int64': 'Int64SliceFromAPI', 'list_bool': 'BoolSliceFromAPI'}[t]
        return [
            'if rawSlice_%s := %s(%s["%s"]); rawSlice_%s != nil {' % (sn, slice_helper, source_var, jn, sn),
            '\tlv, d := types.ListValueFrom(ctx, %s, rawSlice_%s)' % (elem_type, sn),
            '\tdiags.Append(d...)',
            '\tdata.%s = lv' % pn,
            '} else {',
            '\tdata.%s = types.ListNull(%s)' % (pn, elem_type),
            '}',
        ]
    elif t == 'list_nested':
        nested_type = field['nested_type_name']
        sub_fields = field['sub_fields']
        lines = [
            'if rawItems_%s, ok := %s["%s"].([]interface{}); ok && rawItems_%s != nil {' % (sn, source_var, jn, sn),
            '\tvar items_%s []map[string]interface{}' % sn,
            '\tfor _, raw_%s := range rawItems_%s {' % (sn, sn),
            '\t\tm_%s, _ := raw_%s.(map[string]interface{})' % (sn, sn),
            '\t\tif m_%s == nil { m_%s = map[string]interface{}{} }' % (sn, sn),
            '\t\titems_%s = append(items_%s, m_%s)' % (sn, sn, sn),
            '\t}',
            '\tlv_%s, d_%s := types.ListValueFrom(ctx, types.ObjectType{AttrTypes: %sAttrTypes()}, items_%s)' % (sn, sn, nested_type, sn),
            '\tdiags.Append(d_%s...)' % sn,
            '\tdata.%s = lv_%s' % (pn, sn),
            '} else {',
            '\tdata.%s = types.ListNull(types.ObjectType{AttrTypes: %sAttrTypes()})' % (pn, nested_type),
            '}',
        ]
        return lines
    elif t == 'single_nested':
        nested_type = field['nested_type_name']
        lines = [
            'if rawObj_%s, ok := %s["%s"].(map[string]interface{}); ok {' % (sn, source_var, jn),
            '\tov_%s, d_%s := types.ObjectValueFrom(ctx, %sAttrTypes(), rawObj_%s)' % (sn, sn, nested_type, sn),
            '\tdiags.Append(d_%s...)' % sn,
            '\tdata.%s = ov_%s' % (pn, sn),
            '} else {',
            '\tdata.%s = types.ObjectNull(%sAttrTypes())' % (pn, nested_type),
            '}',
        ]
        return lines
    return []


def go_populate_nested_field(field, source_var, dest_var):
    t = field['tf_attr_type']
    pn = field['pascal_name']
    jn = field['json_name']
    if t == 'string':
        return ['%s.%s = StringFromAPI(%s["%s"])' % (dest_var, pn, source_var, jn)]
    elif t == 'int64':
        return ['%s.%s = Int64FromAPI(%s["%s"])' % (dest_var, pn, source_var, jn)]
    elif t == 'bool':
        return ['%s.%s = BoolFromAPI(%s["%s"])' % (dest_var, pn, source_var, jn)]
    elif t == 'float64':
        return ['%s.%s = Float64FromAPI(%s["%s"])' % (dest_var, pn, source_var, jn)]
    return ['// %s: complex nested field — expand manually if needed' % jn]


def go_build_body_field(field):
    t = field['tf_attr_type']
    sn = field['snake_name']
    pn = field['pascal_name']
    jn = field['json_name']

    if t in ('string', 'int64', 'bool', 'float64'):
        val_method = {'string': 'ValueString', 'int64': 'ValueInt64', 'bool': 'ValueBool', 'float64': 'ValueFloat64'}[t]
        return ['if !data.%s.IsNull() && !data.%s.IsUnknown() {' % (pn, pn),
                '\tbody["%s"] = data.%s.%s()' % (jn, pn, val_method), '}']
    elif t == 'map_string':
        return ['if !data.%s.IsNull() && !data.%s.IsUnknown() {' % (pn, pn),
                '\tvar m_%s map[string]string' % sn,
                '\tdata.%s.ElementsAs(ctx, &m_%s, false)' % (pn, sn),
                '\tbody["%s"] = m_%s' % (jn, sn), '}']
    elif t in ('list_string', 'list_int64', 'list_bool'):
        go_elem = {'list_string': 'string', 'list_int64': 'int64', 'list_bool': 'bool'}[t]
        return ['if !data.%s.IsNull() && !data.%s.IsUnknown() {' % (pn, pn),
                '\tvar sl_%s []%s' % (sn, go_elem),
                '\tdata.%s.ElementsAs(ctx, &sl_%s, false)' % (pn, sn),
                '\tbody["%s"] = sl_%s' % (jn, sn), '}']
    elif t == 'list_nested':
        nested_type = field['nested_type_name']
        return ['if !data.%s.IsNull() && !data.%s.IsUnknown() {' % (pn, pn),
                '\tvar items_%s []map[string]interface{}' % sn,
                '\tdata.%s.ElementsAs(ctx, &items_%s, false)' % (pn, sn),
                '\tbody["%s"] = items_%s' % (jn, sn), '}']
    elif t == 'single_nested':
        nested_type = field['nested_type_name']
        return ['if !data.%s.IsNull() && !data.%s.IsUnknown() {' % (pn, pn),
                '\tvar obj_%s map[string]interface{}' % sn,
                '\tdata.%s.As(ctx, &obj_%s, basetypes.ObjectAsOptions{})' % (pn, sn),
                '\tbody["%s"] = obj_%s' % (jn, sn), '}']
    return []


def go_build_nested_body_field(field, source_var, dest_map):
    t = field['tf_attr_type']
    pn = field['pascal_name']
    jn = field['json_name']
    if t == 'string':
        return ['if !%s.%s.IsNull() { %s["%s"] = %s.%s.ValueString() }' % (source_var, pn, dest_map, jn, source_var, pn)]
    elif t == 'int64':
        return ['if !%s.%s.IsNull() { %s["%s"] = %s.%s.ValueInt64() }' % (source_var, pn, dest_map, jn, source_var, pn)]
    elif t == 'bool':
        return ['if !%s.%s.IsNull() { %s["%s"] = %s.%s.ValueBool() }' % (source_var, pn, dest_map, jn, source_var, pn)]
    elif t == 'float64':
        return ['if !%s.%s.IsNull() { %s["%s"] = %s.%s.ValueFloat64() }' % (source_var, pn, dest_map, jn, source_var, pn)]
    return ['// %s: complex nested field — expand manually if needed' % jn]


def go_path_params(item_path, resource_info):
    id_param = resource_info.get('id_param') or 'id'
    params = []
    if item_path:
        for match in _PATH_PARAM_RE.findall(item_path):
            if match == 'org':
                continue
            snake_param = safe_tf_name(camel_to_snake(match))
            if snake_param == safe_tf_name(camel_to_snake(id_param)):
                params.append('"%s": data.Id.ValueString()' % match)
            else:
                params.append('"%s": data.%s.ValueString()' % (match, snake_to_pascal(snake_param)))
    return 'map[string]string{%s}' % ', '.join(params) if params else 'map[string]string{}'


# ---------------------------------------------------------------------------
# Resource file generation
# ---------------------------------------------------------------------------

def generate_resource_file(resource_info, spec, overrides):
    resource_name = resource_info['resource_name']
    resource_pascal = snake_to_pascal(resource_name)
    tag = resource_info['tag']
    description = go_str(resource_info.get('description') or 'Manages %s resources.' % tag)

    fields, path_params = merge_resource_fields(resource_info, spec)
    scope_fields = overrides.get('scope_fields', [])
    for sf in scope_fields:
        path_params.add(sf)

    create_field_names = set(
        camel_to_snake(p) for p in (resource_info.get('create_schema') or {}).get('properties', {})
    )
    update_field_names = set(
        camel_to_snake(p) for p in (resource_info.get('update_schema') or {}).get('properties', {})
    )

    collection_path = resource_info.get('collection_path') or ''
    item_path = resource_info.get('item_path') or ''
    id_param = resource_info.get('id_param') or 'id'
    no_create = overrides.get('no_create', False)
    has_update = resource_info.get('has_update', False)
    has_delete = resource_info.get('has_delete', False)
    version_field = overrides.get('version_field')

    schema_attrs_lines = [
        '// id is always computed',
        '"id": schema.StringAttribute{Computed: true, Description: "The resource ID."},',
    ]
    field_snake_names = {f['snake_name'] for f in fields}
    for pp in sorted(path_params):
        pp = safe_tf_name(pp)
        if pp not in field_snake_names and pp != 'id':
            fields.append({
                'snake_name': pp, 'pascal_name': snake_to_pascal(pp), 'json_name': pp,
                'go_type': 'types.String', 'tf_attr_type': 'string', 'required': False,
                'read_only': False, 'description': 'Path parameter: %s.' % pp,
                'enum': None, 'sub_fields': [], 'nested_type_name': None, 'element_go_type': None,
                'in_create': False, 'in_update': False, 'in_response': False,
            })

    for f in fields:
        if f['snake_name'] == 'id':
            continue
        is_pp = f['snake_name'] in path_params
        if f['read_only'] or (not f.get('in_create') and not f.get('in_update') and f.get('in_response')):
            schema_attrs_lines.extend(go_schema_attribute(f, required=False, optional=False, computed=True))
        elif f['required'] and not is_pp:
            schema_attrs_lines.extend(go_schema_attribute(f, required=True, optional=False, computed=False))
        else:
            schema_attrs_lines.extend(go_schema_attribute(f, required=False, optional=True, computed=True))

    struct_fields_lines = ['\tId types.String `tfsdk:"id"`']
    for f in fields:
        if f['snake_name'] != 'id':
            struct_fields_lines.append(go_struct_field(f))

    nested_struct_lines = go_nested_structs(fields)

    create_body_lines = [l for f in fields if f['snake_name'] in create_field_names and not f['read_only'] for l in go_build_body_field(f)]
    update_body_lines = []
    if version_field:
        update_body_lines += ['body["%s"] = data.%s.ValueString()' % (version_field, snake_to_pascal(version_field))]
    update_body_lines += [l for f in fields if f['snake_name'] in update_field_names and not f['read_only'] and f['snake_name'] != version_field for l in go_build_body_field(f)]
    populate_lines = [l for f in fields if f['snake_name'] != 'id' and f.get('in_response') for l in go_populate_model_field(f)]

    item_path_params = go_path_params(item_path, resource_info)
    collection_path_params = go_path_params(collection_path, resource_info)

    schema_attrs = '\n\t\t\t'.join(schema_attrs_lines)
    struct_fields = '\n'.join(struct_fields_lines)
    nested_structs = '\n'.join(nested_struct_lines)
    create_body = '\n\t'.join(create_body_lines) if create_body_lines else '// no create fields'
    update_body = '\n\t'.join(update_body_lines) if update_body_lines else '// no update fields'
    populate = '\n\t'.join(populate_lines + ['_ = diags']) if populate_lines else '_ = diags'

    # ImportState: pass ID through; for scoped resources parse composite "scope_id/id"
    scope_fields_list = overrides.get('scope_fields', [])
    if scope_fields_list:
        parse_lines = [
            'parts := strings.SplitN(req.ID, "/", %d)' % (len(scope_fields_list) + 1),
        ]
        for i, sf in enumerate(scope_fields_list):
            parse_lines.append(
                'resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("%s"), parts[%d])...)' % (sf, i)
            )
        parse_lines.append(
            'resp.Diagnostics.Append(resp.State.SetAttribute(ctx, path.Root("id"), parts[%d])...)' % len(scope_fields_list)
        )
        import_body = '\n\t'.join(parse_lines)
        import_method = safe_format(
            'func (r *{rp}Resource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {{\n'
            '\t// Expected format: {scope_fmt}/id\n'
            '\t{import_body}\n'
            '}}',
            rp=resource_pascal,
            scope_fmt='/'.join(scope_fields_list),
            import_body=import_body,
        )
    else:
        import_method = safe_format(
            'func (r *{rp}Resource) ImportState(ctx context.Context, req resource.ImportStateRequest, resp *resource.ImportStateResponse) {{\n'
            '\tresource.ImportStatePassthroughID(ctx, path.Root("id"), req, resp)\n'
            '}}',
            rp=resource_pascal,
        )

    if no_create or not resource_info.get('has_create'):
        create_method = safe_format(
            'func (r *{rp}Resource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {{\n'
            '\tresp.Diagnostics.AddError("Create not supported", "This resource does not support creation.")\n'
            '}}', rp=resource_pascal)
    else:
        create_method = safe_format('''\
func (r *{rp}Resource) Create(ctx context.Context, req resource.CreateRequest, resp *resource.CreateResponse) {{
\tvar data {rp}ResourceModel
\tresp.Diagnostics.Append(req.Plan.Get(ctx, &data)...)
\tif resp.Diagnostics.HasError() {{
\t\treturn
\t}}
\tbody := map[string]interface{}{}
\t{create_body}
\turl := r.client.ResolvePath("{cpath}", {cpparams})
\tresult, err := r.client.Post(ctx, url, body)
\tif err != nil {{
\t\tresp.Diagnostics.AddError("Error creating {tag}", err.Error())
\t\treturn
\t}}
\tdata.Id = StringFromAPI(result["id"])
\tdiags := &resp.Diagnostics
\t{populate}
\tresp.Diagnostics.Append(resp.State.Set(ctx, &data)...)
}}''', rp=resource_pascal, tag=tag, cpath=collection_path, cpparams=collection_path_params,
            create_body=create_body, populate=populate)

    read_method = safe_format('''\
func (r *{rp}Resource) Read(ctx context.Context, req resource.ReadRequest, resp *resource.ReadResponse) {{
\tvar data {rp}ResourceModel
\tresp.Diagnostics.Append(req.State.Get(ctx, &data)...)
\tif resp.Diagnostics.HasError() {{
\t\treturn
\t}}
\turl := r.client.ResolvePath("{ipath}", {ipparams})
\tresult, err := r.client.Get(ctx, url)
\tif err != nil {{
\t\tresp.Diagnostics.AddError("Error reading {tag}", err.Error())
\t\treturn
\t}}
\tif result == nil {{
\t\tresp.State.RemoveResource(ctx)
\t\treturn
\t}}
\tdata.Id = StringFromAPI(result["id"])
\tdiags := &resp.Diagnostics
\t{populate}
\tresp.Diagnostics.Append(resp.State.Set(ctx, &data)...)
}}''', rp=resource_pascal, tag=tag, ipath=item_path, ipparams=item_path_params, populate=populate)

    if not has_update:
        update_method = safe_format(
            'func (r *{rp}Resource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {{\n'
            '\tresp.Diagnostics.AddError("Update not supported", "This resource does not support in-place updates.")\n'
            '}}', rp=resource_pascal)
    else:
        update_method = safe_format('''\
func (r *{rp}Resource) Update(ctx context.Context, req resource.UpdateRequest, resp *resource.UpdateResponse) {{
\tvar data {rp}ResourceModel
\tresp.Diagnostics.Append(req.Plan.Get(ctx, &data)...)
\tif resp.Diagnostics.HasError() {{
\t\treturn
\t}}
\tbody := map[string]interface{}{}
\t{update_body}
\turl := r.client.ResolvePath("{ipath}", {ipparams})
\tresult, err := r.client.Patch(ctx, url, body)
\tif err != nil {{
\t\tresp.Diagnostics.AddError("Error updating {tag}", err.Error())
\t\treturn
\t}}
\tdata.Id = StringFromAPI(result["id"])
\tdiags := &resp.Diagnostics
\t{populate}
\tresp.Diagnostics.Append(resp.State.Set(ctx, &data)...)
}}''', rp=resource_pascal, tag=tag, ipath=item_path, ipparams=item_path_params,
            update_body=update_body, populate=populate)

    if not has_delete:
        delete_method = safe_format(
            'func (r *{rp}Resource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {{\n'
            '\tresp.Diagnostics.AddError("Delete not supported", "This resource does not support deletion.")\n'
            '}}', rp=resource_pascal)
    else:
        delete_body_fields = list(overrides.get('delete_body_fields', []))
        delete_schema = resource_info.get('delete_schema')
        if delete_schema and 'properties' in delete_schema:
            for pname in delete_schema['properties']:
                sn = camel_to_snake(pname)
                if sn not in delete_body_fields:
                    delete_body_fields.append(sn)

        if delete_body_fields:
            del_body_lines = ['body := map[string]interface{}{}']
            for f in fields:
                if f['snake_name'] in delete_body_fields:
                    del_body_lines.extend(go_build_body_field(f))
            del_body_lines.append('err := r.client.Delete(ctx, url, body)')
        else:
            del_body_lines = ['err := r.client.Delete(ctx, url, nil)']

        delete_method = safe_format('''\
func (r *{rp}Resource) Delete(ctx context.Context, req resource.DeleteRequest, resp *resource.DeleteResponse) {{
\tvar data {rp}ResourceModel
\tresp.Diagnostics.Append(req.State.Get(ctx, &data)...)
\tif resp.Diagnostics.HasError() {{
\t\treturn
\t}}
\turl := r.client.ResolvePath("{ipath}", {ipparams})
\t{del_body}
\tif err != nil {{
\t\tresp.Diagnostics.AddError("Error deleting {tag}", err.Error())
\t}}
}}''', rp=resource_pascal, tag=tag, ipath=item_path, ipparams=item_path_params,
            del_body='\n\t'.join(del_body_lines))

    return safe_format('''\
// Code generated by tools/generate/generate.py. Do not edit manually.
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package provider

import (
\t"context"
\t"fmt"
\t"strings"

\t"github.com/hashicorp/terraform-plugin-framework/attr"
\t"github.com/hashicorp/terraform-plugin-framework/diag"
\t"github.com/hashicorp/terraform-plugin-framework/path"
\t"github.com/hashicorp/terraform-plugin-framework/resource"
\t"github.com/hashicorp/terraform-plugin-framework/resource/schema"
\t"github.com/hashicorp/terraform-plugin-framework/types"
\t"github.com/hashicorp/terraform-plugin-framework/types/basetypes"
)

var _ resource.Resource = &{rp}Resource{{}}
var _ resource.ResourceWithConfigure = &{rp}Resource{{}}
var _ resource.ResourceWithImportState = &{rp}Resource{{}}

func New{rp}Resource() resource.Resource {{
\treturn &{rp}Resource{{}}
}}

type {rp}Resource struct {{
\tclient *Client
}}

type {rp}ResourceModel struct {{
{struct_fields}
}}

{nested_structs}
func (r *{rp}Resource) Metadata(_ context.Context, req resource.MetadataRequest, resp *resource.MetadataResponse) {{
\tresp.TypeName = req.ProviderTypeName + "_{rn}"
}}

func (r *{rp}Resource) Schema(_ context.Context, _ resource.SchemaRequest, resp *resource.SchemaResponse) {{
\tresp.Schema = schema.Schema{{
\t\tDescription: "{desc}",
\t\tAttributes: map[string]schema.Attribute{{
\t\t\t{schema_attrs}
\t\t}},
\t}}
}}

func (r *{rp}Resource) Configure(_ context.Context, req resource.ConfigureRequest, resp *resource.ConfigureResponse) {{
\tif req.ProviderData == nil {{
\t\treturn
\t}}
\tclient, ok := req.ProviderData.(*Client)
\tif !ok {{
\t\tresp.Diagnostics.AddError(
\t\t\t"Unexpected Resource Configure Type",
\t\t\tfmt.Sprintf("Expected *Client, got: %T.", req.ProviderData),
\t\t)
\t\treturn
\t}}
\tr.client = client
}}

{create_method}

{read_method}

{update_method}

{delete_method}

{import_method}

func (r *{rp}Resource) populateModel(ctx context.Context, data *{rp}ResourceModel, result map[string]interface{{}}, diags *diag.Diagnostics) {{
\t{populate}
}}
''',
        rp=resource_pascal, rn=resource_name, desc=description,
        struct_fields=struct_fields,
        nested_structs=nested_structs + '\n' if nested_struct_lines else '',
        schema_attrs=schema_attrs,
        create_method=create_method, read_method=read_method,
        update_method=update_method, delete_method=delete_method,
        import_method=import_method,
        populate='\n\t'.join(populate_lines + ['_ = diags']) if populate_lines else '_ = diags; _ = ctx; _ = result',
    )


# ---------------------------------------------------------------------------
# Data source file generation
# ---------------------------------------------------------------------------

def generate_datasource_file(resource_info, spec, overrides):
    resource_name = resource_info['resource_name']
    resource_pascal = snake_to_pascal(resource_name)
    tag = resource_info['tag']
    description = go_str(resource_info.get('description') or 'Reads %s data.' % tag)

    response_schema = resource_info.get('response_schema') or {}
    ds_fields = analyze_schema_fields(response_schema, spec, resource_pascal + 'Ds')

    collection_path = resource_info.get('collection_path') or ''
    item_path = resource_info.get('item_path') or ''
    has_list = resource_info.get('has_list', False)
    has_get = resource_info.get('has_get', False)

    filter_fields = []
    seen_filter = set()
    for param in resource_info.get('list_query_params', []):
        sname = safe_tf_name(camel_to_snake(param.get('name', '')))
        if sname in seen_filter:
            continue
        seen_filter.add(sname)
        filter_fields.append({
            'snake_name': sname, 'pascal_name': snake_to_pascal(sname),
            'json_name': param.get('name', ''), 'go_type': 'types.String', 'tf_attr_type': 'string',
            'required': False, 'read_only': False,
            'description': param.get('description', 'Filter by %s.' % sname),
            'enum': None, 'sub_fields': [], 'nested_type_name': None, 'element_go_type': None,
        })

    for path in (collection_path, item_path):
        for match in _PATH_PARAM_RE.findall(path):
            if match == 'org':
                continue
            sname = safe_tf_name(camel_to_snake(match))
            id_param = resource_info.get('id_param') or 'id'
            if sname == safe_tf_name(camel_to_snake(id_param)) or sname in seen_filter:
                continue
            seen_filter.add(sname)
            filter_fields.append({
                'snake_name': sname, 'pascal_name': snake_to_pascal(sname), 'json_name': match,
                'go_type': 'types.String', 'tf_attr_type': 'string', 'required': False, 'read_only': False,
                'description': 'Path parameter: %s.' % sname,
                'enum': None, 'sub_fields': [], 'nested_type_name': None, 'element_go_type': None,
            })

    filter_snake_names = {ff['snake_name'] for ff in filter_fields}
    ds_fields_deduped = [f for f in ds_fields if f['snake_name'] not in filter_snake_names]

    seen_attrs = set()
    schema_attrs_lines = []
    if has_get and item_path:
        schema_attrs_lines.append('"id": schema.StringAttribute{Optional: true, Computed: true, Description: "ID of the resource to retrieve."},')
        seen_attrs.add('id')
    for ff in filter_fields:
        if ff['snake_name'] not in seen_attrs:
            seen_attrs.add(ff['snake_name'])
            schema_attrs_lines.extend(go_schema_attribute(ff, required=False, optional=True, computed=True))
    for f in ds_fields_deduped:
        if f['snake_name'] == 'id':
            if 'id' not in seen_attrs:
                schema_attrs_lines.append('"id": schema.StringAttribute{Computed: true, Description: "The resource ID."},')
                seen_attrs.add('id')
            continue
        if f['snake_name'] not in seen_attrs:
            seen_attrs.add(f['snake_name'])
            schema_attrs_lines.extend(go_schema_attribute(f, required=False, optional=False, computed=True))

    seen_fields = set()
    struct_fields_lines = []
    if has_get and item_path:
        struct_fields_lines.append('\tId types.String `tfsdk:"id"`')
        seen_fields.add('id')
    for ff in filter_fields:
        if ff['snake_name'] not in seen_fields:
            seen_fields.add(ff['snake_name'])
            struct_fields_lines.append(go_struct_field(ff))
    for f in ds_fields_deduped:
        if f['snake_name'] == 'id':
            if 'id' not in seen_fields:
                struct_fields_lines.append('\tId types.String `tfsdk:"id"`')
                seen_fields.add('id')
            continue
        if f['snake_name'] not in seen_fields:
            seen_fields.add(f['snake_name'])
            struct_fields_lines.append(go_struct_field(f))

    nested_struct_lines = go_nested_structs(ds_fields_deduped)
    populate_lines = [l for f in ds_fields_deduped if f['snake_name'] != 'id' for l in go_populate_model_field(f)]

    item_path_params = go_path_params(item_path, resource_info)
    collection_path_params = go_path_params(collection_path, resource_info)
    schema_attrs = '\n\t\t\t'.join(schema_attrs_lines)
    struct_fields = '\n'.join(struct_fields_lines)
    nested_structs = '\n'.join(nested_struct_lines)
    populate = '\n\t'.join(populate_lines + ['_ = diags']) if populate_lines else '_ = diags; _ = ctx; _ = result'

    if has_get and item_path:
        read_body = safe_format('''\
\tif !data.Id.IsNull() && !data.Id.IsUnknown() && data.Id.ValueString() != "" {{
\t\turl := d.client.ResolvePath("{ipath}", {ipparams})
\t\tresult, err := d.client.Get(ctx, url)
\t\tif err != nil {{
\t\t\tresp.Diagnostics.AddError("Error reading {tag}", err.Error())
\t\t\treturn
\t\t}}
\t\tif result == nil {{
\t\t\tresp.Diagnostics.AddError("{tag} not found", "No resource found with the given id.")
\t\t\treturn
\t\t}}
\t\tdata.Id = StringFromAPI(result["id"])
\t\tdiags := &resp.Diagnostics
\t\t{populate}
\t}} else if {has_list} {{
\t\turl := d.client.ResolvePath("{cpath}", {cpparams})
\t\titems, err := d.client.List(ctx, url, map[string]string{{}})
\t\tif err != nil {{
\t\t\tresp.Diagnostics.AddError("Error listing {tag}", err.Error())
\t\t\treturn
\t\t}}
\t\tif len(items) > 0 {{
\t\t\tresult := items[0]
\t\t\tdata.Id = StringFromAPI(result["id"])
\t\t\tdiags := &resp.Diagnostics
\t\t\t{populate}
\t\t}}
\t}}''',
            ipath=item_path, ipparams=item_path_params,
            cpath=collection_path, cpparams=collection_path_params,
            tag=tag, has_list=str(has_list).lower(),
            populate='\n\t\t'.join(populate_lines + ['_ = diags']) if populate_lines else '_ = diags',
        )
    elif has_list:
        read_body = safe_format('''\
\turl := d.client.ResolvePath("{cpath}", {cpparams})
\tqueryParams := map[string]string{{}}
\t{filter_params}
\titems, err := d.client.List(ctx, url, queryParams)
\tif err != nil {{
\t\tresp.Diagnostics.AddError("Error listing {tag}", err.Error())
\t\treturn
\t}}
\t_ = items
\t// TODO: expose items as a list attribute''',
            cpath=collection_path, cpparams=collection_path_params, tag=tag,
            filter_params='\n\t'.join(
                'if !data.{p}.IsNull() && !data.{p}.IsUnknown() {{ queryParams["{j}"] = data.{p}.ValueString() }}'.format(
                    p=ff['pascal_name'], j=ff['json_name'])
                for ff in filter_fields
            ) or '// no filter params',
        )
    else:
        read_body = '\t// singleton resource'

    return safe_format('''\
// Code generated by tools/generate/generate.py. Do not edit manually.
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package provider

import (
\t"context"
\t"fmt"

\t"github.com/hashicorp/terraform-plugin-framework/attr"
\t"github.com/hashicorp/terraform-plugin-framework/diag"
\t"github.com/hashicorp/terraform-plugin-framework/datasource"
\t"github.com/hashicorp/terraform-plugin-framework/datasource/schema"
\t"github.com/hashicorp/terraform-plugin-framework/types"
\t"github.com/hashicorp/terraform-plugin-framework/types/basetypes"
)

var _ datasource.DataSource = &{rp}DataSource{{}}
var _ datasource.DataSourceWithConfigure = &{rp}DataSource{{}}

func New{rp}DataSource() datasource.DataSource {{
\treturn &{rp}DataSource{{}}
}}

type {rp}DataSource struct {{
\tclient *Client
}}

type {rp}DataSourceModel struct {{
{struct_fields}
}}

{nested_structs}
func (d *{rp}DataSource) Metadata(_ context.Context, req datasource.MetadataRequest, resp *datasource.MetadataResponse) {{
\tresp.TypeName = req.ProviderTypeName + "_{rn}"
}}

func (d *{rp}DataSource) Schema(_ context.Context, _ datasource.SchemaRequest, resp *datasource.SchemaResponse) {{
\tresp.Schema = schema.Schema{{
\t\tDescription: "{desc}",
\t\tAttributes: map[string]schema.Attribute{{
\t\t\t{schema_attrs}
\t\t}},
\t}}
}}

func (d *{rp}DataSource) Configure(_ context.Context, req datasource.ConfigureRequest, resp *datasource.ConfigureResponse) {{
\tif req.ProviderData == nil {{
\t\treturn
\t}}
\tclient, ok := req.ProviderData.(*Client)
\tif !ok {{
\t\tresp.Diagnostics.AddError(
\t\t\t"Unexpected DataSource Configure Type",
\t\t\tfmt.Sprintf("Expected *Client, got: %T.", req.ProviderData),
\t\t)
\t\treturn
\t}}
\td.client = client
}}

func (d *{rp}DataSource) Read(ctx context.Context, req datasource.ReadRequest, resp *datasource.ReadResponse) {{
\tvar data {rp}DataSourceModel
\tresp.Diagnostics.Append(req.Config.Get(ctx, &data)...)
\tif resp.Diagnostics.HasError() {{
\t\treturn
\t}}

{read_body}

\tresp.Diagnostics.Append(resp.State.Set(ctx, &data)...)
}}

func (d *{rp}DataSource) populateModel(ctx context.Context, data *{rp}DataSourceModel, result map[string]interface{{}}, diags *diag.Diagnostics) {{
\t{populate}
}}
''',
        rp=resource_pascal, rn=resource_name, desc=description,
        struct_fields=struct_fields,
        nested_structs=nested_structs + '\n' if nested_struct_lines else '',
        schema_attrs=schema_attrs, read_body=read_body, populate=populate,
    )


# ---------------------------------------------------------------------------
# Registry file generation
# ---------------------------------------------------------------------------

def generate_registry_file(resource_names, datasource_names):
    resource_constructors = '\n\t\t'.join(
        'New%sResource,' % snake_to_pascal(name) for name in sorted(resource_names)
    )
    datasource_constructors = '\n\t\t'.join(
        'New%sDataSource,' % snake_to_pascal(name) for name in sorted(datasource_names)
    )
    return '''\
// Code generated by tools/generate/generate.py. Do not edit manually.
// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

package provider

import (
\t"github.com/hashicorp/terraform-plugin-framework/datasource"
\t"github.com/hashicorp/terraform-plugin-framework/resource"
)

func providerResources() []func() resource.Resource {
\treturn []func() resource.Resource{
\t\t%s
\t}
}

func providerDataSources() []func() datasource.DataSource {
\treturn []func() datasource.DataSource{
\t\t%s
\t}
}
''' % (resource_constructors, datasource_constructors)


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------

def generate(spec, output_dir, root=None, dry_run=False, **kwargs):
    """Generate all Terraform provider Go files from the OpenAPI spec."""
    spec_version = spec.get('info', {}).get('version', 'unknown')
    print('Spec version: %s' % spec_version)

    if root is None:
        root = os.path.dirname(os.path.dirname(os.path.abspath(output_dir)))

    groups = group_paths_by_tag(spec, SKIP_PATHS, SKIP_TAGS, tag_to_resource_name)

    if not dry_run:
        os.makedirs(output_dir, exist_ok=True)

    resource_names = []
    datasource_names = []
    generated = []

    for tag, group in sorted(groups.items()):
        # common.py uses module_name; alias it as resource_name for this backend
        resource_name = group['module_name']
        group = dict(group, resource_name=resource_name)
        overrides = RESOURCE_OVERRIDES.get(resource_name, {})
        is_read_only = tag in READ_ONLY_TAGS
        resource_info = analyze_resource(tag, group, spec)
        resource_info['resource_name'] = resource_info.pop('module_name', resource_name)

        has_readable = resource_info.get('has_list') or resource_info.get('has_get')
        has_writable = resource_info.get('has_create') or resource_info.get('has_update') or resource_info.get('has_delete')

        if is_read_only:
            if has_readable:
                filename = 'data_source_%s.go' % resource_name
                datasource_names.append(resource_name)
                if dry_run:
                    print('  [data-source] %s' % filename)
                else:
                    code = generate_datasource_file(resource_info, spec, overrides)
                    with open(os.path.join(output_dir, filename), 'w') as fh:
                        fh.write(code)
                    print('  Generated %s' % filename)
                    generated.append(filename)
        else:
            if has_writable:
                filename = 'resource_%s.go' % resource_name
                resource_names.append(resource_name)
                if dry_run:
                    print('  [resource] %s' % filename)
                else:
                    code = generate_resource_file(resource_info, spec, overrides)
                    with open(os.path.join(output_dir, filename), 'w') as fh:
                        fh.write(code)
                    print('  Generated %s' % filename)
                    generated.append(filename)

            if has_readable:
                filename = 'data_source_%s.go' % resource_name
                datasource_names.append(resource_name)
                if dry_run:
                    print('  [data-source] %s' % filename)
                else:
                    code = generate_datasource_file(resource_info, spec, overrides)
                    with open(os.path.join(output_dir, filename), 'w') as fh:
                        fh.write(code)
                    print('  Generated %s' % filename)
                    generated.append(filename)

    if not dry_run:
        registry_code = generate_registry_file(resource_names, datasource_names)
        registry_path = os.path.join(output_dir, 'resources_registry.go')
        with open(registry_path, 'w') as fh:
            fh.write(registry_code)
        print('  Generated resources_registry.go')
        generated.append('resources_registry.go')

    print('\nGenerated %d Go files (%d resources, %d data sources).' % (
        len(generated), len(resource_names), len(datasource_names)))
    return generated
