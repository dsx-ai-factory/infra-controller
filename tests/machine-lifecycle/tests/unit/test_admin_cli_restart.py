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

"""Unit tests for NICo-managed host restart requests."""

import datetime
import subprocess
from types import SimpleNamespace

import pytest

from lib import admin_cli
from lib.config import ClientCertificateConfig, GrpcApiConfig
from lib.site_vault import ClientCertificate
from tests.lifecycle import machine_lifecycle_test as lifecycle

INGESTION_STARTED_AT = datetime.datetime(2026, 8, 6, 12, 31, tzinfo=datetime.timezone.utc)


def _mock_current_ingestion_restart(monkeypatch):
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_mh_show",
        lambda _machine_id: {
            "host_last_reboot_requested_time_and_mode": (
                "2026-08-06 12:32:00.123456789 UTC/Reboot"
            )
        },
    )


@pytest.mark.parametrize(
    ("skip_factory_reset", "test_sitewide_bmc_fallback", "expected"),
    [
        (False, False, True),
        (True, False, False),
        (False, True, False),
        (True, True, False),
    ],
)
def test_restart_request_expected(
    skip_factory_reset,
    test_sitewide_bmc_fallback,
    expected,
):
    test_config = SimpleNamespace(
        skip_factory_reset=skip_factory_reset,
        test_sitewide_bmc_fallback=test_sitewide_bmc_fallback,
    )

    assert lifecycle._restart_request_expected(test_config) is expected


def test_restart_request_accepts_current_ingestion(monkeypatch):
    _mock_current_ingestion_restart(monkeypatch)
    admin_cli.assert_host_restart_requested("machine-id", INGESTION_STARTED_AT)


def test_restart_request_requires_managed_reboot(monkeypatch):
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_mh_show",
        lambda _machine_id: {"host_last_reboot_requested_time_and_mode": None},
    )

    with pytest.raises(AssertionError, match="did not record a managed Reboot"):
        admin_cli.assert_host_restart_requested("machine-id", INGESTION_STARTED_AT)


def test_restart_request_rejects_non_reboot_mode(monkeypatch):
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_mh_show",
        lambda _machine_id: {
            "host_last_reboot_requested_time_and_mode": (
                "2026-08-06 12:32:00.123456789 UTC/ForceRestart"
            )
        },
    )

    with pytest.raises(AssertionError, match="did not record a managed Reboot"):
        admin_cli.assert_host_restart_requested("machine-id", INGESTION_STARTED_AT)


def test_restart_request_requires_request_from_current_ingestion(monkeypatch):
    monkeypatch.setattr(
        admin_cli,
        "get_machine_from_mh_show",
        lambda _machine_id: {
            "host_last_reboot_requested_time_and_mode": (
                "2026-08-06 12:30:00.123456789 UTC/Reboot"
            )
        },
    )

    with pytest.raises(AssertionError, match="predates this ingestion"):
        admin_cli.assert_host_restart_requested(
            "machine-id",
            datetime.datetime(2026, 8, 6, 12, 31, tzinfo=datetime.timezone.utc),
        )


def _grpc_api_config(with_certificate=True):
    certificate = ClientCertificateConfig(
        vault_pki_mount="pki",
        vault_pki_role="client-role",
        common_name="client",
        ttl="12h",
    )
    return GrpcApiConfig(
        url="https://api.example.test:1079",
        root_ca_path="/var/run/secrets/roots/ca.crt",
        client_certificate=certificate if with_certificate else None,
    )


class _StagingRecorder:
    """Stands in for the kubectl calls that stage and remove credentials."""

    def __init__(self):
        self.written = {}
        self.removed = []

    def install(self, monkeypatch, pod="api-pod-1"):
        monkeypatch.setattr(
            admin_cli.kubectl, "get_deployment_pod", lambda _namespace, _deployment: pod
        )
        monkeypatch.setattr(
            admin_cli.kubectl,
            "write_pod_file",
            lambda _namespace, _pod, path, content: self.written.update({path: content}),
        )
        monkeypatch.setattr(
            admin_cli.kubectl,
            "remove_pod_path",
            lambda _namespace, _pod, path: self.removed.append(path),
        )
        return self


def _mock_minted_certificate(monkeypatch):
    class FakeSiteVaultClient:
        def __enter__(self):
            return self

        def __exit__(self, *_args):
            return False

        def issue_client_certificate(self, **kwargs):
            issue_requests.append(kwargs)
            return ClientCertificate(
                certificate="-----BEGIN CERTIFICATE-----",
                private_key="-----BEGIN EC PRIVATE KEY-----",
            )

    issue_requests = []
    monkeypatch.setattr(admin_cli, "SiteVaultClient", FakeSiteVaultClient)
    return issue_requests


EXEC_ONLY_PREFIX = [
    "kubectl",
    "exec",
    "-n",
    "forge-system",
    "deployment/nico-api",
    "--",
    "/opt/nico/nico-admin-cli",
]


def test_command_prefix_without_client_identity_is_unchanged():
    """The invocation used where the API accepts an unauthenticated caller."""
    assert admin_cli._admin_cli_command_prefix() == EXEC_ONLY_PREFIX


def test_client_identity_without_configuration_changes_nothing(monkeypatch):
    recorder = _StagingRecorder().install(monkeypatch)

    with admin_cli.client_identity(None):
        assert admin_cli._admin_cli_command_prefix() == EXEC_ONLY_PREFIX

    assert recorder.written == {}
    assert recorder.removed == []


def test_api_address_applies_without_a_client_certificate(monkeypatch):
    """An explicit address is needed even where no identity is presented."""
    recorder = _StagingRecorder().install(monkeypatch)

    with admin_cli.client_identity(_grpc_api_config(with_certificate=False)):
        prefix = admin_cli._admin_cli_command_prefix()

    assert prefix == EXEC_ONLY_PREFIX + [
        "--api-url",
        "https://api.example.test:1079",
        "--root-ca-path",
        "/var/run/secrets/roots/ca.crt",
    ]
    # Nothing is minted or staged, so no pod is singled out and none is owed cleanup.
    assert recorder.written == {}
    assert recorder.removed == []
    assert admin_cli._admin_cli_command_prefix() == EXEC_ONLY_PREFIX


def test_client_identity_stages_a_certificate_and_selects_it(monkeypatch):
    recorder = _StagingRecorder().install(monkeypatch)
    issue_requests = _mock_minted_certificate(monkeypatch)

    with admin_cli.client_identity(_grpc_api_config()):
        prefix = admin_cli._admin_cli_command_prefix()

    assert issue_requests == [
        {
            "pki_mount": "pki",
            "pki_role": "client-role",
            "common_name": "client",
            "ttl": "12h",
        }
    ]

    # Addressed directly: a deployment reference may resolve to another replica.
    assert prefix[:6] == [
        "kubectl",
        "exec",
        "-n",
        "forge-system",
        "api-pod-1",
        "--",
    ]

    flags = dict(zip(prefix[7::2], prefix[8::2]))
    assert flags["--api-url"] == "https://api.example.test:1079"
    assert flags["--root-ca-path"] == "/var/run/secrets/roots/ca.crt"

    cert_path = flags["--client-cert-path"]
    key_path = flags["--client-key-path"]
    assert set(recorder.written) == {cert_path, key_path}
    # The private key must stay on tmpfs; the container's /tmp is disk-backed.
    for path in (cert_path, key_path):
        assert path.startswith("/dev/shm/")
    assert recorder.written[key_path] == "-----BEGIN EC PRIVATE KEY-----"


def test_client_identity_is_removed_after_the_run(monkeypatch):
    recorder = _StagingRecorder().install(monkeypatch)
    _mock_minted_certificate(monkeypatch)

    with admin_cli.client_identity(_grpc_api_config()):
        staged_directory = admin_cli._grpc_api_target.certificate.cert_path.rsplit("/", 1)[0]

    assert recorder.removed == [staged_directory]
    assert admin_cli._grpc_api_target is None
    assert admin_cli._admin_cli_command_prefix() == EXEC_ONLY_PREFIX


def test_client_identity_is_removed_after_a_failure(monkeypatch):
    recorder = _StagingRecorder().install(monkeypatch)
    _mock_minted_certificate(monkeypatch)

    with pytest.raises(RuntimeError, match="lifecycle stage failed"):
        with admin_cli.client_identity(_grpc_api_config()):
            raise RuntimeError("lifecycle stage failed")

    assert len(recorder.removed) == 1
    assert admin_cli._grpc_api_target is None


def test_cleanup_failure_does_not_mask_the_run_failure(monkeypatch):
    _StagingRecorder().install(monkeypatch)
    _mock_minted_certificate(monkeypatch)

    def failing_remove(_namespace, _pod, _path):
        raise RuntimeError("kubectl exec failed")

    monkeypatch.setattr(admin_cli.kubectl, "remove_pod_path", failing_remove)

    with pytest.raises(RuntimeError, match="lifecycle stage failed"):
        with admin_cli.client_identity(_grpc_api_config()):
            raise RuntimeError("lifecycle stage failed")

    assert admin_cli._grpc_api_target is None


def test_run_admin_cli_times_out_rather_than_hanging(monkeypatch):
    """An address the server certificate does not cover makes the CLI retry."""

    def hang(command, **kwargs):
        raise subprocess.TimeoutExpired(command, kwargs["timeout"])

    monkeypatch.setattr(admin_cli.subprocess, "run", hang)
    monkeypatch.setattr(admin_cli, "ADMIN_CLI_TIMEOUT_SECONDS", 7)

    with pytest.raises(subprocess.TimeoutExpired) as raised:
        admin_cli.run_admin_cli(["managed-host", "show"])

    assert raised.value.timeout == 7


def test_run_admin_cli_timeout_does_not_leak_a_password(monkeypatch):
    def hang(command, **kwargs):
        raise subprocess.TimeoutExpired(command, kwargs["timeout"])

    monkeypatch.setattr(admin_cli.subprocess, "run", hang)

    with pytest.raises(subprocess.TimeoutExpired) as raised:
        admin_cli.run_admin_cli(["redfish", "power-off", "--password", "hunter2"])

    assert "hunter2" not in str(raised.value.cmd)
    assert "***" in raised.value.cmd
