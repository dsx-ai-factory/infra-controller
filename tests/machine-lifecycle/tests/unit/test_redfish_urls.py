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

import pytest
import requests

from lib import admin_cli, dell_factory_reset, gb200_factory_reset, network
from lib.site_vault import BmcCredentials
from tests.lifecycle import machine_lifecycle_test as lifecycle
from tools import nico_readiness_probe


@pytest.mark.parametrize(
    ("hostname", "url_host"),
    [
        ("192.0.2.10", "192.0.2.10"),
        ("bmc.example.test", "bmc.example.test"),
        ("2001:db8::10", "[2001:db8::10]"),
        ("[2001:db8::10]", "[2001:db8::10]"),
    ],
)
def test_redfish_url_prepares_supported_bmc_addresses(hostname, url_host):
    url = network.redfish_url(hostname, "/redfish/v1/Systems/1")

    assert url == f"https://{url_host}/redfish/v1/Systems/1"
    assert requests.Request("GET", url).prepare().url == url


@pytest.mark.parametrize(
    ("operation", "command"),
    [
        (admin_cli.factory_reset_bmc, "bmc-reset-to-defaults"),
        (admin_cli.get_bmc_accounts, "get-bmc-accounts"),
    ],
)
def test_admin_cli_redfish_arguments_use_ipv6_url_host(monkeypatch, operation, command):
    recorded = []

    def run_admin_cli(args, *, no_json):
        recorded.append((args, no_json))

    monkeypatch.setattr(admin_cli, "run_admin_cli", run_admin_cli)

    operation("2001:db8::10", "operator", "secret")

    assert recorded == [
        (
            [
                "redfish",
                "--address",
                "[2001:db8::10]",
                "--username",
                "operator",
                "--password",
                "secret",
                command,
            ],
            True,
        )
    ]


@pytest.fixture
def redfish_http(monkeypatch):
    """Keep Requests URL preparation, but replace the transport to the BMC."""

    def respond_with(*responses):
        pending = iter(responses)
        recorded = []

        def send(_adapter, request, **_kwargs):
            recorded.append((request.method, request.url))
            status, payload = next(pending)
            response = requests.Response()
            response.status_code = status
            response._content = json.dumps(payload).encode()
            response.request = request
            response.url = request.url
            return response

        monkeypatch.setattr(requests.adapters.HTTPAdapter, "send", send)
        return recorded

    return respond_with


def test_redfish_recovery_wait_uses_ipv6_url(redfish_http):
    recorded = redfish_http((200, {"Vendor": "NVIDIA"}))

    network.wait_for_redfish_endpoint("2001:db8::10", consecutive_successes=1)

    assert recorded == [("GET", "https://[2001:db8::10]/redfish/v1/")]


def test_readiness_probe_uses_ipv6_url(redfish_http):
    recorded = redfish_http((200, {}))

    nico_readiness_probe._redfish("2001:db8::10", "/redfish/v1/", None)

    assert recorded == [
        ("GET", "https://[2001:db8::10]/redfish/v1/"),
    ]


def test_lenovo_credential_probe_keeps_ipv6_url_on_username_fallback(redfish_http):
    recorded = redfish_http((401, {}), (200, {}))
    machine_info = SimpleNamespace(host_bmc_ip="2001:db8::10")
    site_config = SimpleNamespace(host_bmc_credentials=BmcCredentials("USERID", "secret"))

    lifecycle._resolve_lenovo_host_bmc_username(machine_info, site_config)

    assert recorded == [("GET", "https://[2001:db8::10]/redfish/v1/Systems/1")] * 2
    assert machine_info.host_bmc_ip == "2001:db8::10"
    assert site_config.host_bmc_credentials.username == "root"


def test_gb200_bios_task_and_bmc_reset_use_ipv6_urls(redfish_http):
    recorded = redfish_http((202, {"Id": "task-1"}), (200, {"TaskState": "Completed"}), (204, {}))
    methods = gb200_factory_reset.GB200FactoryResetMethods("2001:db8::10", "admin", "secret")

    methods.reset_bios()
    methods.factory_reset_bmc()

    assert recorded == [
        ("POST", "https://[2001:db8::10]/redfish/v1/Systems/System_0/Bios/Actions/Bios.ResetBios"),
        ("GET", "https://[2001:db8::10]/redfish/v1/TaskService/Tasks/task-1"),
        (
            "POST",
            "https://[2001:db8::10]/redfish/v1/Managers/BMC_0/Actions/Manager.ResetToDefaults",
        ),
    ]


@pytest.mark.parametrize(
    ("method", "http_method", "path"),
    [
        (
            "reset_bios",
            "POST",
            "/redfish/v1/Systems/System.Embedded.1/Bios/Actions/Bios.ResetBios",
        ),
        ("unlock_idrac", "PATCH", "/redfish/v1/Managers/iDRAC.Embedded.1/Attributes"),
        (
            "disable_host_header_check",
            "PATCH",
            "/redfish/v1/Managers/iDRAC.Embedded.1/Attributes",
        ),
        (
            "factory_reset_bmc",
            "POST",
            "/redfish/v1/Managers/iDRAC.Embedded.1/Actions/Oem/DellManager.ResetToDefaults",
        ),
    ],
)
def test_dell_redfish_actions_use_ipv6_urls(monkeypatch, redfish_http, method, http_method, path):
    recorded = redfish_http((200, {}))
    monkeypatch.setattr(dell_factory_reset.time, "sleep", lambda _seconds: None)
    methods = dell_factory_reset.DellFactoryResetMethods("2001:db8::10", "root", "secret")

    getattr(methods, method)()

    assert recorded == [(http_method, f"https://[2001:db8::10]{path}")]


def test_dell_reboot_uses_ipv6_urls_for_status_and_power_changes(monkeypatch, redfish_http):
    recorded = redfish_http(
        (200, {"PowerState": "On"}), (204, {}), (200, {"PowerState": "Off"}), (204, {})
    )
    status_url = "https://[2001:db8::10]/redfish/v1/Systems/System.Embedded.1"
    reset_url = (
        "https://[2001:db8::10]/redfish/v1/Systems/System.Embedded.1/Actions/ComputerSystem.Reset"
    )
    monkeypatch.setattr(dell_factory_reset.time, "sleep", lambda _seconds: None)

    dell_factory_reset.DellFactoryResetMethods("2001:db8::10", "root", "secret").reboot_server()

    assert recorded == [
        ("GET", status_url),
        ("POST", reset_url),
        ("GET", status_url),
        ("POST", reset_url),
    ]
