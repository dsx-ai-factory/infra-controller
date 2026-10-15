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

from lib import network_resources, nico_rest
from lib.config import LifecycleMode
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

    def list_operating_systems(*, query=None, operating_system_type=None):
        calls.append(("list", query, operating_system_type))
        if query is not None:
            return [{"id": "partial-id", "name": "mlt-os-test-partial"}]
        return [{"id": "recovered-os-id", "name": "mlt-os-test"}]

    monkeypatch.setattr(
        nico_rest,
        "list_operating_systems",
        list_operating_systems,
    )
    monkeypatch.setattr(
        nico_rest,
        "delete_operating_system",
        lambda uuid, strict: calls.append(("delete", uuid, strict)) or True,
    )

    lifecycle._cleanup_temporary_operating_system("mlt-os-test", None)

    assert calls == [
        ("list", "mlt-os-test", None),
        ("list", None, None),
        ("delete", "recovered-os-id", False),
    ]


def test_os_preparation_fails_before_machine_discovery(monkeypatch):
    test_config = SimpleNamespace(
        lifecycle=SimpleNamespace(mode=LifecycleMode.FULL),
        operating_system=SimpleNamespace(
            ipxe_script_path="missing.ipxe",
            user_data_template_path="missing.yaml",
        ),
        debug=SimpleNamespace(ssh_public_key=None, enable_console_password=False),
    )
    monkeypatch.setattr(
        lifecycle,
        "build_ephemeral_operating_system",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(ValueError("invalid OS input")),
    )
    monkeypatch.setattr(
        lifecycle,
        "collect_machine_info",
        lambda _config: pytest.fail("machine discovery must follow OS preparation"),
    )

    with pytest.raises(ValueError, match="invalid OS input"):
        lifecycle._run_machine_lifecycle(test_config, 0.0)


def test_prepared_os_is_passed_into_provisioning(monkeypatch):
    prepared = object()
    captured = []
    test_config = SimpleNamespace(
        lifecycle=SimpleNamespace(mode=LifecycleMode.PROVISION_ONLY),
        operating_system=SimpleNamespace(
            ipxe_script_path="boot.ipxe",
            user_data_template_path="user-data.yaml",
        ),
        debug=SimpleNamespace(ssh_public_key="operator-key", enable_console_password=False),
        os_janitor=SimpleNamespace(enabled=False),
        skip_factory_reset=False,
        test_sitewide_bmc_fallback=False,
    )
    machine_info = SimpleNamespace(vendor="dell")
    site_config = object()
    monkeypatch.setattr(
        lifecycle,
        "build_ephemeral_operating_system",
        lambda *args, **kwargs: captured.append((args, kwargs)) or prepared,
    )
    monkeypatch.setattr(lifecycle, "collect_machine_info", lambda _config: machine_info)
    monkeypatch.setattr(
        lifecycle, "setup_site_config", lambda _config, _machine_info: site_config
    )
    monkeypatch.setattr(lifecycle, "_mask_site_config_creds", lambda _config: {})
    monkeypatch.setattr(lifecycle, "verify_initial_machine_state", lambda *_args: None)
    monkeypatch.setattr(
        lifecycle, "verify_machine_has_no_instance_type", lambda _config: None
    )
    monkeypatch.setattr(
        lifecycle,
        "_run_provisioning_cycles",
        lambda *args: captured.append(args),
    )

    lifecycle._run_machine_lifecycle(test_config, 7.0)

    assert captured[0] == (
        ("boot.ipxe", "user-data.yaml"),
        {"debug_public_key": "operator-key", "enable_console_password": False},
    )
    assert captured[1] == (test_config, site_config, machine_info, prepared, 7.0)


def test_instance_ssh_uses_the_inferred_username(monkeypatch):
    connections = []

    class Stream:
        channel = SimpleNamespace(recv_exit_status=lambda: 0)

        @staticmethod
        def readlines():
            return []

    class SSHClient:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return None

        @staticmethod
        def set_missing_host_key_policy(_policy):
            return None

        @staticmethod
        def connect(host, **kwargs):
            connections.append((host, kwargs))

        @staticmethod
        def exec_command(_command):
            return Stream(), Stream(), Stream()

    monkeypatch.setattr(nico_rest, "create_instance", lambda **_kwargs: {"id": "instance-id"})
    monkeypatch.setattr(
        lifecycle.admin_cli, "wait_for_machine_assigned_ready", lambda *_args, **_kwargs: None
    )
    monkeypatch.setattr(nico_rest, "wait_for_instance_ready", lambda *_args, **_kwargs: None)
    monkeypatch.setattr(
        nico_rest, "wait_for_instance_ip", lambda *_args, **_kwargs: "192.0.2.10"
    )
    monkeypatch.setattr(lifecycle.network, "wait_for_host_port", lambda *_args, **_kwargs: None)
    monkeypatch.setattr(lifecycle.paramiko, "SSHClient", SSHClient)

    instance_id = lifecycle.create_instance_and_verify(
        SimpleNamespace(
            expected_dpu_count=1,
            machine_under_test="host-id",
            diagnostics=SimpleNamespace(),
        ),
        SimpleNamespace(site=nico_rest.Site("site")),
        SimpleNamespace(dpu_ids=[]),
        lifecycle.NGCUUIDs(
            site_uuid="site-id",
            vpc_uuid="vpc-id",
            network_interface={"vpcPrefixId": "prefix-id"},
            os_uuid="os-id",
        ),
        object(),
        "external-template-user",
    )

    assert instance_id == "instance-id"
    assert connections[0][1]["username"] == "external-template-user"
