# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from __future__ import absolute_import, division, print_function
__metaclass__ = type

import re


def camel_to_snake(name):
    """Convert camelCase or PascalCase to snake_case."""
    # Handle acronyms like 'VPC' -> 'vpc', 'NVLink' -> 'nv_link'
    s1 = re.sub(r'([A-Z]+)([A-Z][a-z])', r'\1_\2', name)
    s2 = re.sub(r'([a-z0-9])([A-Z])', r'\1_\2', s1)
    return s2.lower()


def snake_to_camel(name):
    """Convert snake_case to camelCase."""
    parts = name.split('_')
    return parts[0] + ''.join(p.capitalize() for p in parts[1:])


# Fields whose dict values have user-defined keys that must not be converted.
# These correspond to OpenAPI schemas with additionalProperties (e.g., Labels).
OPAQUE_DICT_FIELDS = frozenset({'labels'})


def convert_keys(data, converter, opaque_fields=None):
    """Recursively convert all dict keys using the given converter function.

    Keys listed in OPAQUE_DICT_FIELDS or opaque_fields are treated as opaque
    dicts whose child keys are user-defined and should not be converted.
    """
    effective_opaque = OPAQUE_DICT_FIELDS if not opaque_fields else OPAQUE_DICT_FIELDS | set(opaque_fields)
    if isinstance(data, dict):
        result = {}
        for k, v in data.items():
            new_key = converter(k)
            if new_key in effective_opaque or k in effective_opaque:
                # Preserve child keys as-is for opaque dicts
                result[new_key] = v
            else:
                result[new_key] = convert_keys(v, converter, opaque_fields)
        return result
    if isinstance(data, list):
        return [convert_keys(item, converter, opaque_fields) for item in data]
    return data


def get_auth_argument_spec():
    """Return auth argument spec with env var fallbacks.

    Separated into a function so the import of env_fallback
    is deferred until Ansible is available.
    """
    from ansible.module_utils.basic import env_fallback
    return dict(
        api_url=dict(
            type='str',
            required=True,
            fallback=(env_fallback, ['NVIDIA_NICO_API_URL']),
        ),
        api_token=dict(
            type='str',
            required=True,
            no_log=True,
            fallback=(env_fallback, ['NVIDIA_NICO_API_TOKEN']),
        ),
        org=dict(
            type='str',
            required=True,
            fallback=(env_fallback, ['NVIDIA_NICO_ORG']),
        ),
        api_path_prefix=dict(
            type='str',
            default='nico',
            fallback=(env_fallback, ['NVIDIA_NICO_API_PATH_PREFIX']),
        ),
    )
