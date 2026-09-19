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
import subprocess
from types import SimpleNamespace

import pytest

from lib import admin_cli, diagnostics, kubectl, nico_rest
from lib.config import (
    DiagnosticsConfig,
    KubernetesDiagnosticsConfig,
    KubernetesLogWorkload,
)
from tests.lifecycle import machine_lifecycle_test as lifecycle


HOST_ID = "fm100ht-test-host"
DPU_IDS = ["fm100dt-test-dpu-1", "fm100dt-test-dpu-2"]


def _install_successful_probes(monkeypatch):
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_mh_show",
        lambda _machine_id, allow_missing, timeout: {
            "machine_id": HOST_ID,
            "state": "Assigned/Configuring",
            "state_reason": "WaitingForDpu",
            "health": {"alerts": [{"id": "HostNotReady", "message": "waiting"}]},
            "host_bmc_ip": "must-not-be-collected",
            "dpus": [
                {
                    "machine_id": DPU_IDS[0],
                    "state": "Ready",
                    "health": {"alerts": []},
                    "bmc_ip": "must-not-be-collected",
                },
                {
                    "machine_id": DPU_IDS[1],
                    "state": "Ready",
                    "health": {
                        "alerts": [{"id": "DpuNetworkUnhealthy", "message": "not converged"}]
                    },
                },
            ],
        },
    )
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_m_show",
        lambda machine_id, allow_missing, timeout: {
            "id": machine_id,
            "state": "Ready",
            "discovery_info": "must-not-be-collected",
        },
    )
    monkeypatch.setattr(
        admin_cli,
        "get_dpu_network_config",
        lambda dpu_id, timeout: {
            "managed_host_config_version": f"host-version-{dpu_id[-1]}",
            "instance_network_config_version": f"instance-version-{dpu_id[-1]}",
            "use_admin_network": False,
            "tenant_interfaces": ["must-not-be-collected"],
        },
    )
    monkeypatch.setattr(
        admin_cli,
        "get_dpu_network_status_text",
        lambda timeout: "\n".join(
            [
                f"2026-09-03 | {DPU_IDS[0]} | applied-1 | true | | agent-1",
                f"2026-09-03 | {DPU_IDS[1]} | applied-2 | false | alert | agent-2",
                "2026-09-03 | fm100dt-unrelated | private-version | true | | agent-other",
                f"2026-09-03 | {DPU_IDS[0]}-suffix | near-match | true | | agent-other",
                f"2026-09-03 | fm100dt-unrelated | private-version | false | {DPU_IDS[0]} | agent-other",
            ]
        ),
    )
    monkeypatch.setattr(
        nico_rest,
        "get_machine_status",
        lambda _machine_id, _site, allow_missing_machine: "Provisioning",
    )
    monkeypatch.setattr(
        nico_rest,
        "get_instance_info",
        lambda instance_id: {
            "id": instance_id,
            "status": "Provisioning",
            "machineId": HOST_ID,
            "interfaces": [{"ipAddresses": []}],
            "userData": "must-not-be-collected",
        },
    )


def test_collects_targeted_timeout_snapshot(monkeypatch, tmp_path, capsys):
    _install_successful_probes(monkeypatch)

    output_path = diagnostics.collect_timeout_diagnostics(
        config=DiagnosticsConfig(output_directory=str(tmp_path)),
        stage="assignment",
        host_id=HOST_ID,
        dpu_ids=DPU_IDS,
        site=nico_rest.Site("test-site"),
        timeout_error=TimeoutError("machine did not become Assigned/Ready"),
        instance_id="instance-id",
    )

    snapshot = json.loads(output_path.read_text(encoding="utf-8"))
    rendered = json.dumps(snapshot)
    assert snapshot["stage"] == "assignment"
    assert snapshot["managed_host"]["state"] == "Assigned/Configuring"
    assert snapshot["dpus"][DPU_IDS[1]]["health"]["alerts"][0]["id"] == ("DpuNetworkUnhealthy")
    assert snapshot["desired_dpu_network_config"][DPU_IDS[0]] == {
        "instance_network_config_version": "instance-version-1",
        "managed_host_config_version": "host-version-1",
        "use_admin_network": False,
    }
    assert len(snapshot["reported_dpu_network_status"]) == 2
    assert snapshot["cloud"]["machine_status"] == "Provisioning"
    assert snapshot["cloud"]["instance"]["id"] == "instance-id"
    assert snapshot["collection_errors"] == []
    assert "must-not-be-collected" not in rendered
    assert "fm100dt-unrelated" not in rendered
    assert "Timeout diagnostic summary" in capsys.readouterr().out


def test_probe_failures_are_recorded_without_preventing_artifact(monkeypatch, tmp_path):
    def fail(*_args, **_kwargs):
        raise RuntimeError("diagnostic API unavailable")

    monkeypatch.setattr(admin_cli, "get_machine_from_mh_show", fail)
    monkeypatch.setattr(admin_cli, "get_machine_from_m_show", fail)
    monkeypatch.setattr(admin_cli, "get_dpu_network_config", fail)
    monkeypatch.setattr(admin_cli, "get_dpu_network_status_text", fail)
    monkeypatch.setattr(nico_rest, "get_machine_status", fail)

    output_path = diagnostics.collect_timeout_diagnostics(
        config=DiagnosticsConfig(output_directory=str(tmp_path)),
        stage="ingestion",
        host_id=HOST_ID,
        dpu_ids=[DPU_IDS[0]],
        site=nico_rest.Site("test-site"),
        timeout_error=TimeoutError("not Ready"),
    )

    snapshot = json.loads(output_path.read_text(encoding="utf-8"))
    assert snapshot["timeout"]["message"] == "not Ready"
    assert len(snapshot["collection_errors"]) == 6
    assert {error["source"] for error in snapshot["collection_errors"]} == {
        "managed-host show",
        f"machine show {HOST_ID}",
        f"machine show {DPU_IDS[0]}",
        f"dpu network config {DPU_IDS[0]}",
        "dpu network status",
        f"cloud machine {HOST_ID}",
    }


def test_admin_cli_diagnostic_probes_use_short_timeout(monkeypatch, tmp_path):
    timeouts = []

    def managed_host(_machine_id, *, allow_missing, timeout):
        timeouts.append(timeout)
        return None

    def machine(_machine_id, *, allow_missing, timeout):
        timeouts.append(timeout)
        return None

    def network_config(_dpu_id, *, timeout):
        timeouts.append(timeout)
        return {}

    def network_status(*, timeout):
        timeouts.append(timeout)
        return ""

    monkeypatch.setattr(admin_cli, "get_machine_from_mh_show", managed_host)
    monkeypatch.setattr(admin_cli, "get_machine_from_m_show", machine)
    monkeypatch.setattr(admin_cli, "get_dpu_network_config", network_config)
    monkeypatch.setattr(admin_cli, "get_dpu_network_status_text", network_status)
    monkeypatch.setattr(nico_rest, "get_machine_status", lambda *_args, **_kwargs: "Ready")

    diagnostics.collect_timeout_diagnostics(
        config=DiagnosticsConfig(output_directory=str(tmp_path)),
        stage="ingestion",
        host_id=HOST_ID,
        dpu_ids=[DPU_IDS[0]],
        site=nico_rest.Site("test-site"),
        timeout_error=TimeoutError("not Ready"),
    )

    assert len(timeouts) == 5
    assert set(timeouts) == {diagnostics.DIAGNOSTIC_PROBE_TIMEOUT_SECONDS}


def test_machine_show_allow_missing_swallows_machine_not_found(monkeypatch):
    missing = subprocess.CalledProcessError(
        1,
        ["nico-admin-cli", "machine", "show", HOST_ID],
        stderr=f"Error: machine {HOST_ID} not found",
    )
    monkeypatch.setattr(
        admin_cli,
        "run_admin_cli",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(missing),
    )

    assert admin_cli.get_machine_from_m_show(HOST_ID, allow_missing=True) is None


@pytest.mark.parametrize(
    "stderr",
    [
        "Error from server (Forbidden)",
        'Error from server (NotFound): pods "nico-api-123" not found',
        "Unable to connect to the server",
    ],
)
def test_machine_show_allow_missing_preserves_other_failures(monkeypatch, stderr):
    failure = subprocess.CalledProcessError(
        1,
        ["kubectl", "exec"],
        stderr=stderr,
    )
    monkeypatch.setattr(
        admin_cli,
        "run_admin_cli",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(failure),
    )

    with pytest.raises(subprocess.CalledProcessError) as raised:
        admin_cli.get_machine_from_m_show(HOST_ID, allow_missing=True)

    assert raised.value is failure


def test_collects_bounded_current_and_previous_kubernetes_logs(monkeypatch, tmp_path, capsys):
    _install_successful_probes(monkeypatch)
    calls = []
    monkeypatch.setattr(
        kubectl,
        "list_deployment_pod_containers",
        lambda namespace, deployment, timeout: [
            kubectl.PodContainer(pod="api-pod", container="api", restart_count=1),
            kubectl.PodContainer(pod="idle-pod", container="sidecar", restart_count=0),
        ],
    )

    def logs(
        namespace,
        pod,
        container,
        *,
        lookback_minutes,
        max_bytes,
        previous,
        timeout,
    ):
        calls.append((container, lookback_minutes, max_bytes, previous, timeout))
        if container == "sidecar":
            return ""
        return f"secret {container} previous={previous}\n"

    monkeypatch.setattr(kubectl, "get_pod_logs", logs)
    config = DiagnosticsConfig(
        output_directory=str(tmp_path),
        kubernetes=KubernetesDiagnosticsConfig(
            enabled=True,
            lookback_minutes=12,
            max_bytes_per_container=2048,
            workloads=(KubernetesLogWorkload(namespace="forge-system", deployment="nico-api"),),
        ),
    )

    output_path = diagnostics.collect_timeout_diagnostics(
        config=config,
        stage="assignment",
        host_id=HOST_ID,
        dpu_ids=DPU_IDS,
        site=nico_rest.Site("test-site"),
        timeout_error=TimeoutError("not Ready"),
    )

    snapshot = json.loads(output_path.read_text(encoding="utf-8"))
    containers = snapshot["kubernetes_logs"][0]["containers"]
    assert output_path.name == "diagnostics.json"
    assert output_path.parent.parent == tmp_path
    assert f"-assignment-{HOST_ID}" in output_path.parent.name
    assert len(containers) == 2
    assert "previous" in containers[0]
    assert "previous" not in containers[1]
    current_path = output_path.parent / containers[0]["current"]["path"]
    assert current_path.read_text(encoding="utf-8").startswith("secret")
    previous_path = output_path.parent / containers[0]["previous"]["path"]
    assert previous_path.read_text(encoding="utf-8").startswith("secret")
    assert containers[1]["current"] == {"bytes": 0, "empty": True}
    assert not (
        output_path.parent
        / "kubernetes"
        / "forge-system"
        / "nico-api"
        / "idle-pod"
    ).exists()
    assert calls == [
        ("api", 12, 2048, False, 30),
        ("api", 12, 2048, True, 30),
        ("sidecar", 12, 2048, False, 30),
    ]
    assert "secret api" not in capsys.readouterr().out


def test_default_kubernetes_lookback_covers_time_since_run_started(monkeypatch, tmp_path):
    _install_successful_probes(monkeypatch)
    collected_lookbacks = []
    monkeypatch.setattr(diagnostics.time, "monotonic", lambda: 12 * 60 + 1)
    monkeypatch.setattr(
        diagnostics,
        "_collect_kubernetes_logs",
        lambda _snapshot, _config, _event_directory, lookback: (
            collected_lookbacks.append(lookback)
        ),
    )

    diagnostics.collect_timeout_diagnostics(
        config=DiagnosticsConfig(
            output_directory=str(tmp_path),
            kubernetes=KubernetesDiagnosticsConfig(enabled=True),
        ),
        stage="ingestion",
        host_id=HOST_ID,
        dpu_ids=DPU_IDS,
        site=nico_rest.Site("test-site"),
        timeout_error=TimeoutError("not Ready"),
        run_started_at=0,
    )

    assert collected_lookbacks == [13]


def test_deployment_with_no_pods_is_recorded_as_collection_error(monkeypatch, tmp_path):
    _install_successful_probes(monkeypatch)
    monkeypatch.setattr(kubectl, "_deployment_pods", lambda *_args, **_kwargs: [])

    output_path = diagnostics.collect_timeout_diagnostics(
        config=DiagnosticsConfig(
            output_directory=str(tmp_path),
            kubernetes=KubernetesDiagnosticsConfig(
                enabled=True,
                workloads=(
                    KubernetesLogWorkload(
                        namespace="forge-system",
                        deployment="nico-api",
                    ),
                ),
            ),
        ),
        stage="ingestion",
        host_id=HOST_ID,
        dpu_ids=DPU_IDS,
        site=nico_rest.Site("test-site"),
        timeout_error=TimeoutError("not Ready"),
        run_started_at=0,
    )

    snapshot = json.loads(output_path.read_text(encoding="utf-8"))
    assert snapshot["collection_errors"][-1] == {
        "error_type": "RuntimeError",
        "message": "Deployment forge-system/nico-api has no pods",
        "source": "Kubernetes deployment forge-system/nico-api",
    }


def test_disabled_diagnostics_do_not_run_probes(monkeypatch, tmp_path):
    def fail_if_called(*_args, **_kwargs):
        raise AssertionError("diagnostic probe should not run")

    monkeypatch.setattr(admin_cli, "get_machine_from_mh_show", fail_if_called)

    output_path = diagnostics.collect_timeout_diagnostics(
        config=DiagnosticsConfig(enabled=False, output_directory=str(tmp_path)),
        stage="ingestion",
        host_id=HOST_ID,
        dpu_ids=DPU_IDS,
        site=nico_rest.Site("test-site"),
        timeout_error=TimeoutError("not Ready"),
    )

    assert output_path is None
    assert list(tmp_path.iterdir()) == []


def test_null_managed_host_dpus_do_not_abort_collection(monkeypatch, tmp_path):
    _install_successful_probes(monkeypatch)
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_mh_show",
        lambda _machine_id, allow_missing, timeout: {"dpus": None},
    )

    output_path = diagnostics.collect_timeout_diagnostics(
        config=DiagnosticsConfig(output_directory=str(tmp_path)),
        stage="ingestion",
        host_id=HOST_ID,
        dpu_ids=DPU_IDS,
        site=nico_rest.Site("test-site"),
        timeout_error=TimeoutError("not Ready"),
    )

    snapshot = json.loads(output_path.read_text(encoding="utf-8"))
    assert snapshot["managed_host"] == {}
    assert set(snapshot["dpus"]) == set(DPU_IDS)
    assert snapshot["desired_dpu_network_config"][DPU_IDS[0]] is not None
    assert snapshot["cloud"]["machine_status"] == "Provisioning"


def test_admin_cli_text_returns_stdout_without_format_flag(monkeypatch):
    recorded = {}

    def run(command, **_kwargs):
        recorded["command"] = command
        return subprocess.CompletedProcess(command, 0, stdout="status table\n", stderr="")

    monkeypatch.setattr(admin_cli.subprocess, "run", run)

    assert admin_cli.run_admin_cli_text(["dpu", "network", "status"]) == "status table\n"
    assert "--format" not in recorded["command"]


def test_ingestion_timeout_triggers_diagnostics_before_failure(monkeypatch):
    calls = []
    test_config = SimpleNamespace(
        machine_under_test=HOST_ID,
        skip_factory_reset=True,
        test_sitewide_bmc_fallback=False,
        diagnostics=DiagnosticsConfig(),
    )
    site_config = SimpleNamespace(site=nico_rest.Site("test-site"))
    machine_info = SimpleNamespace(
        machine_under_test_dpu=DPU_IDS[0],
        machine_under_test_predicted_host="fm100hp-predicted",
        dpu_ids=DPU_IDS,
    )

    monkeypatch.setattr(admin_cli, "force_delete_machine", lambda *_args, **_kwargs: None)
    monkeypatch.setattr(
        admin_cli,
        "wait_for_machine_hostinitializing",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(TimeoutError("ingestion timed out")),
    )
    monkeypatch.setattr(
        diagnostics,
        "collect_timeout_diagnostics",
        lambda **kwargs: calls.append(kwargs),
    )
    monkeypatch.setattr(
        lifecycle,
        "_error_and_exit",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(SystemExit(1)),
    )

    with pytest.raises(SystemExit):
        lifecycle.force_delete_and_await_reingestion(
            test_config,
            site_config,
            machine_info,
            run_started_at=123.0,
        )

    assert len(calls) == 1
    assert calls[0]["stage"] == "ingestion"
    assert calls[0]["dpu_ids"] == DPU_IDS
    assert calls[0]["run_started_at"] == 123.0
    assert isinstance(calls[0]["timeout_error"], TimeoutError)


def test_assignment_timeout_triggers_diagnostics_with_instance_id(monkeypatch):
    calls = []
    test_config = SimpleNamespace(
        machine_under_test=HOST_ID,
        expected_dpu_count=2,
        diagnostics=DiagnosticsConfig(),
    )
    site_config = SimpleNamespace(site=nico_rest.Site("test-site"))
    machine_info = SimpleNamespace(dpu_ids=DPU_IDS)
    ngc_uuids = SimpleNamespace(
        network_interface={"vpcId": "vpc-id", "ipFamilies": ["IPv4"]},
        os_uuid="os-id",
        vpc_uuid="vpc-id",
    )

    monkeypatch.setattr(nico_rest, "create_instance", lambda **_kwargs: {"id": "instance-id"})
    monkeypatch.setattr(
        admin_cli,
        "wait_for_machine_assigned_ready",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(TimeoutError("assignment timed out")),
    )
    monkeypatch.setattr(
        diagnostics,
        "collect_timeout_diagnostics",
        lambda **kwargs: calls.append(kwargs),
    )
    monkeypatch.setattr(
        lifecycle,
        "_error_and_exit",
        lambda *_args, **_kwargs: (_ for _ in ()).throw(SystemExit(1)),
    )

    with pytest.raises(SystemExit):
        lifecycle.create_instance_and_verify(
            test_config,
            site_config,
            machine_info,
            ngc_uuids,
            object(),
            run_started_at=123.0,
        )

    assert len(calls) == 1
    assert calls[0]["stage"] == "assignment"
    assert calls[0]["instance_id"] == "instance-id"
    assert calls[0]["run_started_at"] == 123.0
    assert isinstance(calls[0]["timeout_error"], TimeoutError)
