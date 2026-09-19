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

from copy import deepcopy
from dataclasses import replace
from types import SimpleNamespace

import pytest

from lib import network_resources, nico_rest
from lib.config import LifecycleMode, NetworkResourcesConfig
from tests.lifecycle import machine_lifecycle_test as lifecycle


@pytest.fixture
def resource_config():
    return NetworkResourcesConfig(
        vpc_name="mlt-vpc",
        vpc_prefix_name="mlt-prefix",
        ip_block_name="tenant-ip-block",
        vpc_prefix_length=29,
        cleanup=True,
    )


@pytest.fixture
def compatible_resources():
    return {
        "vpc": {
            "id": "vpc-id",
            "name": "mlt-vpc",
            "siteId": "site-id",
            "networkVirtualizationType": "FNN",
            "status": "Ready",
        },
        "ip_block": {
            "id": "ip-block-id",
            "name": "tenant-ip-block",
            "siteId": "site-id",
            "tenantId": "tenant-id",
            "protocolVersion": "IPv4",
            "prefixLength": 24,
            "status": "Ready",
        },
        "prefix": {
            "id": "prefix-id",
            "name": "mlt-prefix",
            "siteId": "site-id",
            "vpcId": "vpc-id",
            "ipBlockId": "ip-block-id",
            "prefixLength": 29,
            "status": "Ready",
        },
    }


def _mock_lists(monkeypatch, resources):
    monkeypatch.setattr(
        nico_rest,
        "list_vpcs",
        lambda _site: [{"id": resources["vpc"]["id"], "name": resources["vpc"]["name"]}],
    )
    monkeypatch.setattr(nico_rest, "get_vpc", lambda _uuid: resources["vpc"])
    monkeypatch.setattr(nico_rest, "list_ip_blocks", lambda _site: [resources["ip_block"]])
    monkeypatch.setattr(
        nico_rest,
        "list_vpc_prefixes",
        lambda _site: [
            {"id": resources["prefix"]["id"], "name": resources["prefix"]["name"]}
        ],
    )
    monkeypatch.setattr(nico_rest, "get_vpc_prefix", lambda _uuid: resources["prefix"])


def test_reuses_compatible_resources_without_taking_ownership(
    monkeypatch, resource_config, compatible_resources
):
    _mock_lists(monkeypatch, compatible_resources)
    monkeypatch.setattr(
        nico_rest,
        "create_vpc",
        lambda *_args, **_kwargs: pytest.fail("must not create an existing VPC"),
    )
    monkeypatch.setattr(
        nico_rest,
        "create_vpc_prefix",
        lambda *_args, **_kwargs: pytest.fail("must not create an existing prefix"),
    )
    ownership = network_resources.NetworkResourceOwnership()

    result = network_resources.reconcile_network_resources(resource_config, "site-id", ownership)

    assert result == network_resources.NetworkResourceIDs(
        vpc_uuid="vpc-id", vpc_prefix_uuid="prefix-id"
    )
    assert ownership.vpc_uuid is None
    assert ownership.vpc_prefix_uuid is None
    assert ownership.reused_vpc_uuid == "vpc-id"
    assert ownership.reused_vpc_name == "mlt-vpc"
    assert ownership.reused_vpc_prefix_uuid == "prefix-id"
    assert ownership.reused_vpc_prefix_name == "mlt-prefix"


def test_creates_missing_resources_and_tracks_exact_ownership(
    monkeypatch, resource_config, compatible_resources
):
    calls = []
    monkeypatch.setattr(nico_rest, "list_vpcs", lambda _site: [])
    monkeypatch.setattr(
        nico_rest,
        "create_vpc",
        lambda *args, **kwargs: calls.append(("create-vpc", args, kwargs)) or "vpc-id",
    )
    monkeypatch.setattr(
        nico_rest,
        "get_vpc",
        lambda uuid: calls.append(("get-vpc", uuid)) or compatible_resources["vpc"],
    )
    monkeypatch.setattr(
        nico_rest, "list_ip_blocks", lambda _site: [compatible_resources["ip_block"]]
    )
    monkeypatch.setattr(nico_rest, "list_vpc_prefixes", lambda _site: [])
    monkeypatch.setattr(
        nico_rest,
        "create_vpc_prefix",
        lambda *args: calls.append(("create-prefix", args)) or "prefix-id",
    )
    monkeypatch.setattr(
        nico_rest,
        "get_vpc_prefix",
        lambda uuid: calls.append(("get-prefix", uuid)) or compatible_resources["prefix"],
    )
    ownership = network_resources.NetworkResourceOwnership()

    result = network_resources.reconcile_network_resources(resource_config, "site-id", ownership)

    assert result.vpc_uuid == "vpc-id"
    assert result.vpc_prefix_uuid == "prefix-id"
    assert calls == [
        (
            "create-vpc",
            ("mlt-vpc", "site-id"),
            {"description": "Machine lifecycle test network"},
        ),
        ("get-vpc", "vpc-id"),
        (
            "create-prefix",
            ("mlt-prefix", "vpc-id", "ip-block-id", 29),
        ),
        ("get-prefix", "prefix-id"),
    ]
    assert ownership.vpc_uuid == "vpc-id"
    assert ownership.parent_vpc_uuid == "vpc-id"
    assert ownership.vpc_name == "mlt-vpc"
    assert ownership.vpc_prefix_uuid == "prefix-id"
    assert ownership.vpc_prefix_name == "mlt-prefix"


def test_refuses_to_create_an_absent_vpc_when_creation_is_disabled(
    monkeypatch, resource_config, compatible_resources
):
    config = replace(resource_config, create_missing=False)
    _mock_lists(monkeypatch, compatible_resources)
    monkeypatch.setattr(nico_rest, "list_vpcs", lambda _site: [])
    monkeypatch.setattr(
        nico_rest,
        "create_vpc",
        lambda *_args, **_kwargs: pytest.fail("must not create a VPC"),
    )
    ownership = network_resources.NetworkResourceOwnership()

    with pytest.raises(network_resources.NetworkResourceError, match="will not create it"):
        network_resources.reconcile_network_resources(config, "site-id", ownership)

    assert ownership.vpc_uuid is None


def test_refuses_to_create_an_absent_prefix_when_creation_is_disabled(
    monkeypatch, resource_config, compatible_resources
):
    config = replace(resource_config, create_missing=False)
    _mock_lists(monkeypatch, compatible_resources)
    monkeypatch.setattr(nico_rest, "list_vpc_prefixes", lambda _site: [])
    monkeypatch.setattr(
        nico_rest,
        "create_vpc_prefix",
        lambda *_args, **_kwargs: pytest.fail("must not create a prefix"),
    )
    ownership = network_resources.NetworkResourceOwnership()

    with pytest.raises(network_resources.NetworkResourceError, match="will not create it"):
        network_resources.reconcile_network_resources(config, "site-id", ownership)

    # The VPC was found, so it is recorded as reused and stays protected.
    assert ownership.reused_vpc_uuid == "vpc-id"
    assert ownership.vpc_prefix_uuid is None


def test_disabling_creation_still_reuses_existing_resources(
    monkeypatch, resource_config, compatible_resources
):
    config = replace(resource_config, create_missing=False)
    _mock_lists(monkeypatch, compatible_resources)
    ownership = network_resources.NetworkResourceOwnership()

    result = network_resources.reconcile_network_resources(config, "site-id", ownership)

    assert result == network_resources.NetworkResourceIDs(
        vpc_uuid="vpc-id", vpc_prefix_uuid="prefix-id"
    )
    assert ownership.reused_vpc_uuid == "vpc-id"
    assert ownership.reused_vpc_prefix_uuid == "prefix-id"


def test_waits_for_a_transitional_resource(monkeypatch, resource_config, compatible_resources):
    provisioning = {**compatible_resources["vpc"], "status": "Provisioning"}
    vpc_responses = iter([provisioning, compatible_resources["vpc"]])
    monkeypatch.setattr(
        nico_rest,
        "list_vpcs",
        lambda _site: [{"id": "vpc-id", "name": "mlt-vpc"}],
    )
    monkeypatch.setattr(nico_rest, "get_vpc", lambda _uuid: next(vpc_responses))
    monkeypatch.setattr(
        nico_rest, "list_ip_blocks", lambda _site: [compatible_resources["ip_block"]]
    )
    monkeypatch.setattr(
        nico_rest,
        "list_vpc_prefixes",
        lambda _site: [{"id": "prefix-id", "name": "mlt-prefix"}],
    )
    monkeypatch.setattr(
        nico_rest,
        "get_vpc_prefix",
        lambda _uuid: compatible_resources["prefix"],
    )
    monkeypatch.setattr(network_resources.time, "sleep", lambda _seconds: None)

    result = network_resources.reconcile_network_resources(
        resource_config,
        "site-id",
        network_resources.NetworkResourceOwnership(),
        poll_interval=0,
    )

    assert result.vpc_uuid == "vpc-id"


def test_wait_uses_remaining_timeout_for_final_poll(monkeypatch, compatible_resources):
    now = [0.0]
    sleeps = []
    provisioning = {**compatible_resources["vpc"], "status": "Provisioning"}

    monkeypatch.setattr(network_resources.time, "monotonic", lambda: now[0])

    def sleep(seconds):
        sleeps.append(seconds)
        now[0] += seconds

    monkeypatch.setattr(network_resources.time, "sleep", sleep)

    result = network_resources._wait_until_ready(
        "VPC",
        provisioning,
        lambda _uuid: compatible_resources["vpc"],
        timeout=3,
        poll_interval=5,
    )

    assert result["status"] == "Ready"
    assert sleeps == [3]


def test_rejects_an_incompatible_existing_vpc(monkeypatch, resource_config, compatible_resources):
    incompatible = deepcopy(compatible_resources)
    incompatible["vpc"]["networkVirtualizationType"] = "ETHERNET_VIRTUALIZER"
    _mock_lists(monkeypatch, incompatible)

    with pytest.raises(
        network_resources.NetworkResourceError,
        match="Existing VPC.*networkVirtualizationType",
    ):
        network_resources.reconcile_network_resources(
            resource_config,
            "site-id",
            network_resources.NetworkResourceOwnership(),
        )


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("vpcId", "different-vpc"),
        ("ipBlockId", "different-ip-block"),
        ("prefixLength", 28),
    ],
)
def test_rejects_an_incompatible_existing_prefix(
    monkeypatch, resource_config, compatible_resources, field, value
):
    incompatible = deepcopy(compatible_resources)
    incompatible["prefix"][field] = value
    _mock_lists(monkeypatch, incompatible)

    with pytest.raises(
        network_resources.NetworkResourceError,
        match=f"Existing VPC prefix.*{field}",
    ):
        network_resources.reconcile_network_resources(
            resource_config,
            "site-id",
            network_resources.NetworkResourceOwnership(),
        )


def test_requires_a_compatible_tenant_ipv4_ip_block(
    monkeypatch, resource_config, compatible_resources
):
    incompatible = deepcopy(compatible_resources)
    incompatible["ip_block"]["tenantId"] = None
    _mock_lists(monkeypatch, incompatible)

    with pytest.raises(network_resources.NetworkResourceError, match="not tenant-derived"):
        network_resources.reconcile_network_resources(
            resource_config,
            "site-id",
            network_resources.NetworkResourceOwnership(),
        )


def test_ip_block_must_be_larger_than_requested_prefix(
    monkeypatch, resource_config, compatible_resources
):
    incompatible = deepcopy(compatible_resources)
    incompatible["ip_block"]["prefixLength"] = resource_config.vpc_prefix_length
    _mock_lists(monkeypatch, incompatible)

    with pytest.raises(
        network_resources.NetworkResourceError,
        match=r"prefixLength=29 cannot supply a /29 child prefix",
    ):
        network_resources.reconcile_network_resources(
            resource_config,
            "site-id",
            network_resources.NetworkResourceOwnership(),
        )


def test_cleanup_deletes_only_owned_resources_in_dependency_order(monkeypatch):
    deleted = []
    monkeypatch.setattr(nico_rest, "get_instances", lambda _site, _vpc: [])
    monkeypatch.setattr(
        nico_rest,
        "delete_vpc_prefix",
        lambda uuid: deleted.append(("prefix", uuid)),
    )
    monkeypatch.setattr(
        nico_rest,
        "get_vpc_prefix",
        lambda uuid, *, allow_missing: deleted.append(("get-prefix", uuid)) or None,
    )
    monkeypatch.setattr(nico_rest, "delete_vpc", lambda uuid: deleted.append(("vpc", uuid)))
    ownership = network_resources.NetworkResourceOwnership(
        site_uuid="site-id",
        parent_vpc_uuid="owned-vpc",
        vpc_uuid="owned-vpc",
        vpc_name="mlt-vpc",
        vpc_prefix_uuid="owned-prefix",
        vpc_prefix_name="mlt-prefix",
    )

    network_resources.cleanup_network_resources(ownership)

    assert deleted == [
        ("prefix", "owned-prefix"),
        ("get-prefix", "owned-prefix"),
        ("vpc", "owned-vpc"),
    ]


def test_cleanup_waits_for_deleting_prefix_before_deleting_vpc(monkeypatch):
    calls = []
    prefix_states = iter([{"id": "owned-prefix", "status": "Deleting"}, None])
    monkeypatch.setattr(nico_rest, "get_instances", lambda _site, _vpc: [])
    monkeypatch.setattr(
        nico_rest,
        "delete_vpc_prefix",
        lambda uuid: calls.append(("delete-prefix", uuid)),
    )
    monkeypatch.setattr(
        nico_rest,
        "get_vpc_prefix",
        lambda uuid, *, allow_missing: calls.append(("get-prefix", uuid))
        or next(prefix_states),
    )
    monkeypatch.setattr(
        network_resources.time,
        "sleep",
        lambda seconds: calls.append(("sleep", seconds)),
    )
    monkeypatch.setattr(
        nico_rest,
        "delete_vpc",
        lambda uuid: calls.append(("delete-vpc", uuid)),
    )
    ownership = network_resources.NetworkResourceOwnership(
        site_uuid="site-id",
        parent_vpc_uuid="owned-vpc",
        vpc_uuid="owned-vpc",
        vpc_prefix_uuid="owned-prefix",
    )

    network_resources.cleanup_network_resources(ownership)

    assert calls == [
        ("delete-prefix", "owned-prefix"),
        ("get-prefix", "owned-prefix"),
        ("sleep", network_resources.NETWORK_RESOURCE_POLL_INTERVAL),
        ("get-prefix", "owned-prefix"),
        ("delete-vpc", "owned-vpc"),
    ]


def test_wait_for_prefix_deletion_fails_fast_on_error(monkeypatch):
    calls = []
    monkeypatch.setattr(
        nico_rest,
        "get_vpc_prefix",
        lambda uuid, *, allow_missing: calls.append((uuid, allow_missing))
        or {"id": uuid, "status": "Error"},
    )
    monkeypatch.setattr(
        network_resources.time,
        "sleep",
        lambda _seconds: pytest.fail("terminal status must not be polled"),
    )

    with pytest.raises(
        network_resources.NetworkResourceError,
        match="entered terminal status 'Error'",
    ):
        network_resources._wait_until_vpc_prefix_deleted("owned-prefix")

    assert calls == [("owned-prefix", True)]


def test_cleanup_reports_reused_resources(capsys):
    ownership = network_resources.NetworkResourceOwnership(
        reused_vpc_uuid="reused-vpc",
        reused_vpc_name="existing-vpc",
        reused_vpc_prefix_uuid="reused-prefix",
        reused_vpc_prefix_name="existing-prefix",
    )

    network_resources.cleanup_network_resources(ownership)

    output = capsys.readouterr().out
    assert "Not deleting pre-existing VPC 'existing-vpc' (reused-vpc)" in output
    assert "Not deleting pre-existing VPC prefix 'existing-prefix' (reused-prefix)" in output


def test_cleanup_preserves_vpc_when_prefix_deletion_fails(monkeypatch):
    monkeypatch.setattr(nico_rest, "get_instances", lambda _site, _vpc: [])
    monkeypatch.setattr(
        nico_rest,
        "delete_vpc_prefix",
        lambda _uuid: (_ for _ in ()).throw(nico_rest.NicoError("in use")),
    )
    monkeypatch.setattr(
        nico_rest,
        "delete_vpc",
        lambda _uuid: pytest.fail("VPC must remain while its prefix exists"),
    )
    ownership = network_resources.NetworkResourceOwnership(
        site_uuid="site-id",
        parent_vpc_uuid="owned-vpc",
        vpc_uuid="owned-vpc",
        vpc_prefix_uuid="owned-prefix",
    )

    network_resources.cleanup_network_resources(ownership)


def test_cleanup_preserves_resources_while_an_instance_uses_the_vpc(monkeypatch):
    monkeypatch.setattr(
        nico_rest,
        "get_instances",
        lambda _site, _vpc: [{"id": "active-instance"}],
    )
    monkeypatch.setattr(
        nico_rest,
        "delete_vpc_prefix",
        lambda _uuid: pytest.fail("an in-use prefix must not be deleted"),
    )
    monkeypatch.setattr(
        nico_rest,
        "delete_vpc",
        lambda _uuid: pytest.fail("an in-use VPC must not be deleted"),
    )
    ownership = network_resources.NetworkResourceOwnership(
        site_uuid="site-id",
        parent_vpc_uuid="owned-vpc",
        vpc_uuid="owned-vpc",
        vpc_prefix_uuid="owned-prefix",
    )

    network_resources.cleanup_network_resources(ownership)


def test_prefix_only_cleanup_checks_its_reused_parent_vpc(monkeypatch):
    calls = []
    monkeypatch.setattr(
        nico_rest,
        "get_instances",
        lambda site, vpc: calls.append(("instances", site, vpc)) or [],
    )
    monkeypatch.setattr(
        nico_rest,
        "delete_vpc_prefix",
        lambda uuid: calls.append(("delete-prefix", uuid)),
    )
    monkeypatch.setattr(
        nico_rest,
        "get_vpc_prefix",
        lambda uuid, *, allow_missing: calls.append(("get-prefix", uuid)) or None,
    )
    monkeypatch.setattr(
        nico_rest,
        "delete_vpc",
        lambda _uuid: pytest.fail("the reused parent VPC must not be deleted"),
    )
    ownership = network_resources.NetworkResourceOwnership(
        site_uuid="site-id",
        parent_vpc_uuid="reused-vpc",
        vpc_prefix_uuid="owned-prefix",
    )

    network_resources.cleanup_network_resources(ownership)

    assert calls == [
        ("instances", "site-id", "reused-vpc"),
        ("delete-prefix", "owned-prefix"),
        ("get-prefix", "owned-prefix"),
    ]


def test_provisioning_cleanup_runs_when_reconciliation_fails(monkeypatch):
    ownership_seen = []
    test_config = SimpleNamespace(resources=SimpleNamespace(cleanup=True))
    site_config = SimpleNamespace(site=SimpleNamespace(name="test-site"))

    def fail(_resources, site_uuid, ownership):
        assert site_uuid == "site-id"
        ownership.vpc_uuid = "owned-vpc"
        raise RuntimeError("lifecycle failed")

    monkeypatch.setattr(nico_rest, "get_site_uuid", lambda _name: "site-id")
    monkeypatch.setattr(network_resources, "reconcile_network_resources", fail)
    monkeypatch.setattr(
        network_resources,
        "cleanup_network_resources",
        lambda ownership: ownership_seen.append(ownership.vpc_uuid),
    )

    with pytest.raises(RuntimeError, match="lifecycle failed"):
        lifecycle._run_provisioning_cycles(
            test_config, site_config, SimpleNamespace(), 0.0
        )

    assert ownership_seen == ["owned-vpc"]


def test_provisioning_warns_and_skips_cleanup_when_disabled(monkeypatch, capsys):
    test_config = SimpleNamespace(resources=SimpleNamespace(cleanup=False))
    site_config = SimpleNamespace(site=SimpleNamespace(name="test-site"))

    def fail(_resources, _site_uuid, ownership):
        ownership.vpc_uuid = "owned-vpc"
        raise RuntimeError("lifecycle failed")

    monkeypatch.setattr(nico_rest, "get_site_uuid", lambda _name: "site-id")
    monkeypatch.setattr(network_resources, "reconcile_network_resources", fail)
    monkeypatch.setattr(
        network_resources,
        "cleanup_network_resources",
        lambda _ownership: pytest.fail("cleanup is disabled"),
    )

    with pytest.raises(RuntimeError, match="lifecycle failed"):
        lifecycle._run_provisioning_cycles(
            test_config, site_config, SimpleNamespace(), 0.0
        )

    assert "Resources created by this run were not deleted" in capsys.readouterr().out


def test_provisioning_does_not_warn_when_disabled_cleanup_has_no_owned_resources(
    monkeypatch, capsys
):
    test_config = SimpleNamespace(
        resources=SimpleNamespace(cleanup=False),
        provision_cycles=0,
        debug=SimpleNamespace(
            ssh_public_key=None,
            enable_console_password=False,
        ),
    )
    site_config = SimpleNamespace(site=SimpleNamespace(name="test-site"))

    monkeypatch.setattr(nico_rest, "get_site_uuid", lambda _name: "site-id")
    monkeypatch.setattr(
        network_resources,
        "reconcile_network_resources",
        lambda *_args: network_resources.NetworkResourceIDs("vpc-id", "prefix-id"),
    )
    monkeypatch.setattr(lifecycle, "collect_ngc_uuids", lambda *_args: object())
    monkeypatch.setattr(
        lifecycle,
        "build_ephemeral_operating_system",
        lambda **_kwargs: (_ for _ in ()).throw(RuntimeError("OS creation failed")),
    )
    monkeypatch.setattr(
        network_resources,
        "cleanup_network_resources",
        lambda _ownership: pytest.fail("cleanup is disabled"),
    )

    with pytest.raises(RuntimeError, match="OS creation failed"):
        lifecycle._run_provisioning_cycles(
            test_config, site_config, SimpleNamespace(), 0.0
        )

    assert "WARNING" not in capsys.readouterr().out


def test_ingestion_only_does_not_reconcile_network_resources(monkeypatch):
    test_config = SimpleNamespace(
        lifecycle=SimpleNamespace(mode=LifecycleMode.INGESTION_ONLY),
        os_janitor=SimpleNamespace(enabled=False),
        skip_factory_reset=True,
        test_sitewide_bmc_fallback=False,
    )
    machine_info = SimpleNamespace(vendor="dell")
    site_config = object()

    monkeypatch.setattr(lifecycle, "collect_machine_info", lambda _config: machine_info)
    monkeypatch.setattr(lifecycle, "setup_site_config", lambda _config, _machine_info: site_config)
    monkeypatch.setattr(lifecycle, "_mask_site_config_creds", lambda _site_config: {})
    monkeypatch.setattr(lifecycle, "verify_initial_machine_state", lambda *_args: None)
    monkeypatch.setattr(lifecycle, "verify_machine_has_no_instance_type", lambda _config: None)
    monkeypatch.setattr(
        lifecycle,
        "force_delete_and_await_reingestion",
        lambda *_args, **_kwargs: None,
    )
    monkeypatch.setattr(lifecycle, "_refresh_site_vault_bmc_credentials", lambda *_args: None)
    monkeypatch.setattr(
        lifecycle,
        "_run_provisioning_cycles",
        lambda *_args: pytest.fail("ingestion-only must not reconcile networking"),
    )

    lifecycle._run_machine_lifecycle(test_config, 0.0)
