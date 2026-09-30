# SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
# SPDX-License-Identifier: Apache-2.0
#
# Licensed under the Apache License, Version 2.0 (the "License");
# you may not use this file except in compliance with the License.
# You may obtain a copy of the License at
#
# http://www.apache.org/licenses/LICENSE-2.0
#
# Unless required by applicable law or agreed to in writing, software
# distributed under the License is distributed on an "AS IS" BASIS,
# WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
# See the License for the specific language governing permissions and
# limitations under the License.

import pytest

from .machine_lifecycle_test import test_machine_lifecycle as _test_machine_lifecycle

# Wrapper functions that call the actual test script


@pytest.mark.T5566186
def test_machine_lifecycle_lenovo():
    _test_machine_lifecycle()


@pytest.mark.T5566188
def test_machine_lifecycle_dell():
    _test_machine_lifecycle()


@pytest.mark.T5566199
def test_machine_lifecycle_supermicro():
    _test_machine_lifecycle()


@pytest.mark.T5674437
def test_machine_lifecycle_gb200():
    _test_machine_lifecycle()


# TODO: add test ID
def test_machine_lifecycle_vr():
    _test_machine_lifecycle()
