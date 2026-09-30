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

from types import SimpleNamespace

import pytest

from tests.lifecycle import conftest as lifecycle_conftest


def test_machine_profile_is_read_from_explicit_input(monkeypatch):
    monkeypatch.setenv("MACHINE_PROFILE", " Dell ")

    assert lifecycle_conftest._machine_profile() == "dell"


def test_machine_profile_is_required(monkeypatch):
    monkeypatch.delenv("MACHINE_PROFILE", raising=False)

    with pytest.raises(
        lifecycle_conftest.MachineProfileError,
        match="must be provided when running via pytest",
    ):
        lifecycle_conftest._machine_profile()


def test_machine_profile_rejects_unknown_value(monkeypatch):
    monkeypatch.setenv("MACHINE_PROFILE", "dell_bf3")

    with pytest.raises(
        lifecycle_conftest.MachineProfileError,
        match="Unsupported machine profile 'dell_bf3'",
    ):
        lifecycle_conftest._machine_profile()


def test_machine_profile_selects_matching_pytest_wrapper(monkeypatch):
    monkeypatch.setenv("MACHINE_UNDER_TEST", "machine-id")
    monkeypatch.setenv("MACHINE_PROFILE", "supermicro")
    config = SimpleNamespace(getoption=lambda *args, **kwargs: False)
    items = [
        SimpleNamespace(name="test_machine_lifecycle_lenovo"),
        SimpleNamespace(name="test_machine_lifecycle_supermicro"),
    ]

    lifecycle_conftest.pytest_collection_modifyitems(config, items)

    assert [item.name for item in items] == ["test_machine_lifecycle_supermicro"]
