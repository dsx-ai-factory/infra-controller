# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

from __future__ import absolute_import, division, print_function
__metaclass__ = type


class ModuleDocFragment(object):
    DOCUMENTATION = r'''
options:
  api_url:
    description:
      - Base URL of the NICo REST API.
      - Can also be set via the C(NVIDIA_NICO_API_URL) environment variable.
    type: str
    required: true
  api_token:
    description:
      - Bearer token for authentication.
      - Can also be set via the C(NVIDIA_NICO_API_TOKEN) environment variable.
      - This value is treated as a secret and will not be logged.
    type: str
    required: true
  org:
    description:
      - Organization name.
      - Can also be set via the C(NVIDIA_NICO_ORG) environment variable.
    type: str
    required: true
  api_path_prefix:
    description:
      - Optional API path prefix.
      - Can also be set via the C(NVIDIA_NICO_API_PATH_PREFIX) environment variable.
    type: str
    default: nico
'''
