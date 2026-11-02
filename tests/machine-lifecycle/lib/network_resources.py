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

"""Idempotent reconciliation and ownership-safe cleanup of NICo networking."""

from __future__ import annotations

import sys
import time
from dataclasses import dataclass
from typing import Callable

from lib import nico_rest
from lib.config import NetworkResourcesConfig

NETWORK_RESOURCE_READY_TIMEOUT = 10 * 60
NETWORK_RESOURCE_POLL_INTERVAL = 5
_TRANSITIONAL_STATUSES = {"Pending", "Provisioning"}
_TERMINAL_STATUSES = {"Deleting", "Deleted", "Error", "Failed"}


class NetworkResourceError(RuntimeError):
    """The configured network resources cannot safely be used by MLT."""


@dataclass
class NetworkResourceOwnership:
    """Reconciled resources and those created by this run for cleanup."""

    site_uuid: str | None = None
    parent_vpc_uuid: str | None = None
    vpc_uuid: str | None = None
    vpc_name: str | None = None
    vpc_prefix_uuid: str | None = None
    vpc_prefix_name: str | None = None
    reused_vpc_uuid: str | None = None
    reused_vpc_name: str | None = None
    reused_vpc_prefix_uuid: str | None = None
    reused_vpc_prefix_name: str | None = None


@dataclass(frozen=True)
class NetworkResourceIDs:
    """Reconciled resource IDs used for instance provisioning."""

    vpc_uuid: str
    vpc_prefix_uuid: str


def _named_resource(resources: list[dict], name: str, kind: str) -> dict | None:
    matches = [resource for resource in resources if resource.get("name") == name]
    if len(matches) > 1:
        ids = [resource.get("id") for resource in matches]
        raise NetworkResourceError(f"Multiple {kind} resources named {name!r} were returned: {ids}")
    return matches[0] if matches else None


def _required_string(resource: dict, field: str, kind: str) -> str:
    value = resource.get(field)
    if not isinstance(value, str) or not value.strip():
        raise NetworkResourceError(
            f"{kind} {resource.get('name')!r} has no valid {field!r}: {resource!r}"
        )
    return value


def _wait_until_ready(
    kind: str,
    resource: dict,
    getter: Callable[[str], dict],
    *,
    timeout: int,
    poll_interval: int,
) -> dict:
    resource_uuid = _required_string(resource, "id", kind)
    deadline = time.monotonic() + timeout
    while True:
        status = resource.get("status")
        if status == "Ready":
            return resource
        if status in _TERMINAL_STATUSES:
            raise NetworkResourceError(
                f"{kind} {resource.get('name')!r} ({resource_uuid}) is in "
                f"incompatible status {status!r}"
            )
        if status not in _TRANSITIONAL_STATUSES:
            raise NetworkResourceError(
                f"{kind} {resource.get('name')!r} ({resource_uuid}) returned "
                f"unknown status {status!r}"
            )
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise NetworkResourceError(
                f"{kind} {resource.get('name')!r} ({resource_uuid}) did not "
                f"reach Ready within {timeout} seconds; last status was {status!r}"
            )
        time.sleep(min(poll_interval, remaining))
        resource = getter(resource_uuid)


def _wait_until_vpc_prefix_deleted(
    vpc_prefix_uuid: str,
    *,
    timeout: int = NETWORK_RESOURCE_READY_TIMEOUT,
    poll_interval: int = NETWORK_RESOURCE_POLL_INTERVAL,
) -> None:
    """Wait until a deleted VPC prefix is no longer returned by NICo."""
    deadline = time.monotonic() + timeout
    while True:
        vpc_prefix = nico_rest.get_vpc_prefix(vpc_prefix_uuid, allow_missing=True)
        if vpc_prefix is None:
            return
        status = vpc_prefix.get("status")
        if status == "Error":
            raise NetworkResourceError(
                f"VPC prefix {vpc_prefix_uuid} entered terminal status {status!r} "
                "while waiting for deletion"
            )
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise NetworkResourceError(
                f"VPC prefix {vpc_prefix_uuid} still exists after {timeout} seconds"
            )
        time.sleep(min(poll_interval, remaining))


def _validate_vpc(vpc: dict, config: NetworkResourcesConfig, site_uuid: str) -> None:
    mismatches = []
    if vpc.get("siteId") != site_uuid:
        mismatches.append(f"siteId={vpc.get('siteId')!r}, expected {site_uuid!r}")
    if vpc.get("networkVirtualizationType") != "FNN":
        mismatches.append(
            f"networkVirtualizationType={vpc.get('networkVirtualizationType')!r}, expected 'FNN'"
        )
    if mismatches:
        raise NetworkResourceError(
            f"Existing VPC {config.vpc_name!r} is incompatible: " + "; ".join(mismatches)
        )


def _validate_ip_block(ip_block: dict, config: NetworkResourcesConfig, site_uuid: str) -> str:
    mismatches = []
    if ip_block.get("siteId") != site_uuid:
        mismatches.append(f"siteId={ip_block.get('siteId')!r}, expected {site_uuid!r}")
    if ip_block.get("protocolVersion") != "IPv4":
        mismatches.append(f"protocolVersion={ip_block.get('protocolVersion')!r}, expected 'IPv4'")
    if ip_block.get("status") != "Ready":
        mismatches.append(f"status={ip_block.get('status')!r}, expected 'Ready'")
    if not ip_block.get("tenantId"):
        mismatches.append("tenantId is absent; the IP block is not tenant-derived")
    parent_length = ip_block.get("prefixLength")
    if not isinstance(parent_length, int) or parent_length >= config.vpc_prefix_length:
        mismatches.append(
            f"prefixLength={parent_length!r} cannot supply a "
            f"/{config.vpc_prefix_length} child prefix"
        )
    if mismatches:
        raise NetworkResourceError(
            f"IP block {config.ip_block_name!r} is incompatible: " + "; ".join(mismatches)
        )
    return _required_string(ip_block, "id", "IP block")


def _validate_vpc_prefix(
    vpc_prefix: dict,
    config: NetworkResourcesConfig,
    site_uuid: str,
    vpc_uuid: str,
    ip_block_uuid: str,
) -> None:
    mismatches = []
    expected = {
        "siteId": site_uuid,
        "vpcId": vpc_uuid,
        "ipBlockId": ip_block_uuid,
        "prefixLength": config.vpc_prefix_length,
    }
    for field, expected_value in expected.items():
        if vpc_prefix.get(field) != expected_value:
            mismatches.append(f"{field}={vpc_prefix.get(field)!r}, expected {expected_value!r}")
    if mismatches:
        raise NetworkResourceError(
            f"Existing VPC prefix {config.vpc_prefix_name!r} is incompatible: "
            + "; ".join(mismatches)
        )


def reconcile_network_resources(
    config: NetworkResourcesConfig,
    site_uuid: str,
    ownership: NetworkResourceOwnership,
    *,
    timeout: int = NETWORK_RESOURCE_READY_TIMEOUT,
    poll_interval: int = NETWORK_RESOURCE_POLL_INTERVAL,
) -> NetworkResourceIDs:
    """Create or validate the configured VPC and VPC prefix."""
    ownership.site_uuid = site_uuid

    listed_vpc = _named_resource(nico_rest.list_vpcs(site_uuid), config.vpc_name, "VPC")
    if listed_vpc is None:
        # Provisioned externally where create_missing is false, so absence is
        # an error rather than something to resolve by building our own.
        if not config.create_missing:
            raise NetworkResourceError(
                f"VPC {config.vpc_name!r} does not exist at site {site_uuid} and "
                "resources.create_missing is false; MLT will not create it"
            )
        print(f"Creating VPC {config.vpc_name!r}")
        vpc_uuid = nico_rest.create_vpc(
            config.vpc_name,
            site_uuid,
            description="Machine lifecycle test network",
        )
        ownership.vpc_uuid = vpc_uuid
        ownership.vpc_name = config.vpc_name
    else:
        print(f"Reusing existing VPC {config.vpc_name!r}")
        vpc_uuid = _required_string(listed_vpc, "id", "VPC")
        ownership.reused_vpc_uuid = vpc_uuid
        ownership.reused_vpc_name = config.vpc_name

    vpc = nico_rest.get_vpc(vpc_uuid)
    _validate_vpc(vpc, config, site_uuid)
    vpc = _wait_until_ready(
        "VPC",
        vpc,
        nico_rest.get_vpc,
        timeout=timeout,
        poll_interval=poll_interval,
    )
    _validate_vpc(vpc, config, site_uuid)
    vpc_uuid = _required_string(vpc, "id", "VPC")
    ownership.parent_vpc_uuid = vpc_uuid

    ip_block = _named_resource(
        nico_rest.list_ip_blocks(site_uuid), config.ip_block_name, "IP block"
    )
    if ip_block is None:
        raise NetworkResourceError(
            f"Required tenant IPv4 IP block {config.ip_block_name!r} does not exist "
            f"at site {site_uuid}; MLT does not create allocations or IP blocks"
        )
    ip_block_uuid = _validate_ip_block(ip_block, config, site_uuid)

    listed_vpc_prefix = _named_resource(
        nico_rest.list_vpc_prefixes(site_uuid),
        config.vpc_prefix_name,
        "VPC prefix",
    )
    if listed_vpc_prefix is None:
        # As above: absence is an error where these are provisioned for us.
        if not config.create_missing:
            raise NetworkResourceError(
                f"VPC prefix {config.vpc_prefix_name!r} does not exist at site "
                f"{site_uuid} and resources.create_missing is false; MLT will not "
                "create it"
            )
        print(f"Creating VPC prefix {config.vpc_prefix_name!r}")
        vpc_prefix_uuid = nico_rest.create_vpc_prefix(
            config.vpc_prefix_name,
            vpc_uuid,
            ip_block_uuid,
            config.vpc_prefix_length,
        )
        ownership.vpc_prefix_uuid = vpc_prefix_uuid
        ownership.vpc_prefix_name = config.vpc_prefix_name
    else:
        print(f"Reusing existing VPC prefix {config.vpc_prefix_name!r}")
        vpc_prefix_uuid = _required_string(listed_vpc_prefix, "id", "VPC prefix")
        ownership.reused_vpc_prefix_uuid = vpc_prefix_uuid
        ownership.reused_vpc_prefix_name = config.vpc_prefix_name

    vpc_prefix = nico_rest.get_vpc_prefix(vpc_prefix_uuid)
    _validate_vpc_prefix(vpc_prefix, config, site_uuid, vpc_uuid, ip_block_uuid)
    vpc_prefix = _wait_until_ready(
        "VPC prefix",
        vpc_prefix,
        nico_rest.get_vpc_prefix,
        timeout=timeout,
        poll_interval=poll_interval,
    )
    _validate_vpc_prefix(vpc_prefix, config, site_uuid, vpc_uuid, ip_block_uuid)
    vpc_prefix_uuid = _required_string(vpc_prefix, "id", "VPC prefix")
    return NetworkResourceIDs(
        vpc_uuid=vpc_uuid,
        vpc_prefix_uuid=vpc_prefix_uuid,
    )


def cleanup_network_resources(ownership: NetworkResourceOwnership) -> None:
    """Best-effort cleanup of only the resources created by this run."""
    if ownership.reused_vpc_prefix_uuid is not None:
        print(
            f"Not deleting pre-existing VPC prefix {ownership.reused_vpc_prefix_name!r} "
            f"({ownership.reused_vpc_prefix_uuid}) because it was not created by this run"
        )
    if ownership.reused_vpc_uuid is not None:
        print(
            f"Not deleting pre-existing VPC {ownership.reused_vpc_name!r} "
            f"({ownership.reused_vpc_uuid}) because it was not created by this run"
        )

    if ownership.vpc_uuid is not None or ownership.vpc_prefix_uuid is not None:
        if ownership.site_uuid is None:
            print(
                "WARNING: preserving MLT-created network resources because the "
                "owning site is unknown",
                file=sys.stderr,
            )
            return
        parent_vpc_uuid = ownership.parent_vpc_uuid or ownership.vpc_uuid
        if parent_vpc_uuid is None:
            print(
                "WARNING: preserving MLT-created network resources because the "
                "parent VPC is unknown",
                file=sys.stderr,
            )
            return
        try:
            instances = nico_rest.get_instances(ownership.site_uuid, parent_vpc_uuid)
        except Exception as error:
            print(
                "WARNING: preserving MLT-created network resources because "
                f"instance-use validation failed for VPC {parent_vpc_uuid}: {error}",
                file=sys.stderr,
            )
            return
        if instances:
            instance_ids = [instance.get("id") for instance in instances]
            print(
                "WARNING: preserving MLT-created network resources because VPC "
                f"{parent_vpc_uuid} still contains instances: {instance_ids}",
                file=sys.stderr,
            )
            return

    prefix_deleted = True
    if ownership.vpc_prefix_uuid is not None:
        print(
            f"Deleting MLT-created VPC prefix {ownership.vpc_prefix_name!r} "
            f"({ownership.vpc_prefix_uuid})"
        )
        try:
            nico_rest.delete_vpc_prefix(ownership.vpc_prefix_uuid)
            _wait_until_vpc_prefix_deleted(ownership.vpc_prefix_uuid)
        except Exception as error:
            prefix_deleted = False
            print(
                "WARNING: failed to delete MLT-created VPC prefix "
                f"{ownership.vpc_prefix_uuid}: {error}",
                file=sys.stderr,
            )
        else:
            ownership.vpc_prefix_uuid = None
            ownership.vpc_prefix_name = None

    if ownership.vpc_uuid is not None and prefix_deleted:
        print(f"Deleting MLT-created VPC {ownership.vpc_name!r} ({ownership.vpc_uuid})")
        try:
            nico_rest.delete_vpc(ownership.vpc_uuid)
        except Exception as error:
            print(
                f"WARNING: failed to delete MLT-created VPC {ownership.vpc_uuid}: {error}",
                file=sys.stderr,
            )
        else:
            ownership.vpc_uuid = None
            ownership.vpc_name = None
    elif ownership.vpc_uuid is not None:
        print(
            f"WARNING: preserving MLT-created VPC {ownership.vpc_uuid} because "
            "its VPC prefix could not be deleted",
            file=sys.stderr,
        )
