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

"""Helpers for querying Kubernetes with kubectl."""

import json
import shlex
import subprocess
from dataclasses import dataclass
from pathlib import PurePosixPath

KUBECTL_TIMEOUT_SECONDS = 120


@dataclass(frozen=True)
class PodContainer:
    """One container belonging to a pod selected by a deployment."""

    pod: str
    container: str
    restart_count: int


def _deployment_pod_selector(namespace: str, deployment: str, timeout: int | None = None) -> str:
    """Return the label selector used by a Kubernetes deployment's pods."""
    command = [
        "kubectl",
        "get",
        "deployment",
        deployment,
        "-n",
        namespace,
        "-o",
        "json",
    ]
    print(f"Executing {command}")
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise subprocess.CalledProcessError(
            result.returncode,
            command,
            output=result.stdout,
            stderr=result.stderr,
        )

    try:
        labels = json.loads(result.stdout)["spec"]["selector"]["matchLabels"]
        if not isinstance(labels, dict) or not labels:
            raise ValueError
        return ",".join(f"{key}={labels[key]}" for key in sorted(labels))
    except (json.JSONDecodeError, KeyError, TypeError, ValueError):
        raise RuntimeError(
            f"Deployment {namespace}/{deployment} "
            "does not have a usable matchLabels pod selector"
        ) from None


def _deployment_pods(namespace: str, deployment: str, timeout: int | None = None) -> list[dict]:
    """Return pod objects selected by a deployment."""
    pod_selector = _deployment_pod_selector(namespace, deployment, timeout=timeout)
    command = [
        "kubectl",
        "get",
        "pods",
        "-n",
        namespace,
        "--selector",
        pod_selector,
        "-o",
        "json",
    ]
    print(f"Executing {command}")
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        raise subprocess.CalledProcessError(
            result.returncode,
            command,
            output=result.stdout,
            stderr=result.stderr,
        )

    try:
        items = json.loads(result.stdout)["items"]
        if not isinstance(items, list):
            raise TypeError
    except (json.JSONDecodeError, KeyError, TypeError):
        raise RuntimeError(
            f"Could not list pods of deployment {namespace}/{deployment}"
        ) from None
    return [item for item in items if isinstance(item, dict)]


def get_deployment_pod(
    namespace: str, deployment: str, timeout: int = KUBECTL_TIMEOUT_SECONDS
) -> str:
    """Return the name of one ready pod belonging to a deployment.

    Callers that write into a pod and later execute in it must address the same
    pod both times, which ``deployment/<name>`` does not guarantee.
    """
    items = _deployment_pods(namespace, deployment, timeout=timeout)

    ready_pods = sorted(
        name
        for name in (_ready_pod_name(item) for item in items)
        if name is not None
    )
    if not ready_pods:
        raise RuntimeError(f"Deployment {namespace}/{deployment} has no ready pods")
    return ready_pods[0]


def list_deployment_pod_containers(
    namespace: str, deployment: str, timeout: int | None = None
) -> list[PodContainer]:
    """List every regular container in pods selected by a deployment."""
    pods = _deployment_pods(namespace, deployment, timeout=timeout)
    if not pods:
        raise RuntimeError(f"Deployment {namespace}/{deployment} has no pods")

    containers = []
    for pod in pods:
        metadata = pod.get("metadata") or {}
        spec = pod.get("spec") or {}
        status = pod.get("status") or {}
        if not all(isinstance(value, dict) for value in (metadata, spec, status)):
            continue
        pod_name = metadata.get("name")
        configured_containers = spec.get("containers") or []
        if not isinstance(pod_name, str) or not isinstance(configured_containers, list):
            continue
        statuses = status.get("containerStatuses") or []
        restart_counts = {
            item.get("name"): item.get("restartCount", 0)
            for item in statuses
            if isinstance(item, dict) and isinstance(item.get("name"), str)
        }
        for container in configured_containers:
            if not isinstance(container, dict):
                continue
            container_name = container.get("name")
            if not isinstance(container_name, str) or not container_name:
                continue
            restart_count = restart_counts.get(container_name, 0)
            if isinstance(restart_count, bool) or not isinstance(restart_count, int):
                restart_count = 0
            containers.append(
                PodContainer(
                    pod=pod_name,
                    container=container_name,
                    restart_count=restart_count,
                )
            )
    return sorted(containers, key=lambda item: (item.pod, item.container))


def get_pod_logs(
    namespace: str,
    pod: str,
    container: str,
    *,
    lookback_minutes: int,
    max_bytes: int,
    previous: bool = False,
    timeout: int | None = None,
) -> str:
    """Return bounded, timestamped logs for one pod container."""
    command = [
        "kubectl",
        "logs",
        "-n",
        namespace,
        pod,
        "--container",
        container,
        "--timestamps=true",
        f"--since={lookback_minutes}m",
        f"--limit-bytes={max_bytes}",
    ]
    if previous:
        command.append("--previous=true")
    print(f"Executing {command}")
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        detail = result.stderr.strip() or "no detail"
        stream = "previous logs" if previous else "logs"
        raise RuntimeError(f"Could not read {stream} for {namespace}/{pod}/{container}: {detail}")
    return result.stdout


def _ready_pod_name(pod: object) -> str | None:
    """Return a pod's name when it is running, ready and not terminating."""
    if not isinstance(pod, dict):
        return None
    metadata = pod.get("metadata") or {}
    status = pod.get("status") or {}
    if not isinstance(metadata, dict) or not isinstance(status, dict):
        return None
    if metadata.get("deletionTimestamp") or status.get("phase") != "Running":
        return None

    conditions = status.get("conditions") or []
    if not isinstance(conditions, list):
        return None
    ready = any(
        isinstance(condition, dict)
        and condition.get("type") == "Ready"
        and condition.get("status") == "True"
        for condition in conditions
    )
    name = metadata.get("name")
    return name if ready and isinstance(name, str) and name else None


def write_pod_file(
    namespace: str, pod: str, path: str, content: str, timeout: int = KUBECTL_TIMEOUT_SECONDS
) -> None:
    """Write a file inside a pod, readable only by its owner.

    The content goes over stdin, so it never enters the pod's process table.

    The script reports the size it wrote and that is checked, because `kubectl
    exec -i` does not guarantee stdin is delivered: a remote command that
    produces no output can finish before it arrives, leaving an empty file and
    an exit status of zero. Observed writing 0 of 10 bytes, repeatably, with
    the bare redirect this used to use. Reporting the size is also what fixes
    it -- the stream now stays open long enough for stdin to land -- but it is
    checked rather than assumed, since a silently empty credential fails much
    later and looks like a certificate problem.
    """
    quoted_path = shlex.quote(path)
    directory = shlex.quote(str(PurePosixPath(path).parent))
    script = (
        f"umask 077 && mkdir -p {directory} && cat > {quoted_path} "
        f"&& wc -c < {quoted_path}"
    )
    command = ["kubectl", "exec", "-i", "-n", namespace, pod, "--", "sh", "-c", script]

    print(f"Executing {command}")
    # Encoding pinned on both sides. Left to the locale, stdin would be encoded
    # one way and counted another, failing a correct write of non-ASCII content.
    result = subprocess.run(
        command,
        input=content,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=timeout,
    )
    if result.returncode:
        detail = result.stderr.strip() or "no detail"
        raise RuntimeError(f"Could not write {path} in {namespace}/{pod}: {detail}")

    expected = len(content.encode("utf-8"))
    reported = result.stdout.strip()
    try:
        written = int(reported)
    except ValueError:
        raise RuntimeError(
            f"Could not confirm the size of {path} in {namespace}/{pod}: "
            f"expected a byte count, got {reported!r}"
        ) from None
    if written != expected:
        raise RuntimeError(
            f"Wrote {written} of {expected} bytes to {path} in {namespace}/{pod}; "
            "the content did not arrive intact"
        )


def remove_pod_path(
    namespace: str, pod: str, path: str, timeout: int = KUBECTL_TIMEOUT_SECONDS
) -> None:
    """Remove a path inside a pod, recursively and without failing if absent."""
    command = [
        "kubectl",
        "exec",
        "-n",
        namespace,
        pod,
        "--",
        "rm",
        "-rf",
        "--",
        path,
    ]
    print(f"Executing {command}")
    result = subprocess.run(command, capture_output=True, text=True, timeout=timeout)
    if result.returncode:
        detail = result.stderr.strip() or "no detail"
        raise RuntimeError(f"Could not remove {path} in {namespace}/{pod}: {detail}")
