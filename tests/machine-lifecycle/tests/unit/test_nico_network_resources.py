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

import requests

from lib import nico_rest


def _response(payload):
    response = requests.Response()
    response.status_code = 200
    response._content = json.dumps(payload).encode()
    response.headers["Content-Type"] = "application/json"
    return response


def test_create_vpc_uses_explicit_site_and_network_capability(monkeypatch):
    captured = {}

    def request(method, path, *, params=None, body=None):
        captured.update(method=method, path=path, params=params, body=body)
        return _response({"id": "vpc-id"})

    monkeypatch.setattr(nico_rest, "_request", request)

    result = nico_rest.create_vpc(
        "mlt-vpc",
        "site-id",
        description="Machine lifecycle test network",
    )

    assert result == "vpc-id"
    assert captured == {
        "method": "POST",
        "path": "vpc",
        "params": None,
        "body": {
            "name": "mlt-vpc",
            "siteId": "site-id",
            "networkVirtualizationType": "FNN",
            "description": "Machine lifecycle test network",
        },
    }


def test_create_vpc_prefix_uses_ip_block_and_prefix_length(monkeypatch):
    captured = {}

    def request(method, path, *, params=None, body=None):
        captured.update(method=method, path=path, params=params, body=body)
        return _response({"id": "prefix-id"})

    monkeypatch.setattr(nico_rest, "_request", request)

    result = nico_rest.create_vpc_prefix("mlt-prefix", "vpc-id", "ip-block-id", 29)

    assert result == "prefix-id"
    assert captured == {
        "method": "POST",
        "path": "vpc-prefix",
        "params": None,
        "body": {
            "name": "mlt-prefix",
            "vpcId": "vpc-id",
            "ipBlockId": "ip-block-id",
            "prefixLength": 29,
        },
    }


def test_network_resource_deletes_use_resource_specific_endpoints(monkeypatch):
    calls = []

    def request(method, path, *, params=None, body=None):
        calls.append((method, path, params, body))
        return _response({})

    monkeypatch.setattr(nico_rest, "_request", request)

    nico_rest.delete_vpc_prefix("prefix/id")
    nico_rest.delete_vpc("vpc/id")

    assert calls == [
        ("DELETE", "vpc-prefix/prefix%2Fid", None, None),
        ("DELETE", "vpc/vpc%2Fid", None, None),
    ]


def test_get_vpc_prefix_can_confirm_a_deleted_prefix_is_missing(monkeypatch):
    response = _response({"message": "not found"})
    response.status_code = 404
    monkeypatch.setattr(
        nico_rest,
        "_dispatch",
        lambda *_args: ("https://nico.test/vpc-prefix/prefix-id", response),
    )

    assert nico_rest.get_vpc_prefix("prefix-id", allow_missing=True) is None
