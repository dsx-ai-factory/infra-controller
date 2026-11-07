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

"""Unit tests for recovering the staged admin-cli identity after a rollout."""

import subprocess

import pytest

from lib import admin_cli
from lib.config import ClientCertificateConfig, GrpcApiConfig
from lib.site_vault import ClientCertificate


def _target(pod="api-old", directory="/dev/shm/mlt-aaaa"):
    return admin_cli._GrpcApiTarget(
        api_url="https://api:1079",
        root_ca_path="/ca.crt",
        certificate=admin_cli._StagedCertificate(
            pod=pod,
            cert_path=f"{directory}/client.crt",
            key_path=f"{directory}/client.key",
        ),
    )


@pytest.fixture
def staged(monkeypatch):
    """A run holding a staged identity, as `client_identity` would leave it."""
    monkeypatch.setattr(admin_cli, "_grpc_api_target", _target())
    monkeypatch.setattr(admin_cli, "_staged_identity", ("CERT", "KEY"))
    monkeypatch.setattr(admin_cli, "_staged_api_build", "v1")


@pytest.mark.parametrize(
    "stderr",
    [
        'Error from server (NotFound): pods "nico-api-7cbdcc9bdc-lrlkt" not found',
        "unable to upgrade connection: container not found",
        "/dev/shm/mlt-0924a88f: No such file or directory",
    ],
)
def test_recognises_a_lost_pod(stderr):
    assert admin_cli._lost_staged_identity(stderr) is True


@pytest.mark.parametrize(
    "stderr",
    [
        "",
        "machine fm100 not found",
        "Error: the object is in the state for longer than defined by the SLA",
    ],
)
def test_does_not_mistake_the_cli_s_own_not_found(stderr):
    # The admin-cli reports its own missing-object conditions; treating those as
    # a lost pod would re-stage on every absent machine lookup.
    assert admin_cli._lost_staged_identity(stderr) is False


def test_restages_into_the_replacement_pod(staged, monkeypatch, capsys):
    written = []
    monkeypatch.setattr(admin_cli.kubectl, "get_deployment_pod", lambda *_: "api-new")
    monkeypatch.setattr(
        admin_cli.kubectl,
        "write_pod_file",
        lambda _ns, pod, path, content: written.append((pod, path, content)),
    )
    monkeypatch.setattr(admin_cli, "_read_api_build_version", lambda: "v1")

    assert admin_cli._restage_client_identity() is True

    assert admin_cli._grpc_api_target.certificate.pod == "api-new"
    # The material is re-written, not just the paths re-pointed.
    assert {c for _pod, _path, c in written} == {"CERT", "KEY"}
    assert all(pod == "api-new" for pod, _path, _c in written)
    # A fresh directory, so a stale one cannot be reused.
    assert "/dev/shm/mlt-aaaa" not in admin_cli._grpc_api_target.certificate.cert_path


def test_reports_a_build_change_across_the_restage(staged, monkeypatch, capsys):
    monkeypatch.setattr(admin_cli.kubectl, "get_deployment_pod", lambda *_: "api-new")
    monkeypatch.setattr(admin_cli.kubectl, "write_pod_file", lambda *_a: None)
    monkeypatch.setattr(admin_cli, "_read_api_build_version", lambda: "v2")

    assert admin_cli._restage_client_identity() is True

    assert "v1 -> v2" in capsys.readouterr().err
    assert admin_cli._staged_api_build == "v2"


def test_retries_until_a_pod_is_ready(staged, monkeypatch):
    attempts = []

    def flaky(*_args):
        attempts.append(1)
        if len(attempts) < 3:
            raise RuntimeError("no ready replica")
        return "api-new"

    monkeypatch.setattr(admin_cli.kubectl, "get_deployment_pod", flaky)
    monkeypatch.setattr(admin_cli.kubectl, "write_pod_file", lambda *_a: None)
    monkeypatch.setattr(admin_cli, "_read_api_build_version", lambda: None)
    monkeypatch.setattr(admin_cli.time, "sleep", lambda _s: None)

    assert admin_cli._restage_client_identity(attempts=3, delay=0) is True
    assert len(attempts) == 3


def test_gives_up_when_no_pod_becomes_ready(staged, monkeypatch):
    monkeypatch.setattr(
        admin_cli.kubectl,
        "get_deployment_pod",
        lambda *_: (_ for _ in ()).throw(RuntimeError("no ready replica")),
    )
    monkeypatch.setattr(admin_cli.time, "sleep", lambda _s: None)

    assert admin_cli._restage_client_identity(attempts=2, delay=0) is False


def test_nothing_to_restage_without_retained_material(monkeypatch):
    monkeypatch.setattr(admin_cli, "_grpc_api_target", _target())
    monkeypatch.setattr(admin_cli, "_staged_identity", None)

    assert admin_cli._restage_client_identity() is False


def test_a_lost_pod_is_retried_once_after_restaging(staged, monkeypatch):
    calls = []

    def invoke(args, *, json_output, timeout):
        calls.append(admin_cli._grpc_api_target.certificate.pod)
        if len(calls) == 1:
            raise subprocess.CalledProcessError(
                1, ["kubectl"], stderr='Error from server (NotFound): pods "x" not found'
            )
        return subprocess.CompletedProcess(["kubectl"], 0, stdout="{}", stderr="")

    monkeypatch.setattr(admin_cli, "_invoke_admin_cli", invoke)
    monkeypatch.setattr(admin_cli.kubectl, "get_deployment_pod", lambda *_: "api-new")
    monkeypatch.setattr(admin_cli.kubectl, "write_pod_file", lambda *_a: None)
    monkeypatch.setattr(admin_cli, "_read_api_build_version", lambda: "v1")

    admin_cli._run_admin_cli_process(["machine", "show"], json_output=True, timeout=None)

    # Retried against the replacement, not the pod that had gone.
    assert calls == ["api-old", "api-new"]


def test_an_ordinary_failure_is_not_retried(staged, monkeypatch):
    calls = []

    def invoke(args, *, json_output, timeout):
        calls.append(1)
        raise subprocess.CalledProcessError(1, ["kubectl"], stderr="machine not found")

    monkeypatch.setattr(admin_cli, "_invoke_admin_cli", invoke)
    monkeypatch.setattr(
        admin_cli,
        "_restage_client_identity",
        lambda *_a, **_k: pytest.fail("must not re-stage on an ordinary failure"),
    )

    with pytest.raises(subprocess.CalledProcessError):
        admin_cli._run_admin_cli_process(
            ["machine", "show"], json_output=True, timeout=None
        )
    assert len(calls) == 1


def test_a_partly_written_attempt_is_removed_before_retrying(staged, monkeypatch):
    """The key is written first, so a failed cert write leaves one behind."""
    removed = []
    pods = iter(["api-half", "api-new"])
    monkeypatch.setattr(admin_cli.kubectl, "get_deployment_pod", lambda *_: next(pods))
    monkeypatch.setattr(
        admin_cli.kubectl,
        "remove_pod_path",
        lambda _ns, pod, path: removed.append((pod, path)),
    )
    monkeypatch.setattr(admin_cli, "_read_api_build_version", lambda: None)
    monkeypatch.setattr(admin_cli.time, "sleep", lambda _s: None)
    admin_cli._staged_locations.clear()

    writes = []

    def write(_ns, pod, path, content):
        writes.append(path)
        if pod == "api-half" and path.endswith("client.crt"):
            raise RuntimeError("connection reset")

    monkeypatch.setattr(admin_cli.kubectl, "write_pod_file", write)

    assert admin_cli._restage_client_identity(attempts=2, delay=0) is True

    # The abandoned attempt was cleaned up, and is no longer owed a final sweep.
    assert [pod for pod, _path in removed] == ["api-half"]
    assert [pod for pod, _path in admin_cli._staged_locations] == ["api-new"]


def test_an_attempt_that_could_not_be_cleaned_is_kept_for_the_final_sweep(
    staged, monkeypatch
):
    monkeypatch.setattr(admin_cli.kubectl, "get_deployment_pod", lambda *_: "api-gone")
    monkeypatch.setattr(
        admin_cli.kubectl,
        "write_pod_file",
        lambda *_a: (_ for _ in ()).throw(RuntimeError("pod vanished")),
    )
    monkeypatch.setattr(
        admin_cli.kubectl,
        "remove_pod_path",
        lambda *_a: (_ for _ in ()).throw(RuntimeError("pod vanished")),
    )
    monkeypatch.setattr(admin_cli.time, "sleep", lambda _s: None)
    admin_cli._staged_locations.clear()

    assert admin_cli._restage_client_identity(attempts=1, delay=0) is False
    assert [pod for pod, _path in admin_cli._staged_locations] == ["api-gone"]


def _grpc_api_config():
    return GrpcApiConfig(
        url="https://api.example.test:1079",
        root_ca_path="/var/run/secrets/roots/ca.crt",
        client_certificate=ClientCertificateConfig(
            vault_pki_mount="pki",
            vault_pki_role="client-role",
            common_name="client",
            ttl="12h",
        ),
    )


def _mock_vault(monkeypatch):
    class FakeSiteVaultClient:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def issue_client_certificate(self, **_kwargs):
            return ClientCertificate(certificate="CERT", private_key="KEY")

    monkeypatch.setattr(admin_cli, "SiteVaultClient", FakeSiteVaultClient)


def test_every_pod_staged_into_is_cleaned_up_at_the_end(monkeypatch):
    """Two rollouts in one run must not leave the middle pod holding a key."""
    _mock_vault(monkeypatch)
    removed = []
    pods = iter(["api-1", "api-2", "api-3"])
    monkeypatch.setattr(admin_cli.kubectl, "get_deployment_pod", lambda *_: next(pods))
    monkeypatch.setattr(admin_cli.kubectl, "write_pod_file", lambda *_a: None)
    monkeypatch.setattr(
        admin_cli.kubectl,
        "remove_pod_path",
        lambda _ns, pod, path: removed.append(pod),
    )
    monkeypatch.setattr(admin_cli, "_read_api_build_version", lambda: "v1")

    with admin_cli.client_identity(_grpc_api_config()):
        assert admin_cli._restage_client_identity() is True
        assert admin_cli._restage_client_identity() is True

    assert removed == ["api-1", "api-2", "api-3"]
    assert admin_cli._staged_locations == []
