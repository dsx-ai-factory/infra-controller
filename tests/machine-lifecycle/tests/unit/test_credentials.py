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

import pytest
import requests
from hvac import exceptions as hvac_exceptions

from lib import credentials
from lib.config import OAuthConfig


@pytest.fixture(autouse=True)
def isolate_credential_environment(monkeypatch, tmp_path):
    """Keep a developer's real Vault token and credential out of these tests.

    Without it, a test that forgets to set $VAULT_TOKEN falls back to
    ~/.vault-token and passes only on a machine that has one.
    """
    monkeypatch.delenv("VAULT_TOKEN", raising=False)
    monkeypatch.delenv("OAUTH_CREDENTIAL", raising=False)
    monkeypatch.delenv("VAULT_JWT_TOKEN", raising=False)
    monkeypatch.setattr(
        credentials, "DEFAULT_VAULT_TOKEN_FILE", str(tmp_path / "absent-vault-token")
    )


def _config(**overrides) -> OAuthConfig:
    settings = {
        "token_url": "https://issuer.example.test/token",
        "scope": "carbide",
        "vault_address": "https://vault.example.test",
        "secret_path": "path/to/credential",
        "client_id_field": "client_id",
        "client_secret_field": "secret",
    }
    settings.update(overrides)
    return OAuthConfig(**settings)


def _service_account_jwt(name: str = "nico-qa") -> str:
    payload = base64.urlsafe_b64encode(
        json.dumps({"kubernetes.io": {"serviceaccount": {"name": name}}}).encode()
    ).decode().rstrip("=")
    return f"header.{payload}.signature"


class FakeKvV2:
    def __init__(self, record, error=None):
        self.record = record
        self.error = error
        self.calls = []

    def read_secret_version(self, *, mount_point, path):
        self.calls.append((mount_point, path))
        if self.error is not None:
            raise self.error
        return {"data": {"data": self.record}}


class FakeClient:
    """Stand-in for hvac.Client recording how the login was performed."""

    def __init__(self, record=None, error=None, authenticated=True):
        self.token = None
        self.logged_out = False
        self.token_revoked = False
        self.authenticated = authenticated
        self.kubernetes_logins = []
        self.jwt_logins = []
        self.raw_reads = []
        self._raw_record = record
        self._raw_error = error
        kv_v2 = FakeKvV2(record, error)
        self.secrets = type(
            "Secrets", (), {"kv": type("Kv", (), {"v2": kv_v2})()}
        )()
        self.auth = type(
            "Auth",
            (),
            {
                "kubernetes": type(
                    "K8s", (), {"login": self._kubernetes_login}
                )(),
                "jwt": type("Jwt", (), {"jwt_login": self._jwt_login})(),
            },
        )()

    def _kubernetes_login(self, *, role, jwt, mount_point):
        self.kubernetes_logins.append((role, jwt, mount_point))

    def _jwt_login(self, *, role, jwt, path):
        self.jwt_logins.append((role, jwt, path))

    def is_authenticated(self):
        return self.authenticated

    def read(self, path):
        self.raw_reads.append(path)
        if self._raw_error is not None:
            raise self._raw_error
        if self._raw_record is None:
            return None
        return {"data": self._raw_record}

    def logout(self, revoke_token=False):
        self.logged_out = True
        self.token_revoked = revoke_token


def _install(monkeypatch, client):
    monkeypatch.setattr(credentials.hvac, "Client", lambda **_kwargs: client)
    return client


def test_credential_repr_never_shows_the_value():
    credential = credentials.ClientCredential("client-id:super-secret")

    assert "super-secret" not in repr(credential)
    assert credential == "client-id:super-secret"


def test_direct_credential_skips_vault_entirely(monkeypatch):
    monkeypatch.setenv("OAUTH_CREDENTIAL", "  client-id:secret  ")

    def vault_must_not_be_used(**_kwargs):
        raise AssertionError("OAUTH_CREDENTIAL must not trigger a Vault read")

    monkeypatch.setattr(credentials.hvac, "Client", vault_must_not_be_used)

    assert credentials.resolve_client_credential(None) == "client-id:secret"


def test_no_credential_and_no_configuration_is_not_an_error(monkeypatch):

    assert credentials.resolve_client_credential(None) is None


def test_kv_v2_read_joins_the_two_fields(monkeypatch):
    client = _install(monkeypatch, FakeClient({"client_id": "id", "secret": "shh"}))
    monkeypatch.setenv("VAULT_JWT_TOKEN", _service_account_jwt())

    config = _config(auth_method="jwt", auth_role="mlt", auth_jwt_source="env:VAULT_JWT_TOKEN")
    credential = credentials.resolve_client_credential(config)

    assert credential == "id:shh"
    assert client.secrets.kv.v2.calls == [("secrets", "path/to/credential")]
    assert client.logged_out
    assert client.token_revoked


def test_fields_are_stripped_so_a_stray_newline_cannot_corrupt_basic_auth(monkeypatch):
    _install(monkeypatch, FakeClient({"client_id": "id\n", "secret": " shh \n"}))
    monkeypatch.setenv("VAULT_TOKEN", "root")

    with credentials.CredentialReader(_config(auth_method="token")) as reader:
        assert reader.read() == "id:shh"


def test_raw_engine_reads_the_logical_path(monkeypatch):
    client = _install(
        monkeypatch, FakeClient({"client_id": "dsx-id", "secret": "dsx-secret"})
    )
    monkeypatch.setenv("VAULT_TOKEN", "root")

    config = _config(
        auth_method="token",
        secret_engine="raw",
        secret_path="services/dsx/clients/mlt/issue/creds",
    )

    assert credentials.resolve_client_credential(config) == "dsx-id:dsx-secret"
    assert client.raw_reads == ["services/dsx/clients/mlt/issue/creds"]


def test_kubernetes_auth_derives_its_role_from_the_service_account_token(
    monkeypatch, tmp_path
):
    client = _install(monkeypatch, FakeClient({"client_id": "id", "secret": "shh"}))
    token_file = tmp_path / "token"
    token_file.write_text(_service_account_jwt("nico-qa"), encoding="utf-8")

    config = _config(
        auth_method="kubernetes",
        auth_mount="kubernetes",
        auth_jwt_source=f"file:{token_file}",
    )
    credentials.resolve_client_credential(config)

    (role, _jwt, mount) = client.kubernetes_logins[0]
    assert role == "nico-qa"
    assert mount == "kubernetes"


def test_jwt_auth_requires_an_explicit_role(monkeypatch):
    _install(monkeypatch, FakeClient({"client_id": "id", "secret": "shh"}))
    monkeypatch.setenv("VAULT_JWT_TOKEN", _service_account_jwt())

    config = _config(auth_method="jwt", auth_jwt_source="env:VAULT_JWT_TOKEN")

    with pytest.raises(credentials.CredentialError, match="oauth.auth_role must be set"):
        credentials.resolve_client_credential(config)


def test_jwt_auth_uses_the_configured_mount_and_role(monkeypatch):
    client = _install(monkeypatch, FakeClient({"client_id": "id", "secret": "shh"}))
    monkeypatch.setenv("VAULT_JWT_TOKEN", "oidc-token")

    config = _config(
        auth_method="jwt",
        auth_mount="jwt/k8s/dsx-example",
        auth_role="mlt",
        auth_jwt_source="env:VAULT_JWT_TOKEN",
    )
    credentials.resolve_client_credential(config)

    assert client.jwt_logins == [("mlt", "oidc-token", "jwt/k8s/dsx-example")]


def test_missing_jwt_environment_variable_is_reported_by_name(monkeypatch):
    _install(monkeypatch, FakeClient({"client_id": "id", "secret": "shh"}))
    monkeypatch.delenv("VAULT_JWT_TOKEN", raising=False)

    config = _config(
        auth_method="jwt", auth_role="mlt", auth_jwt_source="env:VAULT_JWT_TOKEN"
    )

    with pytest.raises(credentials.CredentialError, match=r"\$VAULT_JWT_TOKEN is unset"):
        credentials.resolve_client_credential(config)


@pytest.mark.parametrize(
    ("record", "expected"),
    [
        ({"secret": "shh"}, "client_id"),
        ({"client_id": "id"}, "secret"),
        ({"client_id": "id", "secret": "   "}, "secret"),
        ({"client_id": 7, "secret": "shh"}, "client_id"),
    ],
)
def test_a_malformed_record_names_the_field_and_nothing_else(
    monkeypatch, record, expected
):
    _install(monkeypatch, FakeClient(record))
    monkeypatch.setenv("VAULT_TOKEN", "root")

    with pytest.raises(credentials.CredentialError) as error:
        credentials.resolve_client_credential(_config(auth_method="token"))

    message = str(error.value)
    assert repr(expected) in message
    assert "shh" not in message


@pytest.mark.parametrize(
    ("vault_error", "expected"),
    [
        (hvac_exceptions.Forbidden("denied"), "denied access"),
        (hvac_exceptions.InvalidPath("nope"), "No client credential exists"),
        (requests.RequestException("boom"), "failed to read"),
    ],
)
def test_vault_read_errors_are_reported_without_vault_detail(
    monkeypatch, vault_error, expected
):
    _install(monkeypatch, FakeClient(None, error=vault_error))
    monkeypatch.setenv("VAULT_TOKEN", "root")

    with pytest.raises(credentials.CredentialError, match=expected):
        credentials.resolve_client_credential(_config(auth_method="token"))


def test_authentication_failure_names_the_mount_not_the_cause(monkeypatch):
    _install(monkeypatch, FakeClient({}, authenticated=False))
    monkeypatch.setenv("VAULT_TOKEN", "root")

    with pytest.raises(credentials.CredentialError, match="produced no usable token"):
        credentials.resolve_client_credential(_config(auth_method="token"))


def test_a_minted_token_is_revoked_even_when_the_read_fails(monkeypatch):
    client = _install(
        monkeypatch, FakeClient(None, error=hvac_exceptions.Forbidden("denied"))
    )
    monkeypatch.setenv("VAULT_JWT_TOKEN", _service_account_jwt())

    with pytest.raises(credentials.CredentialError):
        credentials.resolve_client_credential(
            _config(auth_method="jwt", auth_role="mlt",
                    auth_jwt_source="env:VAULT_JWT_TOKEN")
        )

    # Revoked server-side, not merely dropped locally.
    assert client.logged_out
    assert client.token_revoked


def test_a_borrowed_token_is_released_but_never_revoked(monkeypatch):
    """$VAULT_TOKEN is someone else's session; revoking it would end it."""
    client = _install(monkeypatch, FakeClient({"client_id": "i", "secret": "s"}))
    monkeypatch.setenv("VAULT_TOKEN", "a-developers-own-session")

    credentials.resolve_client_credential(_config(auth_method="token"))

    assert client.logged_out
    assert not client.token_revoked


def test_a_token_issued_before_a_failed_check_is_still_revoked(monkeypatch):
    """__exit__ never runs when __enter__ raises, so authenticate cleans up."""
    client = _install(monkeypatch, FakeClient({"client_id": "i", "secret": "s"}))
    client.is_authenticated = lambda: (_ for _ in ()).throw(
        requests.RequestException("network gone")
    )
    monkeypatch.setenv("VAULT_JWT_TOKEN", _service_account_jwt())

    with pytest.raises(credentials.CredentialError, match="could not be confirmed"):
        credentials.resolve_client_credential(
            _config(auth_method="jwt", auth_role="mlt",
                    auth_jwt_source="env:VAULT_JWT_TOKEN")
        )

    assert client.token_revoked
