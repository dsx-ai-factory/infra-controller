# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from __future__ import absolute_import, division, print_function
__metaclass__ = type

import re
import time
from urllib.parse import quote as urlquote

from ansible_collections.nvidia.infra_controller.plugins.module_utils.client import NicoClient
from ansible_collections.nvidia.infra_controller.plugins.module_utils.common import (
    camel_to_snake,
    snake_to_camel,
    convert_keys,
    OPAQUE_DICT_FIELDS,
)


def _to_api_key(field_map, key):
    """Return the API-side camelCase name for a snake_case argument key.

    Consults field_map first so acronym-heavy names (DPU, IB, SSH, …) round-trip
    correctly; falls back to generic snake_to_camel for everything else.
    """
    return field_map.get(key, snake_to_camel(key))


def _to_api_body(field_map, data, opaque_fields=None):
    """Recursively convert snake_case keys in an outbound payload to camelCase.

    Uses field_map to preserve OpenAPI acronym spellings. Keys listed in
    OPAQUE_DICT_FIELDS (global, e.g. 'labels') or opaque_fields (per-resource,
    e.g. 'annotations', 'values') are treated as opaque — their child keys are
    user-defined and must not be converted.
    """
    effective_opaque = OPAQUE_DICT_FIELDS if not opaque_fields else OPAQUE_DICT_FIELDS | opaque_fields
    if isinstance(data, dict):
        result = {}
        for k, v in data.items():
            api_key = _to_api_key(field_map, k)
            if api_key in effective_opaque or k in effective_opaque:
                result[api_key] = v
            else:
                result[api_key] = _to_api_body(field_map, v, opaque_fields)
        return result
    if isinstance(data, list):
        return [_to_api_body(field_map, item, opaque_fields) for item in data]
    return data


# Matches {someParam} placeholders in URL paths.
_PATH_PARAM_RE = re.compile(r'\{(\w+)\}')


def _resolve_path(path, module_params, org):
    """Substitute all {param} placeholders in a path using module params.

    {org} is replaced with the org value directly.
    Other placeholders like {allocationId} are resolved by converting the
    placeholder name to snake_case and looking it up in module_params.
    """
    def _replace(match):
        param_name = match.group(1)
        if param_name == 'org':
            return str(org)
        snake_name = camel_to_snake(param_name)
        value = module_params.get(snake_name)
        if value is None:
            return match.group(0)  # Leave unresolved
        return urlquote(str(value), safe='')

    return _PATH_PARAM_RE.sub(_replace, path)


def _assert_path_resolved(module, path):
    """Fail with a clear error if any {placeholder} remains unresolved."""
    unresolved = _PATH_PARAM_RE.findall(path)
    if unresolved:
        module.fail_json(
            msg='Missing required path parameter(s): %s. '
                'Provide %s to complete the request path.'
                % (', '.join('{%s}' % p for p in unresolved),
                   ', '.join(camel_to_snake(p) for p in unresolved)),
        )


def _prune_none(value):
    """Recursively drop None values from dicts (unset Ansible suboptions)."""
    if isinstance(value, dict):
        return {k: _prune_none(v) for k, v in value.items() if v is not None}
    if isinstance(value, list):
        return [_prune_none(item) for item in value]
    return value


def _values_differ(desired, existing):
    """Compare a user-provided value against the API response value.

    For simple types, does a direct comparison.  For lists of dicts
    (e.g., interfaces), compares only the keys the user provided —
    the API response includes read-only fields (id, status, created, etc.)
    that the user never specifies.
    """
    if isinstance(desired, dict) and isinstance(existing, dict):
        # Compare only the keys the user set — the API response includes
        # read-only fields (id, status, createdAt, …) the user never specifies.
        for k, v in desired.items():
            if _values_differ(v, existing.get(k)):
                return True
        return False
    if isinstance(desired, list) and isinstance(existing, list):
        if len(desired) != len(existing):
            return True
        for d_item, e_item in zip(desired, existing):
            if _values_differ(d_item, e_item):
                return True
        return False
    return desired != existing


def _init_base(obj, module, config):
    """Shared initialisation for all resource classes."""
    obj.module = module
    obj.config = config
    obj.client = NicoClient(module)
    obj.field_map = config.get('field_map', {})
    obj.opaque_fields = set(config.get('opaque_fields', []))


class CrudResource(object):
    """Base class for CRUD resource modules (state=present/absent)."""

    def __init__(self, module, config):
        _init_base(self, module, config)

        self.resource_path = config['resource_path']
        self.resource_item_path = config['resource_item_path']
        self.id_param = config.get('id_param', 'id')
        self.name_field = config.get('name_field', 'name')
        self.create_fields = config.get('create_schema_fields', [])
        self.update_fields = config.get('update_schema_fields', [])
        self.scope_fields = config.get('scope_fields', [])
        self.ready_statuses = config.get('ready_statuses', ['Ready'])
        self.error_statuses = config.get('error_statuses', ['Error'])
        self.no_create = config.get('no_create', False)
        self.no_wait = config.get('no_wait', False)
        self.create_method = config.get('create_method', 'POST')
        self.has_get_by_id = config.get('has_get_by_id', True)
        self.delete_body_fields = config.get('delete_body_fields', [])
        self.version_field = config.get('version_field')
        self.update_method = config.get('update_method', 'PATCH')
        self.required_create_fields = config.get('required_create_fields', [])

    def _resolve_collection_path(self):
        """Resolve all path parameters in the collection path."""
        path = _resolve_path(self.resource_path, self.module.params, self.client.org)
        _assert_path_resolved(self.module, path)
        return path

    def _resolve_item_path(self, resource_id):
        """Resolve all path parameters in the item path, including the resource id."""
        params = dict(self.module.params)
        params[camel_to_snake(self.id_param)] = resource_id
        path = _resolve_path(self.resource_item_path, params, self.client.org)
        _assert_path_resolved(self.module, path)
        return path

    def run(self):
        state = self.module.params.get('state') or 'present'
        existing = self._find_existing()

        if state == 'present':
            self._handle_present(existing)
        elif state == 'absent':
            self._handle_absent(existing)

    def _find_existing(self):
        """Find an existing resource by id or by name within scope."""
        params = self.module.params

        # Direct lookup by id — only when the item path supports GET.
        # Some resources (e.g. allocation_constraint) have no GET on the item
        # path; fall through to list+filter for those.
        resource_id = params.get('id')
        if resource_id and self.has_get_by_id:
            path = self._resolve_item_path(resource_id)
            return self.client.get(path)

        # Singleton sub-resource: no id, no name, update-only (no_create=True),
        # no item path. GET the collection path to read the current state.
        # Guard: only for no_create resources — regular collections with no item
        # path (e.g. credential_rotation) must NOT be treated as singletons.
        if self.no_create and not self.name_field and not resource_id and self.resource_path and not self.resource_item_path:
            path = self._resolve_collection_path()
            _assert_path_resolved(self.module, path)
            return self.client.get(path)

        # Lookup by name (list + filter)
        if not self.name_field:
            return None
        name = params.get(self.name_field)
        if not name:
            return None

        query_params = {}
        for field in self.scope_fields:
            val = params.get(field)
            if val:
                query_params[_to_api_key(self.field_map, field)] = val

        collection_path = self._resolve_collection_path()
        resources = self.client.list_all(collection_path, params=query_params)
        camel_name_field = snake_to_camel(self.name_field)
        matches = [r for r in resources if r.get(camel_name_field) == name]

        # Also filter by scope fields on the response in case the API
        # doesn't filter server-side. Only apply when the response actually
        # contains the field — some resources (e.g., SshKey) don't include
        # the parent ID in their response schema.
        for field in self.scope_fields:
            val = params.get(field)
            if val:
                camel_field = _to_api_key(self.field_map, field)
                matches = [r for r in matches
                           if camel_field not in r or r.get(camel_field) == val]

        if len(matches) > 1:
            self.module.fail_json(
                msg='Found %d resources matching %s=%s. Use id to specify which one.' % (
                    len(matches), self.name_field, name,
                ),
            )
            return None
        return matches[0] if matches else None

    def _handle_present(self, existing):
        params = self.module.params
        wait = params.get('wait')
        if wait is None:
            wait = True
        wait_timeout = params.get('wait_timeout')
        if wait_timeout is None:
            wait_timeout = 600

        if existing:
            # Normalize to snake_case so nested structures (interfaces, labels)
            # match the user-supplied arg names before diffing.
            existing_snake = convert_keys(existing, camel_to_snake, self.opaque_fields)

            update_data = self._build_payload(self.update_fields)

            # Don't diff the version field — it's for concurrency control,
            # not a user-visible attribute.
            version_field = self.version_field
            update_data.pop(version_field, None) if version_field else None

            changes = {}
            for key, value in update_data.items():
                existing_value = existing_snake.get(key)
                if value is not None and existing_value is None:
                    # Field provided by user but absent from the response — this
                    # is a write-only field (e.g. site_ids vs site_associations).
                    # We cannot compare it, so always include the user-provided
                    # value in the update payload.
                    changes[key] = value
                elif value is not None and existing_value is not None:
                    if _values_differ(value, existing_value):
                        changes[key] = value

            if not changes:
                self.module.exit_json(
                    changed=False,
                    resource=existing_snake,
                    diff={'before': existing_snake, 'after': existing_snake},
                )
                return

            if self.module.check_mode:
                projected = dict(existing_snake)
                projected.update(changes)
                self.module.exit_json(
                    changed=True,
                    resource=existing_snake,
                    diff={'before': existing_snake, 'after': projected},
                )
                return

            # Inject the version from the existing resource if the API requires
            # it for optimistic concurrency control.
            if version_field:
                version_value = existing_snake.get(version_field)
                if version_value:
                    changes[version_field] = version_value

            # field_map preserves acronym spellings (e.g. fallbackDPUSerialNumbers).
            camel_changes = _to_api_body(self.field_map, changes, self.opaque_fields)

            resource_id = existing.get('id')
            path = self._resolve_item_path(resource_id) if self.resource_item_path else self._resolve_collection_path()
            _assert_path_resolved(self.module, path)
            if self.update_method == 'PUT':
                result = self.client.put(path, camel_changes)
            else:
                result = self.client.update(path, camel_changes)

            if wait and not self.no_wait and self._should_wait(result):
                result = self._wait_for_ready(result, wait_timeout)

            after = convert_keys(result, camel_to_snake, self.opaque_fields) if result else {}
            self.module.exit_json(
                changed=True,
                resource=after,
                diff={'before': existing_snake, 'after': after},
            )
        else:
            if self.no_create:
                # Update-only resources with no list/GET (e.g. machine_power)
                # bypass the create branch and PATCH the collection path directly.
                if self.no_wait and self.update_fields:
                    update_data = self._build_payload(self.update_fields)
                    camel_payload = _to_api_body(self.field_map, update_data, self.opaque_fields)
                    if self.module.check_mode:
                        self.module.exit_json(changed=True, resource={},
                                              diff={'before': {}, 'after': {}})
                        return
                    path = self._resolve_collection_path()
                    result = self.client.update(path, camel_payload)
                    after = convert_keys(result, camel_to_snake, self.opaque_fields) if result else {}
                    self.module.exit_json(changed=True, resource=after,
                                          diff={'before': {}, 'after': after})
                    return
                self.module.fail_json(
                    msg='Resource not found. This resource type does not support creation; id is required.',
                )
                return

            # Validate required create fields before sending — catches missing
            # required params as a module error rather than an opaque API 422.
            missing = [f for f in self.required_create_fields
                       if self.module.params.get(f) is None]
            if missing:
                self.module.fail_json(
                    msg='The following fields are required to create this resource: %s'
                        % ', '.join(missing),
                )
                return

            create_data = self._build_payload(self.create_fields)
            create_body = _to_api_body(self.field_map, create_data, self.opaque_fields)

            if self.module.check_mode:
                self.module.exit_json(
                    changed=True,
                    resource={},
                    diff={'before': {}, 'after': {}},
                )
                return

            collection_path = self._resolve_collection_path()
            if self.create_method == "PUT":
                result = self.client.put(collection_path, create_body)
            else:
                result = self.client.create(collection_path, create_body)

            if wait and not self.no_wait and self._should_wait(result):
                result = self._wait_for_ready(result, wait_timeout)

            after = convert_keys(result, camel_to_snake, self.opaque_fields) if result else {}
            self.module.exit_json(
                changed=True,
                resource=after,
                diff={'before': {}, 'after': after},
            )

    def _handle_absent(self, existing):
        if not existing:
            # No GET endpoint — cannot confirm existence before deleting.
            # Treat a 404 response as "already absent".
            can_probe = self.has_get_by_id or self.name_field
            if can_probe:
                self.module.exit_json(changed=False)
                return
            if self.module.check_mode:
                self.module.exit_json(changed=True, diff={'before': {}, 'after': {}})
                return
            path = _resolve_path(self.resource_item_path or self.resource_path,
                                 self.module.params, self.client.org)
            _assert_path_resolved(self.module, path)
            self.client.delete(path)
            self.module.exit_json(changed=True, diff={'before': {}, 'after': {}})
            return

        # If the resource is already being deleted (Terminating, Deleting, etc.),
        # don't issue another DELETE — just wait for it to finish.
        status = existing.get('status', '')
        already_deleting = status in ('Terminating', 'Terminated', 'Deleting')

        if already_deleting:
            wait = self.module.params.get('wait')
            if wait is None:
                wait = True
            wait_timeout = self.module.params.get('wait_timeout')
            if wait_timeout is None:
                wait_timeout = 600
            if wait:
                resource_id = existing.get('id')
                self._wait_for_deleted(resource_id, wait_timeout)
            self.module.exit_json(changed=False)
            return

        before = convert_keys(existing, camel_to_snake, self.opaque_fields)
        if self.module.check_mode:
            self.module.exit_json(
                changed=True,
                diff={'before': before, 'after': {}},
            )
            return

        resource_id = existing.get('id')
        path = self._resolve_item_path(resource_id)

        delete_body = None
        if self.delete_body_fields:
            raw_body = {}
            for field in self.delete_body_fields:
                val = self.module.params.get(field)
                if val is not None:
                    raw_body[field] = val
            if raw_body:
                delete_body = _to_api_body(self.field_map, raw_body, self.opaque_fields)

        self.client.delete(path, data=delete_body)

        wait = self.module.params.get('wait')
        if wait is None:
            wait = True
        wait_timeout = self.module.params.get('wait_timeout')
        if wait_timeout is None:
            wait_timeout = 600
        if wait:
            self._wait_for_deleted(resource_id, wait_timeout)

        self.module.exit_json(
            changed=True,
            diff={'before': before, 'after': {}},
        )

    def _should_wait(self, result):
        """Check whether we need to poll for the resource to become ready.

        Returns False if the result is missing, has no status field, or is
        already in a ready state.
        """
        if not result:
            return False
        status = result.get('status')
        if status is None:
            return False
        return status not in self.ready_statuses

    def _build_payload(self, fields):
        """Build a payload dict from module params for the given field list."""
        payload = {}
        for field in fields:
            val = self.module.params.get(field)
            if val is not None:
                payload[field] = _prune_none(val)
        return payload

    def _wait_for_ready(self, resource, timeout):
        """Poll until resource reaches a ready or error status."""
        resource_id = resource.get('id')
        path = self._resolve_item_path(resource_id)
        deadline = time.time() + timeout
        poll_interval = 5

        while time.time() < deadline:
            current = self.client.get(path)
            if current is None:
                self.module.fail_json(msg='Resource %s disappeared while waiting' % resource_id)
                return resource

            status = current.get('status', '')
            if status in self.ready_statuses:
                return current
            if status in self.error_statuses:
                self.module.fail_json(
                    msg='Resource %s reached error status: %s' % (resource_id, status),
                    resource=convert_keys(current, camel_to_snake, self.opaque_fields),
                )
                return current

            time.sleep(poll_interval)

        self.module.fail_json(
            msg='Timed out waiting for resource %s to become ready (status: %s)' % (
                resource_id, resource.get('status', 'unknown'),
            ),
        )
        return resource

    def _wait_for_deleted(self, resource_id, timeout):
        """Poll until resource returns 404."""
        path = self._resolve_item_path(resource_id)
        deadline = time.time() + timeout
        poll_interval = 5

        while time.time() < deadline:
            result = self.client.get(path)
            if result is None:
                return
            time.sleep(poll_interval)

        self.module.fail_json(
            msg='Timed out waiting for resource %s to be deleted' % resource_id,
        )


class InfoResource(object):
    """Base class for _info (read-only) modules."""

    def __init__(self, module, config):
        _init_base(self, module, config)
        self.resource_path = config['resource_path']
        self.resource_item_path = config.get('resource_item_path')
        self.id_param = config.get('id_param', 'id')
        self.filter_fields = config.get('filter_fields', [])

    def run(self):
        params = self.module.params
        resource_id = params.get('id')

        if resource_id and self.resource_item_path:
            item_params = dict(params)
            item_params[camel_to_snake(self.id_param)] = resource_id
            path = _resolve_path(
                self.resource_item_path, item_params, self.client.org,
            )
            # Forward filter fields on item GETs — some APIs require scope
            # parameters (e.g. siteId) even on single-resource lookups.
            item_query = {}
            for field in self.filter_fields:
                val = params.get(field)
                if val is not None:
                    item_query[_to_api_key(self.field_map, field)] = val
            if item_query:
                from urllib.parse import urlencode
                path = '%s?%s' % (path, urlencode(item_query))
            result = self.client.get(path)
            if result is None:
                self.module.fail_json(msg='Resource with id %s not found' % resource_id)
                return
            self.module.exit_json(
                changed=False,
                resource=convert_keys(result, camel_to_snake, self.opaque_fields),
            )
        else:
            query_params = {}
            for field in self.filter_fields:
                val = params.get(field)
                if val is not None:
                    query_params[_to_api_key(self.field_map, field)] = val

            collection_path = _resolve_path(
                self.resource_path, params, self.client.org,
            )
            results = self.client.list_all(collection_path, params=query_params)
            self.module.exit_json(
                changed=False,
                resources=[convert_keys(r, camel_to_snake, self.opaque_fields) for r in results],
            )


class ActionResource(object):
    """Base class for action modules (validation, power control, firmware updates)."""

    def __init__(self, module, config):
        _init_base(self, module, config)
        self.collection_path = config['resource_path']
        self.item_path = config.get('resource_item_path')
        self.method = config.get('method', 'PATCH')
        self.body_fields = config.get('body_fields', [])
        self.query_fields = config.get('query_fields', [])
        self.id_param = config.get('id_param', 'id')
        self.required_body_fields = config.get('required_body_fields', [])

    def run(self):
        params = self.module.params
        resource_id = params.get('id')

        if resource_id and self.item_path:
            item_params = dict(params)
            item_params[camel_to_snake(self.id_param)] = resource_id
            path = _resolve_path(self.item_path, item_params, self.client.org)
        else:
            path = _resolve_path(self.collection_path, params, self.client.org)

        if self.method == 'GET':
            # Build query string into path for read-only actions (e.g., validation)
            query_parts = []
            for field in self.query_fields:
                val = params.get(field)
                if val is not None:
                    query_parts.append('%s=%s' % (
                        urlquote(_to_api_key(self.field_map, field), safe=''),
                        urlquote(str(val), safe=''),
                    ))
            if query_parts:
                path = '%s?%s' % (path, '&'.join(query_parts))
            result = self.client.get(path)
            self.module.exit_json(
                changed=False,
                result=convert_keys(result, camel_to_snake, self.opaque_fields) if result else {},
            )
        else:
            missing = [f for f in self.required_body_fields if params.get(f) is None]
            if missing:
                self.module.fail_json(
                    msg='The following fields are required for this action: %s' % ', '.join(missing),
                )
                return
            if self.module.check_mode:
                self.module.exit_json(changed=True, result={})
                return
            body = {}
            for field in self.body_fields:
                val = params.get(field)
                if val is not None:
                    body[field] = val
            request_body = _to_api_body(self.field_map, body, self.opaque_fields)
            result = self.client.update(path, request_body)
            self.module.exit_json(
                changed=True,
                result=convert_keys(result, camel_to_snake, self.opaque_fields) if result else {},
            )


class BatchResource(object):
    """Base class for batch-create-only modules (e.g., instance_batch)."""

    def __init__(self, module, config):
        _init_base(self, module, config)
        self.resource_path = config['resource_path']
        self.create_fields = config.get('create_schema_fields', [])

    def run(self):
        params = self.module.params
        raw_body = {}
        for field in self.create_fields:
            val = params.get(field)
            if val is not None:
                raw_body[field] = val

        create_body = _to_api_body(self.field_map, raw_body, self.opaque_fields)

        if self.module.check_mode:
            self.module.exit_json(changed=True, resources=[])
            return

        collection_path = _resolve_path(
            self.resource_path, params, self.client.org,
        )
        result = self.client.create(collection_path, create_body)
        if not isinstance(result, list):
            result = [result] if result else []

        self.module.exit_json(
            changed=True,
            resources=[convert_keys(r, camel_to_snake, self.opaque_fields) for r in result],
        )
