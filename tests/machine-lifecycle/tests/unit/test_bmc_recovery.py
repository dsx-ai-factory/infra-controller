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

from lib import admin_cli
from tests.lifecycle import machine_lifecycle_test as lifecycle


def _machine_info():
    return SimpleNamespace(
        host_bmc_ip="192.0.2.10",
        host_bmc_mac="02:00:00:00:00:01",
        dpu_ids=["dpu-one", "dpu-two"],
        dpu_info_map={
            "dpu-one": {
                "bmc_ip": "192.0.2.11",
                "bmc_mac": "02:00:00:00:00:02",
            },
            "dpu-two": {
                "bmc_ip": "192.0.2.12",
                "bmc_mac": "02:00:00:00:00:03",
            },
        },
    )


# A real `credential rotation-status --type=bmc --mac-address <MAC>` response
# from a site that has never rotated its site-wide BMC credential.
NEVER_ROTATED_STATUS = {
    "credential_type": "bmc",
    "target_version": 0,
    "converged": 1,
    "pending": 0,
    "quarantined": 0,
    "complete": True,
    "quarantined_device_macs": [],
    "device": {
        "device_mac": "38:25:f3:3a:d7:f1",
        "current_version": 0,
        "rotating_to_version": None,
        "converged": True,
        "quarantined": False,
        "quarantined_until": None,
        "rotate_attempts": 0,
        "last_attempt_at": None,
        "last_error": None,
    },
}


def test_rotation_target_version_read_from_the_site_aggregate(monkeypatch):
    monkeypatch.setattr(admin_cli, "run_admin_cli", lambda _args: NEVER_ROTATED_STATUS)

    assert admin_cli.get_sitewide_bmc_rotation_target_version() == 0


@pytest.mark.parametrize("target_version", [None, "3", True, -1])
def test_rotation_target_version_rejects_unusable_values(monkeypatch, target_version):
    # Guessing a version here would read a superseded credential.
    monkeypatch.setattr(
        admin_cli, "run_admin_cli", lambda _args: {"target_version": target_version}
    )

    with pytest.raises(ValueError):
        admin_cli.get_sitewide_bmc_rotation_target_version()


def test_device_status_scopes_the_query_and_carries_the_site_target(monkeypatch):
    issued = []

    def fake_run(args):
        issued.append(args)
        return NEVER_ROTATED_STATUS

    monkeypatch.setattr(admin_cli, "run_admin_cli", fake_run)

    status = admin_cli.get_bmc_rotation_device_status("38:25:F3:3A:D7:F1")

    assert issued == [
        [
            "credential",
            "rotation-status",
            "--type=bmc",
            "--mac-address",
            "38:25:F3:3A:D7:F1",
        ]
    ]
    assert status["converged"] is True
    assert status["current_version"] == 0
    # Injected so the caller can report how far a lagging device is behind.
    assert status["target_version"] == 0


def test_device_status_rejects_a_response_without_a_device_report(monkeypatch):
    monkeypatch.setattr(admin_cli, "run_admin_cli", lambda _args: {"target_version": 0})

    with pytest.raises(ValueError, match="no device report"):
        admin_cli.get_bmc_rotation_device_status("38:25:F3:3A:D7:F1")


def test_refresh_site_explorer_endpoint_rejects_reported_error(monkeypatch):
    monkeypatch.setattr(
        admin_cli,
        "run_admin_cli",
        lambda _args: {
            "report": {
                "last_exploration_error": '{"Type":"Unauthorized"}',
            }
        },
    )

    with pytest.raises(RuntimeError, match="reported Unauthorized"):
        admin_cli.refresh_site_explorer_endpoint("192.0.2.11")


def test_refresh_site_explorer_endpoint_accepts_successful_report(monkeypatch):
    report = {
        "last_exploration_error": None,
        "vendor": "NvidiaDpu",
    }
    monkeypatch.setattr(
        admin_cli,
        "run_admin_cli",
        lambda _args: {"report": report},
    )

    assert admin_cli.refresh_site_explorer_endpoint("192.0.2.11") == report


def test_endpoint_status_distinguishes_missing_success_and_error(monkeypatch):
    results = iter(
        [
            {},
            {"report": {"last_exploration_error": None, "vendor": "NvidiaDpu"}},
            {"report": {"last_exploration_error": '{"Type":"AvoidLockout"}'}},
        ]
    )
    monkeypatch.setattr(admin_cli, "run_admin_cli", lambda _args: next(results))

    assert admin_cli.get_site_explorer_endpoint_status("192.0.2.11") == (False, None)
    assert admin_cli.get_site_explorer_endpoint_status("192.0.2.11") == (True, None)
    assert admin_cli.get_site_explorer_endpoint_status("192.0.2.11") == (
        False,
        "AvoidLockout",
    )


def test_recovery_covers_host_and_dpus_and_retries_failures_once(monkeypatch):
    machine_info = _machine_info()
    endpoints_seen = []
    sleeps = []

    monkeypatch.setattr(
        lifecycle.admin_cli,
        "get_site_explorer_endpoint_status",
        lambda _ip: (False, "Unauthorized"),
    )
    monkeypatch.setattr(lifecycle.time, "time", lambda: 0)
    monkeypatch.setattr(lifecycle.time, "sleep", sleeps.append)

    def fake_refresh(endpoints):
        endpoints_seen.append(set(endpoints))
        if len(endpoints_seen) == 1:
            host = next(endpoint for endpoint in endpoints if endpoint.name == "host")
            return {host: "Site Explorer refresh reported Unauthorized"}
        return {}

    monkeypatch.setattr(lifecycle, "_refresh_bmc_endpoints", fake_refresh)

    lifecycle._wait_for_bmc_lockout_and_recover(machine_info)

    first_attempt, second_attempt = endpoints_seen
    assert {endpoint.name for endpoint in first_attempt} == {
        "host",
        "DPU dpu-one",
        "DPU dpu-two",
    }
    assert {endpoint.name for endpoint in second_attempt} == {"host"}
    assert sleeps == [600, 600]


def test_refresh_verifies_all_credentials_in_one_site_vault_session(monkeypatch):
    machine_info = _machine_info()
    sessions = []
    credential_reads = []

    class FakeSiteVaultClient:
        def __enter__(self):
            sessions.append(self)
            return self

        def __exit__(self, exc_type, exc_val, exc_tb):
            return None

        def get_bmc_credentials(self, mac):
            credential_reads.append(mac)

    monkeypatch.setattr(lifecycle, "SiteVaultClient", FakeSiteVaultClient)
    monkeypatch.setattr(
        lifecycle.admin_cli,
        "clear_site_explorer_error",
        lambda _ip: None,
    )
    monkeypatch.setattr(
        lifecycle.admin_cli,
        "refresh_site_explorer_endpoint",
        lambda _ip: {},
    )

    endpoints = set(lifecycle._machine_bmc_endpoints(machine_info))
    failures = lifecycle._refresh_bmc_endpoints(endpoints)

    assert failures == {}
    assert len(sessions) == 1
    assert set(credential_reads) == {
        machine_info.host_bmc_mac,
        machine_info.dpu_info_map["dpu-one"]["bmc_mac"],
        machine_info.dpu_info_map["dpu-two"]["bmc_mac"],
    }


def test_clean_endpoint_reports_skip_lockout_recovery(monkeypatch):
    machine_info = _machine_info()
    refreshed = []
    sessions = []
    credential_reads = []

    class FakeSiteVaultClient:
        def __enter__(self):
            sessions.append(self)
            return self

        def __exit__(self, exc_type, exc_val, exc_tb):
            return None

        def get_bmc_credentials(self, mac):
            credential_reads.append(mac)

    monkeypatch.setattr(
        lifecycle.admin_cli,
        "get_site_explorer_endpoint_status",
        lambda _ip: (True, None),
    )
    monkeypatch.setattr(lifecycle, "SiteVaultClient", FakeSiteVaultClient)
    monkeypatch.setattr(lifecycle.time, "time", lambda: 0)
    monkeypatch.setattr(
        lifecycle.time,
        "sleep",
        lambda _seconds: pytest.fail("clean recovery must not sleep"),
    )
    monkeypatch.setattr(
        lifecycle,
        "_refresh_bmc_endpoints",
        lambda *_args: refreshed.append(True),
    )

    lifecycle._wait_for_bmc_lockout_and_recover(machine_info)

    assert refreshed == []
    assert len(sessions) == 1
    assert set(credential_reads) == {
        machine_info.host_bmc_mac,
        machine_info.dpu_info_map["dpu-one"]["bmc_mac"],
        machine_info.dpu_info_map["dpu-two"]["bmc_mac"],
    }


def test_recovery_fails_after_second_refresh_failure(monkeypatch):
    machine_info = _machine_info()
    attempts = []

    monkeypatch.setattr(
        lifecycle.admin_cli,
        "get_site_explorer_endpoint_status",
        lambda _ip: (False, "Unauthorized"),
    )
    monkeypatch.setattr(lifecycle.time, "time", lambda: 0)
    monkeypatch.setattr(lifecycle.time, "sleep", lambda _seconds: None)

    def always_fail(endpoints):
        attempts.append(set(endpoints))
        return {
            endpoint: lifecycle.RecoveryFailure("reported Unauthorized")
            for endpoint in endpoints
        }

    monkeypatch.setattr(lifecycle, "_refresh_bmc_endpoints", always_fail)

    def fail_test(message):
        raise RuntimeError(message)

    monkeypatch.setattr(lifecycle, "_error_and_exit", fail_test)

    with pytest.raises(RuntimeError, match="after two Site Explorer refresh attempts"):
        lifecycle._wait_for_bmc_lockout_and_recover(machine_info)

    assert len(attempts) == 2
