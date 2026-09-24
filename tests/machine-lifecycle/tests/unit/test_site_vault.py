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

import base64
import json
import os
import subprocess
from types import SimpleNamespace
from unittest.mock import MagicMock

import pytest
from hvac import exceptions as hvac_exceptions

from lib.site_vault import (
    BmcCredentials,
    SiteVaultClient,
    SiteVaultCredentialNotFound,
    SiteVaultError,
    normalize_bmc_mac,
    service_account_role_from_jwt,
    sitewide_credential_path,
)
from tests.lifecycle import machine_lifecycle_test as lifecycle


def _service_account_jwt(name: str = "mlt-runner") -> str:
    payload = {
        "kubernetes.io": {
            "namespace": "mlt-tests",
            "serviceaccount": {"name": name, "uid": "test-uid"},
        }
    }
    encoded = base64.urlsafe_b64encode(json.dumps(payload).encode()).decode().rstrip("=")
    return f"header.{encoded}.signature"


@pytest.fixture
def authenticated_client(tmp_path, monkeypatch):
    ca_path = tmp_path / "ca.crt"
    ca_path.write_text("test CA", encoding="utf-8")
    token_path = tmp_path / "token"
    token_path.write_text(_service_account_jwt(), encoding="utf-8")

    hvac_client = MagicMock()
    client_factory = MagicMock(return_value=hvac_client)
    monkeypatch.setattr("lib.site_vault.hvac.Client", client_factory)

    client = SiteVaultClient(
        ca_cert_path=str(ca_path),
        service_account_token_path=str(token_path),
    )
    client.authenticate()
    return client, hvac_client, client_factory


def test_normalize_bmc_mac():
    assert normalize_bmc_mac("02:aa:bb:cc:dd:02") == "02:AA:BB:CC:DD:02"


@pytest.mark.parametrize("value", ["", "02-00-00-00-00-02", "02:00:00:00:00", "not-a-mac"])
def test_normalize_bmc_mac_rejects_invalid_values(value):
    with pytest.raises(SiteVaultError, match="XX:XX"):
        normalize_bmc_mac(value)


def test_service_account_role_from_jwt():
    assert service_account_role_from_jwt(_service_account_jwt()) == "mlt-runner"


@pytest.mark.parametrize("jwt", ["", "invalid", "header.invalid.signature"])
def test_service_account_role_from_jwt_rejects_malformed_tokens(jwt):
    with pytest.raises(SiteVaultError, match="service-account name"):
        service_account_role_from_jwt(jwt)


def test_authenticate_uses_in_cluster_defaults(authenticated_client):
    client, hvac_client, client_factory = authenticated_client

    client_factory.assert_called_once_with(
        url="https://vault.vault.svc.cluster.local:8200",
        verify=client.ca_cert_path,
    )
    hvac_client.auth.kubernetes.login.assert_called_once_with(
        role="mlt-runner",
        jwt=_service_account_jwt(),
        mount_point="kubernetes",
    )


def test_explicit_role_and_mount_overrides(tmp_path, monkeypatch):
    ca_path = tmp_path / "ca.crt"
    ca_path.write_text("test CA", encoding="utf-8")
    token_path = tmp_path / "token"
    token_path.write_text(_service_account_jwt(), encoding="utf-8")
    hvac_client = MagicMock()
    monkeypatch.setattr("lib.site_vault.hvac.Client", MagicMock(return_value=hvac_client))

    client = SiteVaultClient(
        address="https://vault.example.test:8200",
        ca_cert_path=str(ca_path),
        auth_mount="site-kubernetes",
        kv_mount="site-secrets",
        role="mlt-role",
        service_account_token_path=str(token_path),
    )
    client.authenticate()

    hvac_client.auth.kubernetes.login.assert_called_once_with(
        role="mlt-role",
        jwt=_service_account_jwt(),
        mount_point="site-kubernetes",
    )
    assert client.credential_api_path("02:00:00:00:00:02") == (
        "site-secrets/data/machines/bmc/02:00:00:00:00:02/root"
    )


def test_authentication_requires_ca_and_service_account_token(tmp_path):
    with pytest.raises(SiteVaultError, match="CA certificate"):
        SiteVaultClient(
            ca_cert_path=str(tmp_path / "missing-ca"),
            service_account_token_path=str(tmp_path / "missing-token"),
        ).authenticate()

    ca_path = tmp_path / "ca.crt"
    ca_path.write_text("test CA", encoding="utf-8")
    with pytest.raises(SiteVaultError, match="service-account token is unavailable"):
        SiteVaultClient(
            ca_cert_path=str(ca_path),
            service_account_token_path=str(tmp_path / "missing-token"),
        ).authenticate()


def test_authentication_failure_is_redacted(tmp_path, monkeypatch):
    ca_path = tmp_path / "ca.crt"
    ca_path.write_text("test CA", encoding="utf-8")
    token_path = tmp_path / "token"
    token = _service_account_jwt()
    token_path.write_text(token, encoding="utf-8")
    hvac_client = MagicMock()
    hvac_client.auth.kubernetes.login.side_effect = hvac_exceptions.Forbidden("secret response")
    monkeypatch.setattr("lib.site_vault.hvac.Client", MagicMock(return_value=hvac_client))

    with pytest.raises(SiteVaultError) as raised:
        SiteVaultClient(
            ca_cert_path=str(ca_path),
            service_account_token_path=str(token_path),
        ).authenticate()

    assert token not in str(raised.value)
    assert "secret response" not in str(raised.value)


def test_get_bmc_credentials(authenticated_client):
    client, hvac_client, _ = authenticated_client
    hvac_client.secrets.kv.v2.read_secret_version.return_value = {
        "data": {
            "data": {
                "UsernamePassword": {
                    "username": "root",
                    "password": "super-secret",
                }
            }
        }
    }

    credentials = client.get_bmc_credentials("02:00:00:00:00:02")

    assert credentials == BmcCredentials(username="root", password="super-secret")
    assert "root" not in repr(credentials)
    assert "super-secret" not in repr(credentials)
    hvac_client.secrets.kv.v2.read_secret_version.assert_called_once_with(
        mount_point="secrets",
        path="machines/bmc/02:00:00:00:00:02/root",
    )


@pytest.mark.parametrize(
    "response",
    [
        {},
        {"data": {"data": {}}},
        {"data": {"data": {"UsernamePassword": {"username": "", "password": "x"}}}},
        {"data": {"data": {"UsernamePassword": {"username": "root"}}}},
    ],
)
def test_get_bmc_credentials_rejects_malformed_responses(authenticated_client, response):
    client, hvac_client, _ = authenticated_client
    hvac_client.secrets.kv.v2.read_secret_version.return_value = response

    with pytest.raises(SiteVaultError, match="malformed"):
        client.get_bmc_credentials("02:00:00:00:00:02")


@pytest.mark.parametrize(
    ("exception", "message"),
    [
        (hvac_exceptions.Forbidden("sensitive"), "denied"),
        (hvac_exceptions.InvalidPath("sensitive"), "missing"),
    ],
)
def test_get_bmc_credentials_redacts_vault_errors(
    authenticated_client, exception, message
):
    client, hvac_client, _ = authenticated_client
    hvac_client.secrets.kv.v2.read_secret_version.side_effect = exception

    with pytest.raises(SiteVaultError, match=message) as raised:
        client.get_bmc_credentials("02:00:00:00:00:02")
    assert "sensitive" not in str(raised.value)


def test_get_capabilities_supports_vault_response_shapes(authenticated_client):
    client, hvac_client, _ = authenticated_client
    paths = [
        "secrets/data/machines/bmc/02:00:00:00:00:02/root",
        "secrets/data/machines/bmc/site/root",
    ]
    hvac_client.sys.get_capabilities.return_value = {
        "data": {paths[0]: ["read"], paths[1]: ["read"]}
    }

    assert client.get_capabilities(paths) == {paths[0]: {"read"}, paths[1]: {"read"}}


def test_context_manager_logs_out(tmp_path, monkeypatch):
    ca_path = tmp_path / "ca.crt"
    ca_path.write_text("test CA", encoding="utf-8")
    token_path = tmp_path / "token"
    token_path.write_text(_service_account_jwt(), encoding="utf-8")
    hvac_client = MagicMock()
    monkeypatch.setattr("lib.site_vault.hvac.Client", MagicMock(return_value=hvac_client))

    with SiteVaultClient(
        ca_cert_path=str(ca_path), service_account_token_path=str(token_path)
    ):
        pass

    hvac_client.logout.assert_called_once_with(revoke_token=True)


def test_read_credentials_reads_host_and_each_dpu_by_bmc_mac(monkeypatch):
    expected = {
        "02:00:00:00:00:01": BmcCredentials("admin", "host-password"),
        "02:00:00:00:00:02": BmcCredentials("root", "dpu-one-password"),
        "02:00:00:00:00:03": BmcCredentials("root", "dpu-two-password"),
    }
    calls = []

    class FakeSiteVaultClient:
        def __enter__(self):
            return self

        def __exit__(self, exc_type, exc_val, exc_tb):
            return None

        def get_bmc_credentials(self, bmc_mac):
            calls.append(bmc_mac)
            return expected[bmc_mac]

    monkeypatch.setattr(lifecycle, "SiteVaultClient", FakeSiteVaultClient)
    machine_info = SimpleNamespace(
        host_bmc_mac="02:00:00:00:00:01",
        dpu_ids=["dpu-one", "dpu-two"],
        dpu_info_map={
            "dpu-one": {"bmc_mac": "02:00:00:00:00:02"},
            "dpu-two": {"bmc_mac": "02:00:00:00:00:03"},
        },
    )

    host, dpus = lifecycle._read_site_vault_bmc_credentials(machine_info)

    assert host == expected["02:00:00:00:00:01"]
    assert dpus == {
        "dpu-one": expected["02:00:00:00:00:02"],
        "dpu-two": expected["02:00:00:00:00:03"],
    }
    assert calls == [
        "02:00:00:00:00:01",
        "02:00:00:00:00:02",
        "02:00:00:00:00:03",
    ]


@pytest.mark.parametrize(
    ("version", "expected_path"),
    [
        (0, "machines/bmc/site/root"),
        (1, "machines/bmc/site/root/v1"),
        (12, "machines/bmc/site/root/v12"),
    ],
)
def test_sitewide_credential_path_matches_the_rotation_scheme(version, expected_path):
    assert sitewide_credential_path(version) == expected_path


def test_sitewide_credential_path_rejects_a_negative_version():
    with pytest.raises(SiteVaultError, match="negative"):
        sitewide_credential_path(-1)


def test_get_sitewide_bmc_password_reads_the_rotated_version(authenticated_client):
    client, hvac_client, _ = authenticated_client
    hvac_client.secrets.kv.v2.read_secret_version.return_value = {
        "data": {"data": {"UsernamePassword": {"username": "", "password": "rotated"}}}
    }

    assert client.get_sitewide_bmc_password(3) == "rotated"
    hvac_client.secrets.kv.v2.read_secret_version.assert_called_once_with(
        mount_point="secrets",
        path="machines/bmc/site/root/v3",
    )


def test_versioned_sitewide_errors_still_name_the_site_wide_record(authenticated_client):
    client, hvac_client, _ = authenticated_client
    hvac_client.secrets.kv.v2.read_secret_version.side_effect = hvac_exceptions.InvalidPath("x")

    with pytest.raises(SiteVaultCredentialNotFound, match="site-wide BMC credentials"):
        client.get_sitewide_bmc_password(2)


def test_get_sitewide_bmc_password_tolerates_the_empty_stored_username(authenticated_client):
    client, hvac_client, _ = authenticated_client
    # Site Explorer stores this record with an empty username and pairs the
    # password with each BMC's own username, so an empty username is valid.
    hvac_client.secrets.kv.v2.read_secret_version.return_value = {
        "data": {"data": {"UsernamePassword": {"username": "", "password": "sitewide"}}}
    }

    assert client.get_sitewide_bmc_password() == "sitewide"
    hvac_client.secrets.kv.v2.read_secret_version.assert_called_once_with(
        mount_point="secrets",
        path="machines/bmc/site/root",
    )


def test_get_sitewide_bmc_password_still_requires_a_password(authenticated_client):
    client, hvac_client, _ = authenticated_client
    hvac_client.secrets.kv.v2.read_secret_version.return_value = {
        "data": {"data": {"UsernamePassword": {"username": "", "password": ""}}}
    }

    with pytest.raises(SiteVaultError, match="malformed site-wide BMC credentials"):
        client.get_sitewide_bmc_password()


def test_get_sitewide_bmc_password_names_the_record_in_errors(authenticated_client):
    client, hvac_client, _ = authenticated_client
    hvac_client.secrets.kv.v2.read_secret_version.side_effect = hvac_exceptions.Forbidden(
        "sensitive"
    )

    with pytest.raises(SiteVaultError, match="site-wide BMC credentials") as raised:
        client.get_sitewide_bmc_password()
    assert "sensitive" not in str(raised.value)


def test_verify_bmc_sitewide_credentials_checks_host_and_dpus(monkeypatch):
    checked = []
    monkeypatch.setattr(
        lifecycle.admin_cli,
        "get_bmc_accounts",
        lambda bmc_ip, username, password: checked.append((bmc_ip, username, password)),
    )
    site_config = SimpleNamespace(
        sitewide_bmc_password="sitewide-password",
        host_bmc_credentials=BmcCredentials("host-admin", "per-bmc-host"),
        dpu_bmc_credentials={
            "dpu-one": BmcCredentials("root", "per-bmc-one"),
            "dpu-two": BmcCredentials("admin", "per-bmc-two"),
        },
    )
    machine_info = SimpleNamespace(
        host_bmc_ip="192.0.2.1",
        dpu_ids=["dpu-one", "dpu-two"],
        dpu_info_map={
            "dpu-one": {"bmc_ip": "192.0.2.11"},
            "dpu-two": {"bmc_ip": "192.0.2.21"},
        },
    )

    lifecycle.verify_bmc_sitewide_credentials(site_config, machine_info)

    # Each endpoint keeps its own username but always uses the site-wide password.
    assert checked == [
        ("192.0.2.1", "host-admin", "sitewide-password"),
        ("192.0.2.11", "root", "sitewide-password"),
        ("192.0.2.21", "admin", "sitewide-password"),
    ]


def test_sitewide_password_read_uses_the_live_rotation_version(monkeypatch):
    monkeypatch.setattr(
        lifecycle.admin_cli, "get_sitewide_bmc_rotation_target_version", lambda: 4
    )
    requested = []

    class FakeSiteVaultClient:
        def __enter__(self):
            return self

        def __exit__(self, exc_type, exc_val, exc_tb):
            return None

        def get_sitewide_bmc_password(self, version):
            requested.append(version)
            return "rotated-password"

    monkeypatch.setattr(lifecycle, "SiteVaultClient", FakeSiteVaultClient)

    assert lifecycle._read_site_vault_sitewide_bmc_password() == "rotated-password"
    assert requested == [4]


@pytest.mark.parametrize(
    "error",
    [subprocess.CalledProcessError(1, ["admin-cli"]), ValueError("no target_version")],
)
def test_sitewide_password_read_fails_when_the_version_is_unknown(monkeypatch, error):
    def explode():
        raise error

    monkeypatch.setattr(
        lifecycle.admin_cli, "get_sitewide_bmc_rotation_target_version", explode
    )

    # Guessing a version could verify a superseded password, so this must stop
    # the run rather than fall back to 0.
    with pytest.raises(BaseException, match="live site-wide BMC credential version"):
        lifecycle._read_site_vault_sitewide_bmc_password()


def _converged_machine_info():
    return SimpleNamespace(
        host_bmc_mac="02:00:00:00:00:01",
        dpu_ids=["dpu-one", "dpu-two"],
        dpu_info_map={
            "dpu-one": {"bmc_mac": "02:00:00:00:00:02"},
            "dpu-two": {"bmc_mac": "02:00:00:00:00:03"},
        },
    )


def test_rotation_gate_passes_on_a_never_rotated_site(monkeypatch, capsys):
    # What qa6 reports today: target 0, device at version 0, converged.
    monkeypatch.setattr(
        lifecycle.admin_cli,
        "get_bmc_rotation_device_status",
        lambda bmc_mac: {"current_version": 0, "target_version": 0, "converged": True},
    )

    lifecycle.verify_bmc_rotation_converged(_converged_machine_info())

    assert capsys.readouterr().out.count("PASS") == 3


def test_rotation_gate_stops_a_lagging_bmc_before_the_force_delete(monkeypatch):
    monkeypatch.setattr(
        lifecycle.admin_cli,
        "get_bmc_rotation_device_status",
        lambda bmc_mac: {
            "current_version": 1,
            "target_version": 2,
            "converged": bmc_mac != "02:00:00:00:00:03",
            "quarantined": True,
            "quarantined_until": "2026-07-28T12:00:00Z",
            "last_error": "auth failed",
        },
    )

    with pytest.raises(BaseException, match="has not converged") as raised:
        lifecycle.verify_bmc_rotation_converged(_converged_machine_info())

    message = str(raised.value)
    assert "02:00:00:00:00:03" in message
    assert "current version 1" in message and "site target version 2" in message
    assert "quarantined until" in message and "auth failed" in message


def test_rotation_gate_stops_when_the_status_cannot_be_read(monkeypatch):
    def explode(bmc_mac):
        raise subprocess.CalledProcessError(1, ["admin-cli"])

    monkeypatch.setattr(lifecycle.admin_cli, "get_bmc_rotation_device_status", explode)

    with pytest.raises(BaseException, match="Could not read rotation status"):
        lifecycle.verify_bmc_rotation_converged(_converged_machine_info())


def test_refresh_replaces_pre_delete_credentials(monkeypatch):
    refreshed_host = BmcCredentials("admin", "refreshed-host-password")
    refreshed_dpus = {
        "dpu-one": BmcCredentials("root", "refreshed-dpu-password")
    }
    machine_info = SimpleNamespace(host_bmc_mac="02:00:00:00:00:01")
    site_config = SimpleNamespace(
        host_bmc_credentials=BmcCredentials("admin", "pre-delete-host-password"),
        dpu_bmc_credentials={
            "dpu-one": BmcCredentials("root", "pre-delete-dpu-password")
        },
    )
    monkeypatch.setattr(
        lifecycle,
        "_read_site_vault_bmc_credentials",
        lambda received_machine_info: (
            refreshed_host,
            refreshed_dpus,
        ),
    )

    lifecycle._refresh_site_vault_bmc_credentials(site_config, machine_info)

    assert site_config.host_bmc_credentials == refreshed_host
    assert site_config.dpu_bmc_credentials == refreshed_dpus


def test_collect_machine_info_requires_and_retains_dpu_bmc_mac(monkeypatch):
    machine = {
        "host_bmc_ip": "192.0.2.10",
        "host_bmc_mac": "02:00:00:00:00:01",
        "dpus": [
            {
                "machine_id": "fm100ds-test",
                "bmc_ip": "192.0.2.11",
                "bmc_mac": "02:00:00:00:00:02",
                "oob_ip": "192.0.2.12",
            }
        ],
    }
    monkeypatch.setattr(lifecycle.admin_cli, "get_machine_from_mh_show", lambda _id: machine)
    monkeypatch.setattr(lifecycle.admin_cli, "get_machine_vendor", lambda _id: "NVIDIA")
    test_config = SimpleNamespace(
        machine_under_test="fm100ht-test",
        expected_dpu_count=1,
    )

    machine_info = lifecycle.collect_machine_info(test_config)

    assert machine_info.dpu_info_map["fm100ds-test"]["bmc_mac"] == "02:00:00:00:00:02"


def test_setup_uses_site_vault_for_bmc_and_a_configured_source_for_the_bearer(
    monkeypatch,
):
    host_credentials = BmcCredentials("admin", "host-password")
    dpu_credentials = {"dpu-one": BmcCredentials("root", "dpu-password")}
    monkeypatch.setattr(
        lifecycle,
        "_read_site_vault_bmc_credentials",
        lambda _machine_info: (host_credentials, dpu_credentials),
    )

    oauth_config = SimpleNamespace(
        token_url="https://issuer.example.test/token", scope="carbide"
    )
    resolved_from = []
    monkeypatch.setattr(
        lifecycle,
        "resolve_client_credential",
        lambda config: resolved_from.append(config) or "client-id:client-secret",
    )
    exchanges = []
    monkeypatch.setattr(
        lifecycle.nico_rest.oauth,
        "fetch_access_token",
        lambda credential, url, scope: (
            exchanges.append((credential, url, scope)) or "nico-token"
        ),
    )
    monkeypatch.setenv("NICO_BASE_URL", "https://nico.example.test")
    monkeypatch.delenv("NICO_TOKEN", raising=False)

    config = lifecycle.setup_site_config(
        SimpleNamespace(
            site_under_test="example-site",
            test_sitewide_bmc_fallback=False,
            oauth=oauth_config,
        ),
        SimpleNamespace(),
    )

    # From the configured source, with the exchange details kept for refresh.
    assert resolved_from == [oauth_config]
    assert exchanges == [
        ("client-id:client-secret", "https://issuer.example.test/token", "carbide")
    ]
    # Retained in memory, never in the environment subprocesses inherit.
    assert lifecycle.nico_rest._token_refresh == (
        "client-id:client-secret",
        "https://issuer.example.test/token",
        "carbide",
    )
    assert "NICO_OAUTH_CREDENTIAL" not in os.environ
    assert config.site.name == "example-site"
    assert config.host_bmc_credentials == host_credentials
    assert config.dpu_bmc_credentials == dpu_credentials
    # The site-wide record is only read for the fallback test.
    assert config.sitewide_bmc_password is None


def _deleted_credentials_machine_info():
    return SimpleNamespace(
        host_bmc_mac="02:00:00:00:00:01",
        dpu_ids=["dpu-one", "dpu-two"],
        dpu_info_map={
            "dpu-one": {"bmc_mac": "02:00:00:00:00:02"},
            "dpu-two": {"bmc_mac": "02:00:00:00:00:03"},
        },
    )


def _fake_site_vault_client(reads, responder):
    """Build a SiteVaultClient stand-in recording MACs read and sessions closed.

    `responder` is called with each MAC; raising simulates the credential
    being absent, returning simulates it surviving the force-delete.
    """
    sessions = SimpleNamespace(opened=0, closed=0)

    class FakeSiteVaultClient:
        def __enter__(self):
            sessions.opened += 1
            return self

        def __exit__(self, exc_type, exc_val, exc_tb):
            sessions.closed += 1
            return None

        def get_bmc_credentials(self, bmc_mac):
            reads.append(bmc_mac)
            return responder(bmc_mac)

    return FakeSiteVaultClient, sessions


def test_missing_credentials_probes_host_and_every_dpu_in_one_session(monkeypatch, capsys):
    reads = []

    def absent(bmc_mac):
        raise SiteVaultCredentialNotFound(f"no record for {bmc_mac}")

    client, sessions = _fake_site_vault_client(reads, absent)
    monkeypatch.setattr(lifecycle, "SiteVaultClient", client)

    lifecycle.assert_per_mac_credentials_missing(_deleted_credentials_machine_info())

    assert reads == [
        "02:00:00:00:00:01",
        "02:00:00:00:00:02",
        "02:00:00:00:00:03",
    ]
    assert (sessions.opened, sessions.closed) == (1, 1)
    assert "successfully deleted" in capsys.readouterr().out


@pytest.mark.parametrize(
    ("surviving_mac", "expected_name"),
    [
        ("02:00:00:00:00:01", "host"),
        ("02:00:00:00:00:03", "DPU dpu-two"),
    ],
)
def test_a_surviving_credential_fails_the_run(monkeypatch, surviving_mac, expected_name):
    # A record left behind means the force-delete did not remove it, so the
    # site-wide fallback test that follows would be unable to distinguish
    # "fallback worked" from "creds were never deleted".
    reads = []

    def absent_except_one(bmc_mac):
        if bmc_mac == surviving_mac:
            return BmcCredentials("root", "still-here")
        raise SiteVaultCredentialNotFound(f"no record for {bmc_mac}")

    client, sessions = _fake_site_vault_client(reads, absent_except_one)
    monkeypatch.setattr(lifecycle, "SiteVaultClient", client)

    with pytest.raises(BaseException, match="per-BMC credentials still exist after force-delete") as raised:
        lifecycle.assert_per_mac_credentials_missing(_deleted_credentials_machine_info())

    message = str(raised.value)
    assert expected_name in message
    assert surviving_mac in message
    # The failure propagates out of the `with` block, so the session must still
    # be logged out.
    assert (sessions.opened, sessions.closed) == (1, 1)


def test_a_vault_read_error_is_not_treated_as_a_deleted_credential(monkeypatch):
    # Only SiteVaultCredentialNotFound proves absence. Any other Site Vault
    # error (auth failure, malformed record) must propagate rather than let the
    # probe report a clean deletion it never observed.
    reads = []

    def unreadable(bmc_mac):
        raise SiteVaultError("permission denied reading credential record")

    client, sessions = _fake_site_vault_client(reads, unreadable)
    monkeypatch.setattr(lifecycle, "SiteVaultClient", client)

    with pytest.raises(SiteVaultError, match="permission denied"):
        lifecycle.assert_per_mac_credentials_missing(_deleted_credentials_machine_info())

    # Aborted on the first unreadable record rather than continuing.
    assert reads == ["02:00:00:00:00:01"]
    assert (sessions.opened, sessions.closed) == (1, 1)


def test_missing_credentials_probe_tolerates_a_machine_with_no_dpus(monkeypatch):
    reads = []

    def absent(bmc_mac):
        raise SiteVaultCredentialNotFound(f"no record for {bmc_mac}")

    client, _sessions = _fake_site_vault_client(reads, absent)
    monkeypatch.setattr(lifecycle, "SiteVaultClient", client)

    lifecycle.assert_per_mac_credentials_missing(
        SimpleNamespace(
            host_bmc_mac="02:00:00:00:00:01",
            dpu_ids=[],
            dpu_info_map={},
        )
    )

    assert reads == ["02:00:00:00:00:01"]
