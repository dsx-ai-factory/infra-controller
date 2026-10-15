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

"""Unit tests for Kubernetes command construction and pod selection."""

import json
import subprocess

import pytest

from lib import kubectl


def test_deployment_pod_selector_uses_sorted_match_labels(monkeypatch):
    commands = []

    def fake_run(command, **_kwargs):
        commands.append(command)
        return subprocess.CompletedProcess(
            command,
            0,
            stdout=json.dumps(
                {
                    "spec": {
                        "selector": {
                            "matchLabels": {
                                "component": "api",
                                "app": "nico",
                            }
                        }
                    }
                }
            ),
            stderr="",
        )

    monkeypatch.setattr(kubectl.subprocess, "run", fake_run)

    assert (
        kubectl._deployment_pod_selector("test-namespace", "test-api")
        == "app=nico,component=api"
    )
    assert commands == [
        [
            "kubectl",
            "get",
            "deployment",
            "test-api",
            "-n",
            "test-namespace",
            "-o",
            "json",
        ]
    ]


def test_deployment_pod_selector_rejects_missing_match_labels(monkeypatch):
    monkeypatch.setattr(
        kubectl.subprocess,
        "run",
        lambda command, **_kwargs: subprocess.CompletedProcess(
            command,
            0,
            stdout='{"spec":{"selector":{}}}',
            stderr="",
        ),
    )

    with pytest.raises(RuntimeError, match="does not have a usable matchLabels"):
        kubectl._deployment_pod_selector("test-namespace", "test-api")


def test_lists_all_deployment_pod_containers_and_restart_counts(monkeypatch):
    responses = iter(
        [
            {"spec": {"selector": {"matchLabels": {"app": "nico"}}}},
            {
                "items": [
                    {
                        "metadata": {"name": "api-b"},
                        "spec": {"containers": [{"name": "sidecar"}, {"name": "api"}]},
                        "status": {
                            "containerStatuses": [
                                {"name": "api", "restartCount": 2},
                                {"name": "sidecar", "restartCount": 0},
                            ]
                        },
                    },
                    {
                        "metadata": {"name": "api-a"},
                        "spec": {"containers": [{"name": "api"}]},
                        "status": {"containerStatuses": []},
                    },
                ]
            },
        ]
    )
    timeouts = []

    def fake_run(command, **kwargs):
        timeouts.append(kwargs["timeout"])
        return subprocess.CompletedProcess(
            command, 0, stdout=json.dumps(next(responses)), stderr=""
        )

    monkeypatch.setattr(kubectl.subprocess, "run", fake_run)

    assert kubectl.list_deployment_pod_containers("test-namespace", "test-api", timeout=30) == [
        kubectl.PodContainer(pod="api-a", container="api", restart_count=0),
        kubectl.PodContainer(pod="api-b", container="api", restart_count=2),
        kubectl.PodContainer(pod="api-b", container="sidecar", restart_count=0),
    ]
    assert timeouts == [30, 30]


def test_deployment_with_no_pods_is_an_error(monkeypatch):
    monkeypatch.setattr(kubectl, "_deployment_pods", lambda *_args, **_kwargs: [])

    with pytest.raises(RuntimeError, match="test-namespace/test-api has no pods"):
        kubectl.list_deployment_pod_containers(
            "test-namespace",
            "test-api",
            timeout=30,
        )


@pytest.mark.parametrize("previous", [False, True])
def test_get_pod_logs_is_bounded_and_timestamped(monkeypatch, previous):
    recorded = {}

    def fake_run(command, **kwargs):
        recorded["command"] = command
        recorded["timeout"] = kwargs["timeout"]
        return subprocess.CompletedProcess(command, 0, stdout="log output\n", stderr="")

    monkeypatch.setattr(kubectl.subprocess, "run", fake_run)

    assert (
        kubectl.get_pod_logs(
            "test-namespace",
            "api-pod",
            "api",
            lookback_minutes=20,
            max_bytes=1234,
            previous=previous,
            timeout=30,
        )
        == "log output\n"
    )
    assert "--timestamps=true" in recorded["command"]
    assert "--since=20m" in recorded["command"]
    assert "--limit-bytes=1234" in recorded["command"]
    assert ("--previous=true" in recorded["command"]) is previous
    assert recorded["timeout"] == 30


def _write_pod_file_run(monkeypatch, stdout, returncode=0, stderr=""):
    """Capture the invocation and return a canned result."""
    calls = {}

    def fake_run(command, **kwargs):
        calls["command"] = command
        calls["input"] = kwargs.get("input")
        calls["encoding"] = kwargs.get("encoding")
        calls["timeout"] = kwargs.get("timeout")
        return subprocess.CompletedProcess(command, returncode, stdout=stdout, stderr=stderr)

    monkeypatch.setattr(kubectl.subprocess, "run", fake_run)
    return calls


def test_write_pod_file_sends_content_over_stdin_and_checks_the_size(monkeypatch):
    calls = _write_pod_file_run(monkeypatch, stdout="5\n")

    kubectl.write_pod_file("ns", "pod-1", "/dev/shm/mlt-a/client.key", "hello")

    assert calls["input"] == "hello"
    # The size is reported by the remote script, not inferred locally.
    assert calls["command"][-1] == (
        "umask 077 && mkdir -p /dev/shm/mlt-a && cat > /dev/shm/mlt-a/client.key "
        "&& wc -c < /dev/shm/mlt-a/client.key"
    )
    assert calls["command"][:8] == [
        "kubectl", "exec", "-i", "-n", "ns", "pod-1", "--", "sh",
    ]
    assert calls["timeout"] == kubectl.KUBECTL_TIMEOUT_SECONDS


def test_remove_pod_path_is_bounded(monkeypatch):
    recorded = {}

    def fake_run(command, **kwargs):
        recorded["command"] = command
        recorded["timeout"] = kwargs.get("timeout")
        return subprocess.CompletedProcess(command, 0, stdout="", stderr="")

    monkeypatch.setattr(kubectl.subprocess, "run", fake_run)

    kubectl.remove_pod_path("ns", "pod-1", "/dev/shm/mlt-a")

    assert recorded["command"][-4:] == ["rm", "-rf", "--", "/dev/shm/mlt-a"]
    assert recorded["timeout"] == kubectl.KUBECTL_TIMEOUT_SECONDS


def test_get_deployment_pod_bounds_both_kubectl_calls(monkeypatch):
    responses = iter(
        [
            {"spec": {"selector": {"matchLabels": {"app": "nico"}}}},
            {
                "items": [
                    {
                        "metadata": {"name": "api-a"},
                        "status": {
                            "phase": "Running",
                            "conditions": [{"type": "Ready", "status": "True"}],
                        },
                    }
                ]
            },
        ]
    )
    timeouts = []

    def fake_run(command, **kwargs):
        timeouts.append(kwargs.get("timeout"))
        return subprocess.CompletedProcess(
            command, 0, stdout=json.dumps(next(responses)), stderr=""
        )

    monkeypatch.setattr(kubectl.subprocess, "run", fake_run)

    assert kubectl.get_deployment_pod("ns", "api") == "api-a"
    assert timeouts == [kubectl.KUBECTL_TIMEOUT_SECONDS] * 2


def test_write_pod_file_rejects_a_short_write(monkeypatch):
    """`kubectl exec -i` can exit 0 having delivered no stdin at all."""
    _write_pod_file_run(monkeypatch, stdout="0\n")

    with pytest.raises(RuntimeError, match="Wrote 0 of 5 bytes"):
        kubectl.write_pod_file("ns", "pod-1", "/dev/shm/mlt-a/client.key", "hello")


def test_write_pod_file_measures_bytes_not_characters(monkeypatch):
    calls = _write_pod_file_run(monkeypatch, stdout="3\n")

    # One character, three UTF-8 bytes: comparing against len() would reject it.
    kubectl.write_pod_file("ns", "pod-1", "/f", "€")

    assert calls["input"] == "€"
    # Counted as UTF-8, so stdin must be encoded as UTF-8 too rather than
    # however the locale happens to be set.
    assert calls["encoding"] == "utf-8"


def test_write_pod_file_rejects_an_unparseable_size(monkeypatch):
    _write_pod_file_run(monkeypatch, stdout="")

    with pytest.raises(RuntimeError, match="expected a byte count"):
        kubectl.write_pod_file("ns", "pod-1", "/f", "hello")


def test_write_pod_file_still_reports_a_failed_command(monkeypatch):
    _write_pod_file_run(monkeypatch, stdout="", returncode=1, stderr="pods not found")

    with pytest.raises(RuntimeError, match="Could not write /f in ns/pod-1: pods not found"):
        kubectl.write_pod_file("ns", "pod-1", "/f", "hello")
