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

"""Safely remove stale, unused operating systems created by interrupted MLT runs."""

from __future__ import annotations

import datetime
import re
import sys
from dataclasses import dataclass

from lib import nico_rest
from lib.ephemeral_os import TEMPORARY_OS_DESCRIPTION, TEMPORARY_OS_NAME_PREFIX


_TEMPORARY_OS_NAME = re.compile(
    rf"^{re.escape(TEMPORARY_OS_NAME_PREFIX)}[0-9a-f]{{12}}$"
)
_KNOWN_STATUSES = {
    "Pending",
    "Provisioning",
    "Syncing",
    "Ready",
    "Deleting",
    "Error",
    "Deactivated",
}


@dataclass(frozen=True)
class OSJanitorSummary:
    """Outcome counts from one janitor pass."""

    scanned: int
    managed: int
    too_young: int
    in_use: int
    would_delete: int
    deleted: int
    preserved: int
    failed: int


def _managed_os_id(
    operating_system: object, expected_id: str | None = None
) -> str | None:
    """Return the ID only when every durable MLT ownership marker matches."""
    if not isinstance(operating_system, dict):
        return None
    operating_system_id = operating_system.get("id")
    name = operating_system.get("name")
    tenant_id = operating_system.get("tenantId")
    if (
        not isinstance(operating_system_id, str)
        or not operating_system_id.strip()
        or (expected_id is not None and operating_system_id != expected_id)
        or not isinstance(name, str)
        or _TEMPORARY_OS_NAME.fullmatch(name) is None
        or operating_system.get("description") != TEMPORARY_OS_DESCRIPTION
        or not isinstance(tenant_id, str)
        or not tenant_id.strip()
        or operating_system.get("infrastructureProviderId") is not None
        or operating_system.get("type") != "iPXE"
    ):
        return None
    return operating_system_id


def _created_at(operating_system: dict) -> datetime.datetime | None:
    value = operating_system.get("created")
    if not isinstance(value, str):
        return None
    try:
        created = datetime.datetime.fromisoformat(value)
    except ValueError:
        return None
    if created.tzinfo is None or created.utcoffset() is None:
        return None
    return created


def _is_old_enough(
    operating_system: dict,
    *,
    cutoff: datetime.datetime,
) -> bool | None:
    created = _created_at(operating_system)
    if created is None:
        return None
    return created <= cutoff


def cleanup_stale_operating_systems(
    *,
    minimum_age: datetime.timedelta,
    dry_run: bool,
    now: datetime.datetime | None = None,
) -> OSJanitorSummary:
    """Delete old MLT OS definitions only after proving that they are unused."""
    if minimum_age <= datetime.timedelta(0):
        raise ValueError("minimum_age must be greater than zero")

    current_time = now or datetime.datetime.now(datetime.timezone.utc)
    if current_time.tzinfo is None or current_time.utcoffset() is None:
        raise ValueError("now must be timezone-aware")
    cutoff = current_time - minimum_age

    operating_systems = nico_rest.list_operating_systems(
        query=TEMPORARY_OS_NAME_PREFIX,
        operating_system_type="iPXE",
    )
    managed = 0
    too_young = 0
    in_use = 0
    would_delete = 0
    deleted = 0
    preserved = 0
    failed = 0

    for listed_os in operating_systems:
        operating_system_id = _managed_os_id(listed_os)
        if operating_system_id is None:
            continue
        managed += 1

        status = listed_os.get("status")
        if status not in _KNOWN_STATUSES:
            preserved += 1
            print(
                f"WARNING: preserving temporary OS {operating_system_id}: "
                f"unknown status {status!r}",
                file=sys.stderr,
            )
            continue
        if status == "Deleting":
            preserved += 1
            print(f"Preserving temporary OS {operating_system_id}: deletion is already pending")
            continue

        old_enough = _is_old_enough(listed_os, cutoff=cutoff)
        if old_enough is None:
            preserved += 1
            print(
                f"WARNING: preserving temporary OS {operating_system_id}: "
                f"invalid creation timestamp {listed_os.get('created')!r}",
                file=sys.stderr,
            )
            continue
        if not old_enough:
            too_young += 1
            continue

        try:
            current_os = nico_rest.get_operating_system(operating_system_id)
        except Exception as error:
            failed += 1
            preserved += 1
            print(
                f"WARNING: preserving temporary OS {operating_system_id}: "
                f"refresh failed: {error}",
                file=sys.stderr,
            )
            continue

        if _managed_os_id(current_os, expected_id=operating_system_id) is None:
            preserved += 1
            print(
                f"WARNING: preserving temporary OS {operating_system_id}: "
                "its ownership markers changed",
                file=sys.stderr,
            )
            continue
        current_status = current_os.get("status")
        if current_status not in _KNOWN_STATUSES or current_status == "Deleting":
            preserved += 1
            print(
                f"Preserving temporary OS {operating_system_id}: "
                f"current status is {current_status!r}"
            )
            continue
        current_old_enough = _is_old_enough(current_os, cutoff=cutoff)
        if current_old_enough is not True:
            preserved += 1
            print(
                f"WARNING: preserving temporary OS {operating_system_id}: "
                "its refreshed creation timestamp is absent, invalid, or too recent",
                file=sys.stderr,
            )
            continue

        try:
            instances = nico_rest.list_instances_for_operating_system(
                operating_system_id
            )
        except Exception as error:
            failed += 1
            preserved += 1
            print(
                f"WARNING: preserving temporary OS {operating_system_id}: "
                f"instance-use validation failed: {error}",
                file=sys.stderr,
            )
            continue
        if instances:
            in_use += 1
            preserved += 1
            instance_ids = [instance.get("id") for instance in instances]
            print(
                f"Preserving temporary OS {operating_system_id}: "
                f"referenced by instances {instance_ids}"
            )
            continue

        name = current_os["name"]
        if dry_run:
            would_delete += 1
            print(
                f"DRY RUN: would delete stale, unused temporary OS "
                f"{name!r} ({operating_system_id})"
            )
            continue

        print(f"Deleting stale, unused temporary OS {name!r} ({operating_system_id})")
        try:
            nico_rest.delete_operating_system(operating_system_id)
        except Exception as error:
            failed += 1
            preserved += 1
            print(
                f"WARNING: failed to delete temporary OS {operating_system_id}: {error}",
                file=sys.stderr,
            )
        else:
            deleted += 1

    summary = OSJanitorSummary(
        scanned=len(operating_systems),
        managed=managed,
        too_young=too_young,
        in_use=in_use,
        would_delete=would_delete,
        deleted=deleted,
        preserved=preserved,
        failed=failed,
    )
    print(
        "OS janitor summary: "
        f"scanned={summary.scanned}, managed={summary.managed}, "
        f"too_young={summary.too_young}, in_use={summary.in_use}, "
        f"would_delete={summary.would_delete}, deleted={summary.deleted}, "
        f"preserved={summary.preserved}, failed={summary.failed}"
    )
    return summary
