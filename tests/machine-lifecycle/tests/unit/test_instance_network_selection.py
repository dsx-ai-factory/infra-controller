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
from types import SimpleNamespace

import requests

from lib import network_resources, nico_rest
from tests.lifecycle import machine_lifecycle_test as lifecycle


def _response(payload):
    response = requests.Response()
    response.status_code = 200
    response._content = json.dumps(payload).encode()
    response.headers["Content-Type"] = "application/json"
    return response


def test_create_instance_sends_auto_vpc_selector(monkeypatch):
    captured = {}

    def fake_request(method, path, *, params=None, body=None):
        captured.update(method=method, path=path, params=params, body=body)
        return _response({"id": "instance-id"})

    monkeypatch.setattr(nico_rest, "get_tenant_uuid", lambda: "tenant-id")
    monkeypatch.setattr(nico_rest, "_request", fake_request)

    selector = {"vpcId": "vpc-id", "ipFamilies": ["IPv4"]}
    result = nico_rest.create_instance(
        instance_name="instance-name",
        machine_id="machine-id",
        network_interface=selector,
        operating_system_uuid="os-id",
        virtual_private_cloud_uuid="vpc-id",
    )

    assert result == {"id": "instance-id"}
    assert captured["method"] == "POST"
    assert captured["path"] == "instance"
    assert captured["body"]["interfaces"] == [selector]
    assert captured["body"]["machineId"] == "machine-id"
    assert "instanceTypeId" not in captured["body"]


def test_get_instance_ip_matches_auto_vpc_selector(monkeypatch):
    monkeypatch.setattr(
        nico_rest,
        "get_instance_info",
        lambda instance_id: {
            "id": instance_id,
            "vpcId": "vpc-id",
            "interfaces": [
                {
                    "vpcPrefixId": "selected-prefix-id",
                    "ipAddresses": ["192.0.2.3"],
                }
            ],
        },
    )

    assert (
        nico_rest.get_instance_ip(
            "instance-id", {"vpcId": "vpc-id", "ipFamilies": ["IPv4"]}
        )
        == "192.0.2.3"
    )


def test_get_instance_ip_does_not_guess_between_auto_selected_interfaces(monkeypatch):
    monkeypatch.setattr(
        nico_rest,
        "get_instance_info",
        lambda instance_id: {
            "id": instance_id,
            "vpcId": "vpc-id",
            "interfaces": [
                {
                    "vpcPrefixId": "first-prefix-id",
                    "ipAddresses": ["192.0.2.3"],
                },
                {
                    "vpcPrefixId": "second-prefix-id",
                    "ipAddresses": ["192.0.2.4"],
                },
            ],
        },
    )

    assert (
        nico_rest.get_instance_ip(
            "instance-id", {"vpcId": "vpc-id", "ipFamilies": ["IPv4"]}
        )
        is None
    )


def test_reconciled_prefix_is_used_for_instance_network_selection():
    resources = SimpleNamespace(
        vpc_name="test-vpc",
        vpc_prefix_name="test-prefix",
    )
    reconciled = network_resources.NetworkResourceIDs(
        vpc_uuid="vpc-id",
        vpc_prefix_uuid="prefix-id",
    )

    resource_ids = lifecycle.collect_ngc_uuids(
        resources,
        "site-id",
        reconciled,
    )

    assert resource_ids.vpc_uuid == "vpc-id"
    assert resource_ids.network_interface == {"vpcPrefixId": "prefix-id"}
