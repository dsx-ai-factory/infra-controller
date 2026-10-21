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

import datetime
from types import SimpleNamespace

import pytest

from lib import nico_rest, os_janitor
from lib.ephemeral_os import TEMPORARY_OS_DESCRIPTION
from tests.lifecycle import machine_lifecycle_test as lifecycle


NOW = datetime.datetime(2026, 9, 9, 12, tzinfo=datetime.timezone.utc)


def _operating_system(
    operating_system_id: str = "os-1",
    *,
    age_hours: int = 48,
    **overrides,
) -> dict:
    operating_system = {
        "id": operating_system_id,
        "name": "mlt-os-012345abcdef",
        "description": TEMPORARY_OS_DESCRIPTION,
        "tenantId": "tenant-1",
        "infrastructureProviderId": None,
        "type": "iPXE",
        "status": "Ready",
        "created": (NOW - datetime.timedelta(hours=age_hours)).isoformat(),
    }
    operating_system.update(overrides)
    return operating_system


def _prepare_candidate(monkeypatch, operating_system=None, instances=None):
    candidate = operating_system or _operating_system()
    monkeypatch.setattr(
        nico_rest,
        "list_operating_systems",
        lambda **_kwargs: [candidate],
    )
    monkeypatch.setattr(
        nico_rest,
        "get_operating_system",
        lambda _operating_system_id: dict(candidate),
    )
    monkeypatch.setattr(
        nico_rest,
        "list_instances_for_operating_system",
        lambda _operating_system_id: list(instances or []),
    )
    return candidate


def test_dry_run_reports_eligible_os_without_deleting(monkeypatch):
    _prepare_candidate(monkeypatch)
    monkeypatch.setattr(
        nico_rest,
        "delete_operating_system",
        lambda _operating_system_id: pytest.fail("dry run must not delete"),
    )

    summary = os_janitor.cleanup_stale_operating_systems(
        minimum_age=datetime.timedelta(hours=24),
        dry_run=True,
        now=NOW,
    )

    assert summary == os_janitor.OSJanitorSummary(
        scanned=1,
        managed=1,
        too_young=0,
        in_use=0,
        would_delete=1,
        deleted=0,
        preserved=0,
        failed=0,
    )


def test_deletes_eligible_unused_os(monkeypatch):
    candidate = _prepare_candidate(monkeypatch)
    deleted = []
    monkeypatch.setattr(nico_rest, "delete_operating_system", deleted.append)

    summary = os_janitor.cleanup_stale_operating_systems(
        minimum_age=datetime.timedelta(hours=24),
        dry_run=False,
        now=NOW,
    )

    assert deleted == [candidate["id"]]
    assert summary.deleted == 1


def test_preserves_os_referenced_by_an_instance(monkeypatch):
    candidate = _prepare_candidate(
        monkeypatch,
        instances=[{"id": "instance-1", "operatingSystemId": "os-1"}],
    )
    monkeypatch.setattr(
        nico_rest,
        "delete_operating_system",
        lambda _operating_system_id: pytest.fail("in-use OS must not be deleted"),
    )

    summary = os_janitor.cleanup_stale_operating_systems(
        minimum_age=datetime.timedelta(hours=24),
        dry_run=False,
        now=NOW,
    )

    assert candidate["id"] == "os-1"
    assert summary.in_use == 1
    assert summary.preserved == 1


@pytest.mark.parametrize(
    "overrides",
    [
        {"name": "mlt-os-not-a-generated-id"},
        {"description": "user-managed OS"},
        {"tenantId": None},
        {"infrastructureProviderId": "provider-1"},
        {"type": "Virtual"},
    ],
)
def test_ignores_operating_systems_without_all_ownership_markers(
    monkeypatch, overrides
):
    candidate = _operating_system(**overrides)
    monkeypatch.setattr(
        nico_rest,
        "list_operating_systems",
        lambda **_kwargs: [candidate],
    )
    monkeypatch.setattr(
        nico_rest,
        "get_operating_system",
        lambda _operating_system_id: pytest.fail("unmanaged OS must not be refreshed"),
    )

    summary = os_janitor.cleanup_stale_operating_systems(
        minimum_age=datetime.timedelta(hours=24),
        dry_run=False,
        now=NOW,
    )

    assert summary.managed == 0
    assert summary.deleted == 0


@pytest.mark.parametrize(
    "overrides",
    [
        {"age_hours": 1},
        {"created": "not-a-timestamp"},
        {"created": "2026-09-01T12:00:00"},
        {"status": "Unknown"},
        {"status": "Deleting"},
    ],
)
def test_preserves_ineligible_candidates_before_refresh(monkeypatch, overrides):
    age_hours = overrides.pop("age_hours", 48)
    candidate = _operating_system(age_hours=age_hours, **overrides)
    monkeypatch.setattr(
        nico_rest,
        "list_operating_systems",
        lambda **_kwargs: [candidate],
    )
    monkeypatch.setattr(
        nico_rest,
        "get_operating_system",
        lambda _operating_system_id: pytest.fail("candidate must not be refreshed"),
    )

    summary = os_janitor.cleanup_stale_operating_systems(
        minimum_age=datetime.timedelta(hours=24),
        dry_run=False,
        now=NOW,
    )

    assert summary.deleted == 0
    if age_hours == 1:
        assert summary.too_young == 1
    else:
        assert summary.preserved == 1


def test_preserves_candidate_when_refreshed_ownership_markers_change(monkeypatch):
    candidate = _prepare_candidate(monkeypatch)
    changed = {**candidate, "description": "changed after list"}
    monkeypatch.setattr(
        nico_rest,
        "get_operating_system",
        lambda _operating_system_id: changed,
    )
    monkeypatch.setattr(
        nico_rest,
        "list_instances_for_operating_system",
        lambda _operating_system_id: pytest.fail("changed OS must not be checked"),
    )

    summary = os_janitor.cleanup_stale_operating_systems(
        minimum_age=datetime.timedelta(hours=24),
        dry_run=False,
        now=NOW,
    )

    assert summary.preserved == 1
    assert summary.deleted == 0


def test_candidate_failure_is_local_and_later_candidates_are_deleted(monkeypatch):
    first = _operating_system("os-1")
    second = _operating_system("os-2", name="mlt-os-fedcba654321")
    monkeypatch.setattr(
        nico_rest,
        "list_operating_systems",
        lambda **_kwargs: [first, second],
    )
    monkeypatch.setattr(
        nico_rest,
        "get_operating_system",
        lambda operating_system_id: first if operating_system_id == "os-1" else second,
    )

    def instances(operating_system_id):
        if operating_system_id == "os-1":
            raise nico_rest.NicoError("instance lookup unavailable")
        return []

    deleted = []
    monkeypatch.setattr(nico_rest, "list_instances_for_operating_system", instances)
    monkeypatch.setattr(nico_rest, "delete_operating_system", deleted.append)

    summary = os_janitor.cleanup_stale_operating_systems(
        minimum_age=datetime.timedelta(hours=24),
        dry_run=False,
        now=NOW,
    )

    assert deleted == ["os-2"]
    assert summary.failed == 1
    assert summary.preserved == 1
    assert summary.deleted == 1


@pytest.mark.parametrize(
    "minimum_age, now",
    [
        (datetime.timedelta(0), NOW),
        (datetime.timedelta(hours=1), datetime.datetime(2026, 9, 9, 12)),
    ],
)
def test_rejects_unsafe_time_configuration(minimum_age, now):
    with pytest.raises(ValueError):
        os_janitor.cleanup_stale_operating_systems(
            minimum_age=minimum_age,
            dry_run=True,
            now=now,
        )


def test_lifecycle_preflight_passes_configured_safeguards(monkeypatch):
    calls = []
    test_config = SimpleNamespace(
        os_janitor=SimpleNamespace(
            enabled=True,
            minimum_age_hours=72,
            dry_run=False,
        )
    )
    monkeypatch.setattr(
        os_janitor,
        "cleanup_stale_operating_systems",
        lambda **kwargs: calls.append(kwargs),
    )

    lifecycle._run_os_janitor(test_config)

    assert calls == [
        {"minimum_age": datetime.timedelta(hours=72), "dry_run": False}
    ]


def test_lifecycle_preflight_failure_does_not_abort_mlt(monkeypatch, capsys):
    test_config = SimpleNamespace(
        os_janitor=SimpleNamespace(
            enabled=True,
            minimum_age_hours=24,
            dry_run=True,
        )
    )

    def fail(**_kwargs):
        raise nico_rest.NicoError("NICo unavailable")

    monkeypatch.setattr(os_janitor, "cleanup_stale_operating_systems", fail)

    lifecycle._run_os_janitor(test_config)

    assert "continuing MLT" in capsys.readouterr().err
