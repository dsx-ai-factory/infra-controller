# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
"""Load a deployment script and its sibling helpers without changing sys.path."""
import importlib.util
from pathlib import Path
import sys
from unittest.mock import patch


def load_script(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    with patch.object(sys, 'path', [str(Path(path).parent), *sys.path]):
        spec.loader.exec_module(module)
    return module
