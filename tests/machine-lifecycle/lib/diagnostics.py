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

"""Best-effort diagnostic snapshots for lifecycle timeouts."""

from __future__ import annotations

import datetime
import json
import math
import re
import sys
import time
from collections.abc import Callable, Iterable, Mapping
from pathlib import Path
from typing import Any

from lib import admin_cli, kubectl, nico_rest
from lib.config import DiagnosticsConfig, KubernetesDiagnosticsConfig


DIAGNOSTIC_PROBE_TIMEOUT_SECONDS = 30

_MACHINE_FIELDS = (
    "id",
    "machine_id",
    "state",
    "state_version",
    "state_reason",
    "failure_details",
    "health",
    "health_sources",
    "last_observation_time",
)
_MANAGED_HOST_FIELDS = (
    "machine_id",
    "state",
    "state_version",
    "state_reason",
    "time_in_state",
    "state_sla_duration",
    "time_in_state_above_sla",
    "failure_details",
    "maintenance_reference",
    "maintenance_start_time",
    "health",
    "health_sources",
)
_DPU_FIELDS = (
    "machine_id",
    "state",
    "failure_details",
    "last_observation_time",
    "last_reboot_time",
    "last_reboot_requested_time_and_mode",
    "is_primary",
    "health",
)
_NETWORK_CONFIG_FIELDS = (
    "managed_host_config_version",
    "instance_network_config_version",
    "instance_id",
    "is_primary_dpu",
    "use_admin_network",
    "network_virtualization_type",
)
_INSTANCE_FIELDS = (
    "id",
    "name",
    "status",
    "statusReason",
    "machineId",
    "siteId",
    "vpcId",
    "operatingSystemId",
    "interfaces",
    "createdAt",
    "updatedAt",
)


def _selected_fields(value: Any, fields: Iterable[str]) -> dict[str, Any] | None:
    if not isinstance(value, Mapping):
        return None
    return {field: value[field] for field in fields if field in value}


def _record_error(snapshot: dict[str, Any], source: str, error: Exception) -> None:
    snapshot["collection_errors"].append(
        {
            "source": source,
            "error_type": type(error).__name__,
            "message": str(error),
        }
    )
    print(f"Warning: timeout diagnostic probe {source!r} failed: {error}", file=sys.stderr)


def _probe(snapshot: dict[str, Any], source: str, operation: Callable[[], Any]) -> Any | None:
    try:
        return operation()
    except Exception as error:
        _record_error(snapshot, source, error)
        return None


def _target_status_lines(status: str, dpu_ids: Iterable[str]) -> list[str]:
    """Keep only table rows identifying one of the target DPUs."""
    identifiers = set(dpu_ids)
    matched_lines = []
    for line in status.splitlines():
        # FORMAT_NO_LINESEP renders `| Observed at | DPU machine ID | ... |`.
        # Match that column exactly so an ID mentioned in another DPU's alert,
        # or an ID that merely shares a prefix, cannot enter the snapshot.
        columns = [column.strip() for column in line.strip().strip("|").split("|")]
        if len(columns) > 1 and columns[1] in identifiers:
            matched_lines.append(line.rstrip())
    return matched_lines


def _health_alerts(machine: Mapping[str, Any] | None) -> list[dict[str, Any]]:
    if machine is None:
        return []
    health = machine.get("health")
    if not isinstance(health, Mapping):
        return []
    alerts = health.get("alerts")
    if not isinstance(alerts, list):
        return []
    return [alert for alert in alerts if isinstance(alert, dict)]


def _print_summary(snapshot: Mapping[str, Any], output_path: Path) -> None:
    managed_host = snapshot.get("managed_host")
    dpus = snapshot.get("dpus", {})
    desired = snapshot.get("desired_dpu_network_config", {})

    summary = {
        "stage": snapshot["stage"],
        "timeout": snapshot["timeout"],
        "managed_host": None,
        "dpus": {},
        "reported_dpu_network_status": snapshot.get("reported_dpu_network_status", []),
        "kubernetes_logs": snapshot.get("kubernetes_logs", []),
        "collection_errors": snapshot["collection_errors"],
    }
    if isinstance(managed_host, Mapping):
        summary["managed_host"] = {
            "machine_id": managed_host.get("machine_id"),
            "state": managed_host.get("state"),
            "state_reason": managed_host.get("state_reason"),
            "health_alerts": _health_alerts(managed_host),
        }
    if isinstance(dpus, Mapping):
        for dpu_id, dpu in dpus.items():
            dpu_mapping = dpu if isinstance(dpu, Mapping) else None
            summary["dpus"][dpu_id] = {
                "state": None if dpu_mapping is None else dpu_mapping.get("state"),
                "health_alerts": _health_alerts(dpu_mapping),
                "desired_config_versions": desired.get(dpu_id),
            }

    print("Timeout diagnostic summary:")
    print(json.dumps(summary, indent=2, sort_keys=True))
    print(f"Complete timeout diagnostics written to {output_path}")


def _safe_path_component(value: str) -> str:
    """Make a Kubernetes name safe to use as one local path component."""
    component = re.sub(r"[^A-Za-z0-9._-]", "_", value)
    return "_" if component in {"", ".", ".."} else component


def _write_kubernetes_log(
    snapshot: dict[str, Any],
    event_directory: Path,
    relative_path: Path,
    contents: str,
    source: str,
) -> dict[str, Any] | None:
    if not contents:
        return {"bytes": 0, "empty": True}

    path = event_directory / relative_path
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents, encoding="utf-8")
    except Exception as error:
        _record_error(snapshot, source, error)
        return None
    return {
        "path": relative_path.as_posix(),
        "bytes": len(contents.encode("utf-8")),
    }


def _collect_kubernetes_logs(
    snapshot: dict[str, Any],
    config: KubernetesDiagnosticsConfig,
    event_directory: Path,
    lookback_minutes: int,
) -> None:
    """Collect bounded current and previous logs from configured deployments."""
    for workload in config.workloads:
        workload_result: dict[str, Any] = {
            "namespace": workload.namespace,
            "deployment": workload.deployment,
            "containers": [],
        }
        snapshot["kubernetes_logs"].append(workload_result)
        containers = _probe(
            snapshot,
            f"Kubernetes deployment {workload.namespace}/{workload.deployment}",
            lambda workload=workload: kubectl.list_deployment_pod_containers(
                workload.namespace,
                workload.deployment,
                timeout=DIAGNOSTIC_PROBE_TIMEOUT_SECONDS,
            ),
        )
        if not isinstance(containers, list):
            continue

        for container in containers:
            result: dict[str, Any] = {
                "pod": container.pod,
                "container": container.container,
                "restart_count": container.restart_count,
            }
            workload_result["containers"].append(result)
            relative_directory = Path(
                "kubernetes",
                _safe_path_component(workload.namespace),
                _safe_path_component(workload.deployment),
                _safe_path_component(container.pod),
            )

            for previous in (False, True):
                if previous and container.restart_count <= 0:
                    continue
                stream = "previous" if previous else "current"
                source = (
                    f"Kubernetes {stream} logs "
                    f"{workload.namespace}/{container.pod}/{container.container}"
                )
                logs = _probe(
                    snapshot,
                    source,
                    lambda previous=previous, container=container, workload=workload: (
                        kubectl.get_pod_logs(
                            workload.namespace,
                            container.pod,
                            container.container,
                            lookback_minutes=lookback_minutes,
                            max_bytes=config.max_bytes_per_container,
                            previous=previous,
                            timeout=DIAGNOSTIC_PROBE_TIMEOUT_SECONDS,
                        )
                    ),
                )
                if not isinstance(logs, str):
                    continue
                filename = _safe_path_component(container.container)
                if previous:
                    filename += ".previous"
                log_file = _write_kubernetes_log(
                    snapshot,
                    event_directory,
                    relative_directory / f"{filename}.log",
                    logs,
                    source,
                )
                if log_file is not None:
                    result[stream] = log_file


def _collect_timeout_diagnostics(
    *,
    config: DiagnosticsConfig,
    stage: str,
    host_id: str,
    dpu_ids: Iterable[str],
    site: nico_rest.Site,
    timeout_error: Exception,
    instance_id: str | None = None,
    run_started_at: float | None = None,
) -> Path | None:
    """Collect and persist one snapshot; the public wrapper contains failures."""
    observed_at = datetime.datetime.now(datetime.timezone.utc)
    timestamp = observed_at.strftime("%Y%m%dT%H%M%S%fZ")
    artifact_stem = f"{timestamp}-{stage}-{host_id}"
    output_root = Path(config.output_directory)
    event_directory = output_root / _safe_path_component(artifact_stem)
    snapshot: dict[str, Any] = {
        "schema_version": 1,
        "observed_at": observed_at.isoformat(),
        "stage": stage,
        "timeout": {
            "error_type": type(timeout_error).__name__,
            "message": str(timeout_error),
        },
        "target_host_id": host_id,
        "managed_host": None,
        "machines": {},
        "dpus": {},
        "desired_dpu_network_config": {},
        "reported_dpu_network_status": [],
        "kubernetes_logs": [],
        "cloud": {},
        "collection_errors": [],
    }

    managed_host = _probe(
        snapshot,
        "managed-host show",
        lambda: admin_cli.get_machine_from_mh_show(
            host_id,
            allow_missing=True,
            timeout=DIAGNOSTIC_PROBE_TIMEOUT_SECONDS,
        ),
    )
    snapshot["managed_host"] = _selected_fields(managed_host, _MANAGED_HOST_FIELDS)

    resolved_dpu_ids = list(dict.fromkeys(dpu_ids))
    if isinstance(managed_host, Mapping):
        for dpu in managed_host.get("dpus") or []:
            if not isinstance(dpu, Mapping):
                continue
            dpu_id = dpu.get("machine_id")
            if isinstance(dpu_id, str) and dpu_id not in resolved_dpu_ids:
                resolved_dpu_ids.append(dpu_id)
            if isinstance(dpu_id, str):
                snapshot["dpus"][dpu_id] = _selected_fields(dpu, _DPU_FIELDS)

    machine_ids = [host_id, *resolved_dpu_ids]
    for machine_id in machine_ids:
        machine = _probe(
            snapshot,
            f"machine show {machine_id}",
            lambda machine_id=machine_id: admin_cli.get_machine_from_m_show(
                machine_id,
                allow_missing=True,
                timeout=DIAGNOSTIC_PROBE_TIMEOUT_SECONDS,
            ),
        )
        snapshot["machines"][machine_id] = _selected_fields(machine, _MACHINE_FIELDS)
        if machine_id in resolved_dpu_ids and machine_id not in snapshot["dpus"]:
            snapshot["dpus"][machine_id] = _selected_fields(machine, _DPU_FIELDS)

    for dpu_id in resolved_dpu_ids:
        network_config = _probe(
            snapshot,
            f"dpu network config {dpu_id}",
            lambda dpu_id=dpu_id: admin_cli.get_dpu_network_config(
                dpu_id, timeout=DIAGNOSTIC_PROBE_TIMEOUT_SECONDS
            ),
        )
        snapshot["desired_dpu_network_config"][dpu_id] = _selected_fields(
            network_config, _NETWORK_CONFIG_FIELDS
        )

    network_status = _probe(
        snapshot,
        "dpu network status",
        lambda: admin_cli.get_dpu_network_status_text(timeout=DIAGNOSTIC_PROBE_TIMEOUT_SECONDS),
    )
    if isinstance(network_status, str):
        snapshot["reported_dpu_network_status"] = _target_status_lines(
            network_status, resolved_dpu_ids
        )

    cloud_machine_status = _probe(
        snapshot,
        f"cloud machine {host_id}",
        lambda: nico_rest.get_machine_status(host_id, site, allow_missing_machine=True),
    )
    snapshot["cloud"]["machine_status"] = cloud_machine_status
    if instance_id is not None:
        instance = _probe(
            snapshot,
            f"cloud instance {instance_id}",
            lambda: nico_rest.get_instance_info(instance_id),
        )
        snapshot["cloud"]["instance"] = _selected_fields(instance, _INSTANCE_FIELDS)

    if config.kubernetes.enabled:
        lookback_minutes = config.kubernetes.lookback_minutes
        if lookback_minutes is None:
            elapsed_seconds = (
                0.0 if run_started_at is None else time.monotonic() - run_started_at
            )
            lookback_minutes = max(1, math.ceil(elapsed_seconds / 60))
        _collect_kubernetes_logs(
            snapshot,
            config.kubernetes,
            event_directory,
            lookback_minutes,
        )

    output_path = event_directory / "diagnostics.json"
    try:
        output_path.parent.mkdir(parents=True, exist_ok=True)
        temporary_path = output_path.with_suffix(".json.tmp")
        temporary_path.write_text(
            json.dumps(snapshot, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        temporary_path.replace(output_path)
        _print_summary(snapshot, output_path)
        return output_path
    except Exception as error:
        print(f"Warning: could not write timeout diagnostics: {error}", file=sys.stderr)
        return None


def collect_timeout_diagnostics(
    *,
    config: DiagnosticsConfig,
    stage: str,
    host_id: str,
    dpu_ids: Iterable[str],
    site: nico_rest.Site,
    timeout_error: Exception,
    instance_id: str | None = None,
    run_started_at: float | None = None,
) -> Path | None:
    """Collect and persist a timeout snapshot without masking the failure.

    Each external probe is independent. A missing machine during re-ingestion,
    an unavailable desired DPU configuration, or another diagnostic failure is
    recorded in the artifact while the remaining probes continue. An unexpected
    collector error is also contained so the lifecycle timeout remains primary.
    Unless a fixed lookback is configured, Kubernetes logs cover the elapsed
    time since ``run_started_at``.
    """
    if not config.enabled:
        return None
    try:
        return _collect_timeout_diagnostics(
            config=config,
            stage=stage,
            host_id=host_id,
            dpu_ids=dpu_ids,
            site=site,
            timeout_error=timeout_error,
            instance_id=instance_id,
            run_started_at=run_started_at,
        )
    except Exception as error:
        print(f"Warning: timeout diagnostic collection failed: {error}", file=sys.stderr)
        return None
