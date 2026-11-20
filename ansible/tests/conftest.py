# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0

"""Configure sys.path and module aliases for testing without a full Ansible installation."""

import os
import sys
import types

# Project root (ansible/ directory)
PROJECT_ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PLUGINS_DIR = os.path.join(PROJECT_ROOT, 'plugins')

# Add plugins to path for direct imports
sys.path.insert(0, PLUGINS_DIR)

# Create the ansible_collections.nvidia.infra_controller.plugins.module_utils namespace
# so that the fully-qualified imports in resource.py / client.py work
_ns_packages = [
    'ansible_collections',
    'ansible_collections.nvidia',
    'ansible_collections.nvidia.infra_controller',
    'ansible_collections.nvidia.infra_controller.plugins',
    'ansible_collections.nvidia.infra_controller.plugins.module_utils',
]

for pkg in _ns_packages:
    if pkg not in sys.modules:
        sys.modules[pkg] = types.ModuleType(pkg)
        sys.modules[pkg].__path__ = []

# Point the module_utils to our plugins/module_utils
import module_utils as _mu
sys.modules['ansible_collections.nvidia.infra_controller.plugins.module_utils'] = _mu
sys.modules['ansible_collections.nvidia.infra_controller.plugins.module_utils'].__path__ = _mu.__path__

# Also ensure ansible.module_utils.urls and ansible.module_utils.basic are available
# If ansible is not installed, create mocks
try:
    import ansible.module_utils.urls as _ansible_urls_mod  # registers in sys.modules
    del _ansible_urls_mod
except ImportError:
    ansible_mod = types.ModuleType('ansible')
    ansible_mod.__path__ = []
    ansible_mu = types.ModuleType('ansible.module_utils')
    ansible_mu.__path__ = []
    ansible_urls = types.ModuleType('ansible.module_utils.urls')
    ansible_basic = types.ModuleType('ansible.module_utils.basic')

    from unittest.mock import MagicMock
    ansible_urls.open_url = MagicMock()
    ansible_basic.AnsibleModule = MagicMock
    ansible_basic.env_fallback = MagicMock()

    # Mock ansible.module_utils.six.moves.urllib.parse for client.py
    from urllib.parse import quote as _real_quote
    ansible_six = types.ModuleType('ansible.module_utils.six')
    ansible_six.__path__ = []
    ansible_six_moves = types.ModuleType('ansible.module_utils.six.moves')
    ansible_six_moves.__path__ = []
    ansible_six_moves_urllib = types.ModuleType('ansible.module_utils.six.moves.urllib')
    ansible_six_moves_urllib.__path__ = []
    ansible_six_moves_urllib_parse = types.ModuleType('ansible.module_utils.six.moves.urllib.parse')
    ansible_six_moves_urllib_parse.quote = _real_quote

    sys.modules['ansible'] = ansible_mod
    sys.modules['ansible.module_utils'] = ansible_mu
    sys.modules['ansible.module_utils.urls'] = ansible_urls
    sys.modules['ansible.module_utils.basic'] = ansible_basic
    sys.modules['ansible.module_utils.six'] = ansible_six
    sys.modules['ansible.module_utils.six.moves'] = ansible_six_moves
    sys.modules['ansible.module_utils.six.moves.urllib'] = ansible_six_moves_urllib
    sys.modules['ansible.module_utils.six.moves.urllib.parse'] = ansible_six_moves_urllib_parse
