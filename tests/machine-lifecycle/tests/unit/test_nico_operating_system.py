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

import json

import pytest
import requests

from lib import nico_rest


def _response(payload: object) -> requests.Response:
    response = requests.Response()
    response.status_code = 200
    response._content = json.dumps(payload).encode()
    response.headers["Content-Type"] = "application/json"
    return response


def test_create_operating_system_uses_tenant_and_expected_fields(monkeypatch):
    monkeypatch.setattr(nico_rest, "get_tenant_uuid", lambda: "tenant-id")
    captured = {}

    def capture(method, path, *, params=None, body=None):
        captured.update(method=method, path=path, params=params, body=body)
        return _response({"id": "os-id", "status": "Ready"})

    monkeypatch.setattr(nico_rest, "_request", capture)

    result = nico_rest.create_operating_system(
        "mlt-os-test",
        "#!ipxe",
        "#cloud-config",
        description="temporary",
    )

    assert result["id"] == "os-id"
    assert captured == {
        "method": "POST",
        "path": "operating-system",
        "params": None,
        "body": {
            "name": "mlt-os-test",
            "tenantId": "tenant-id",
            "ipxeScript": "#!ipxe",
            "userData": "#cloud-config",
            "allowOverride": True,
            "phoneHomeEnabled": True,
            "description": "temporary",
        },
    }


def test_create_operating_system_rejects_response_without_id(monkeypatch):
    monkeypatch.setattr(nico_rest, "get_tenant_uuid", lambda: "tenant-id")
    monkeypatch.setattr(
        nico_rest,
        "_request",
        lambda *_args, **_kwargs: _response({"status": "Creating"}),
    )

    with pytest.raises(nico_rest.NicoError, match="missing a non-empty 'id'"):
        nico_rest.create_operating_system("mlt-os-test", "#!ipxe", "#cloud-config")


def test_wait_for_operating_system_ready(monkeypatch):
    statuses = iter([{"status": "Creating"}, {"status": "Ready"}])
    monkeypatch.setattr(
        nico_rest, "get_operating_system", lambda _uuid: next(statuses)
    )
    monkeypatch.setattr(nico_rest.time, "sleep", lambda _seconds: None)

    nico_rest.wait_for_operating_system_ready("os-id", timeout=5, poll_interval=0)


def test_wait_for_operating_system_ready_rejects_terminal_failure(monkeypatch):
    monkeypatch.setattr(
        nico_rest,
        "get_operating_system",
        lambda _uuid: {"status": "Error", "message": "bad"},
    )

    with pytest.raises(nico_rest.NicoError, match="terminal status Error"):
        nico_rest.wait_for_operating_system_ready("os-id", timeout=5, poll_interval=0)


@pytest.mark.parametrize("status", [None, "", "   ", 123])
def test_wait_for_operating_system_ready_rejects_invalid_status(monkeypatch, status):
    payload = {"status": status, "message": "unexpected response"}
    monkeypatch.setattr(nico_rest, "get_operating_system", lambda _uuid: payload)

    with pytest.raises(nico_rest.NicoError, match="invalid status payload"):
        nico_rest.wait_for_operating_system_ready(
            "os-id", timeout=5, poll_interval=0
        )


def test_best_effort_operating_system_delete(monkeypatch):
    captured = []

    def fail(method, path, *, params=None, body=None):
        captured.append((method, path, params, body))
        raise nico_rest.NicoError("delete failed")

    monkeypatch.setattr(nico_rest, "_request", fail)

    assert nico_rest.delete_operating_system("os-id", strict=False) is False
    assert captured == [("DELETE", "operating-system/os-id", None, None)]


def test_list_operating_systems_passes_search_query(monkeypatch):
    captured = []
    monkeypatch.setattr(
        nico_rest,
        "_list",
        lambda resource, params=None: captured.append((resource, params)) or [],
    )

    assert nico_rest.list_operating_systems(
        query="mlt-os-", operating_system_type="iPXE"
    ) == []
    assert captured == [
        ("operating-system", {"query": "mlt-os-", "type": "iPXE"})
    ]


def test_list_instances_for_operating_system_uses_server_filter(monkeypatch):
    captured = []
    expected = [{"id": "instance-id", "operatingSystemId": "os-id"}]
    monkeypatch.setattr(
        nico_rest,
        "_list",
        lambda resource, params=None: captured.append((resource, params)) or expected,
    )

    assert nico_rest.list_instances_for_operating_system("os-id") == expected
    assert captured == [("instance", {"operatingSystemId": "os-id"})]


def test_list_instances_for_operating_system_rejects_unexpected_results(monkeypatch):
    monkeypatch.setattr(
        nico_rest,
        "_list",
        lambda _resource, params=None: [
            {"id": "other-instance", "operatingSystemId": "other-os"}
        ],
    )

    with pytest.raises(nico_rest.NicoError, match="unrelated or malformed"):
        nico_rest.list_instances_for_operating_system("os-id")
