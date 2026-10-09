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

import os

import pytest

SUPPORTED_MACHINE_PROFILES = frozenset({"dell", "gb200", "lenovo", "supermicro", "vr"})


class MachineProfileError(ValueError):
    """Raised when the configured machine profile is missing or unsupported."""


def _machine_profile() -> str:
    """Return the explicitly configured machine profile."""
    configured_profile = os.environ.get("MACHINE_PROFILE")
    if configured_profile is None:
        raise MachineProfileError(
            "$MACHINE_PROFILE must be provided when running via pytest"
        )

    normalized = configured_profile.strip().lower()
    if normalized not in SUPPORTED_MACHINE_PROFILES:
        supported = ", ".join(sorted(SUPPORTED_MACHINE_PROFILES))
        raise MachineProfileError(
            f"Unsupported machine profile {configured_profile!r}; expected one of: "
            f"{supported}"
        )
    return normalized


def pytest_collection_modifyitems(config, items):
    """Pytest hook to filter test selection.

    In test run mode: only collect the test that matches the configured machine
    profile.

    In collect-only mode: skip filtering so all are collected.
    """
    # Don't filter during collection-only mode
    if config.getoption("--collect-only", default=False):
        return

    machine = os.environ.get("MACHINE_UNDER_TEST")
    if machine is None:
        pytest.exit("$MACHINE_UNDER_TEST must be provided", returncode=1)
    print(f"Machine: {machine}")

    try:
        machine_profile = _machine_profile()
    except MachineProfileError as error:
        pytest.exit(str(error), returncode=1)
    print(f"Machine profile: {machine_profile}")

    expected_test_name = f"test_machine_lifecycle_{machine_profile}"

    # Filter to only keep the test function matching this machine profile.
    items[:] = [item for item in items if item.name == expected_test_name]

    if not items:
        pytest.exit(
            f"Test function {expected_test_name} not found in pytest collection",
            returncode=1,
        )
