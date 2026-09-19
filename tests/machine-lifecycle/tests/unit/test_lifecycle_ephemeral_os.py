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

from lib import network_resources, nico_rest
from tests.lifecycle import machine_lifecycle_test as lifecycle


def test_disconnected_uuid_collection_does_not_require_static_os(monkeypatch):
    def static_os_lookup_must_not_run(_name):
        raise AssertionError("disconnected provisioning must not look up the static OS")

    monkeypatch.setattr(
        nico_rest, "get_operating_system_uuid", static_os_lookup_must_not_run
    )

    resources = SimpleNamespace(
        vpc_name="test-vpc", vpc_prefix_name="test-vpc-prefix"
    )
    reconciled = network_resources.NetworkResourceIDs(
        vpc_uuid="vpc-id",
        vpc_prefix_uuid="vpc-prefix-id",
    )

    cloud_resource_ids = lifecycle.collect_ngc_uuids(
        resources,
        "site-id",
        reconciled,
    )

    assert cloud_resource_ids.os_uuid is None


def test_temporary_os_cleanup_recovers_missing_uuid_by_name(monkeypatch):
    calls = []
    monkeypatch.setattr(
        nico_rest,
        "get_operating_system_uuid",
        lambda name: calls.append(("lookup", name)) or "recovered-os-id",
    )
    monkeypatch.setattr(
        nico_rest,
        "delete_operating_system",
        lambda uuid, strict: calls.append(("delete", uuid, strict)) or True,
    )

    lifecycle._cleanup_temporary_operating_system("mlt-os-test", None)

    assert calls == [
        ("lookup", "mlt-os-test"),
        ("delete", "recovered-os-id", False),
    ]
