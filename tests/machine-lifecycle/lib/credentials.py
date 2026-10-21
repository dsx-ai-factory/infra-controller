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

"""Read an OAuth client credential from a configured Vault.

Where the credential lives, and how the reader authenticates to hold it, are
deployment facts rather than properties of the test, so both come from the
``[oauth]`` configuration section. Nothing here names a site, a Vault or an
identity provider.

Authentication is ``kubernetes`` (the pod's own service-account token, needing
no external credential), ``jwt`` (a JWT from a file or environment variable),
or ``token`` (``$VAULT_TOKEN`` or ``~/.vault-token``, for developer machines).
"""

from __future__ import annotations

import os
from pathlib import Path

import hvac
import requests
from hvac import exceptions as hvac_exceptions

from lib.config import OAuthConfig
from lib.site_vault import service_account_role_from_jwt

DEFAULT_SERVICE_ACCOUNT_TOKEN_PATH = (
    "/var/run/secrets/kubernetes.io/serviceaccount/token"
)
DEFAULT_VAULT_TOKEN_FILE = "~/.vault-token"


class CredentialError(Exception):
    """A safe-to-display error that never contains credential material."""


class ClientCredential(str):
    """A ``client_id:client_secret`` pair whose repr redacts its value.

    A ``str`` subclass so it passes straight to
    :func:`lib.oauth.fetch_access_token`, while staying out of tracebacks and
    anything that renders a container with ``repr``.
    """

    __slots__ = ()

    def __repr__(self) -> str:
        return "ClientCredential(<redacted>)"


def _jwt_from_source(source: str) -> str:
    """Return the JWT named by an ``env:NAME`` or ``file:/path`` source."""

    scheme, separator, location = source.partition(":")
    if not separator or not location:
        raise CredentialError(
            "oauth.auth_jwt_source must be 'env:<NAME>' or 'file:<PATH>'"
        )

    if scheme == "env":
        jwt = os.environ.get(location, "")
        if not jwt.strip():
            raise CredentialError(f"${location} is unset or empty")
        return jwt.strip()

    if scheme == "file":
        path = Path(location).expanduser()
        if not path.is_file():
            raise CredentialError(f"JWT file {path} is unavailable")
        try:
            jwt = path.read_text(encoding="utf-8").strip()
        except OSError:
            raise CredentialError(f"JWT file {path} could not be read") from None
        if not jwt:
            raise CredentialError(f"JWT file {path} is empty")
        return jwt

    raise CredentialError(
        "oauth.auth_jwt_source must be 'env:<NAME>' or 'file:<PATH>'"
    )


def _existing_vault_token() -> str:
    """Return a Vault token from the environment or the user's token file."""

    token = os.environ.get("VAULT_TOKEN", "")
    if token.strip():
        return token.strip()

    path = Path(DEFAULT_VAULT_TOKEN_FILE).expanduser()
    if not path.is_file():
        raise CredentialError(
            f"$VAULT_TOKEN is unset and {path} does not exist"
        )
    try:
        token = path.read_text(encoding="utf-8").strip()
    except OSError:
        raise CredentialError(f"Vault token file {path} could not be read") from None
    if not token:
        raise CredentialError(f"Vault token file {path} is empty")
    return token


class CredentialReader:
    """Authenticate to a configured Vault and read the client credential.

    This is a context manager: use it in a ``with`` block so the Vault token
    obtained for the read is revoked rather than left to expire.
    """

    def __init__(self, config: OAuthConfig) -> None:
        self.config = config
        self._client: hvac.Client | None = None
        # Whether the token this client holds was issued by our own login, and
        # is therefore ours to revoke.
        self._minted_token = False

    def __enter__(self) -> CredentialReader:
        self.authenticate()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:
        self.logout()

    def authenticate(self) -> None:
        """Obtain a Vault token using the configured authentication method."""

        if not self.config.vault_address or not self.config.secret_path:
            raise CredentialError(
                "Reading a client credential needs oauth.vault_address and "
                "oauth.secret_path"
            )

        verify: str | bool = True
        if self.config.vault_cacert:
            ca_path = Path(self.config.vault_cacert)
            if not ca_path.is_file():
                raise CredentialError("Vault CA certificate is unavailable")
            verify = str(ca_path)

        client = hvac.Client(
            url=self.config.vault_address,
            namespace=self.config.vault_namespace or None,
            verify=verify,
        )
        # Held before the login so that a token issued by a login which then
        # fails a later check is still revoked rather than left live. __exit__
        # does not run when __enter__ raises, so the cleanup happens here.
        self._client = client
        try:
            try:
                self._minted_token = self._login(client)
            except (hvac_exceptions.VaultError, requests.RequestException, OSError):
                # Vault's own message can echo the request, so report only where.
                raise CredentialError(
                    f"Vault authentication failed at {self.config.vault_address} "
                    f"using mount {self.config.auth_mount!r}"
                ) from None

            try:
                authenticated = client.is_authenticated()
            except (hvac_exceptions.VaultError, requests.RequestException, OSError):
                raise CredentialError(
                    f"Vault authentication could not be confirmed at "
                    f"{self.config.vault_address}"
                ) from None
            if not authenticated:
                raise CredentialError(
                    f"Vault authentication produced no usable token at "
                    f"{self.config.vault_address}"
                )
        except BaseException:
            self.logout()
            raise

    def _login(self, client: hvac.Client) -> bool:
        """Authenticate, returning True if this issued a new Vault token."""

        method = self.config.auth_method

        if method == "token":
            # Borrowed from the environment: someone else's session, not ours
            # to revoke.
            client.token = _existing_vault_token()
            return False

        jwt = _jwt_from_source(self.config.auth_jwt_source)
        # A Kubernetes auth mount names its roles after service accounts, so an
        # unset role comes from the token.
        role = self.config.auth_role
        if not role:
            if method != "kubernetes":
                raise CredentialError(
                    "oauth.auth_role must be set when oauth.auth_method is 'jwt'"
                )
            role = service_account_role_from_jwt(jwt)

        if method == "kubernetes":
            client.auth.kubernetes.login(
                role=role, jwt=jwt, mount_point=self.config.auth_mount
            )
            return True

        client.auth.jwt.jwt_login(
            role=role, jwt=jwt, path=self.config.auth_mount
        )
        return True

    def logout(self) -> None:
        """Release the Vault token, revoking it if this client obtained it."""

        if self._client is None:
            return
        try:
            # hvac's logout defaults to revoke_token=False, which drops only
            # the local copy and leaves the token live for its whole lease.
            self._client.logout(revoke_token=self._minted_token)
        except (hvac_exceptions.VaultError, requests.RequestException):
            # Cleanup must not mask the result of the credential read.
            pass
        finally:
            self._client = None
            self._minted_token = False

    def read(self) -> ClientCredential:
        """Read and validate the client credential as ``client_id:client_secret``."""

        record = self._read_record()
        client_id = self._require_field(record, self.config.client_id_field)
        client_secret = self._require_field(record, self.config.client_secret_field)
        return ClientCredential(f"{client_id}:{client_secret}")

    def _read_record(self) -> dict:
        """Read the configured secret, redacting Vault's error detail."""

        if self._client is None:
            raise CredentialError("Vault client is not authenticated")

        path = self.config.secret_path
        try:
            if self.config.secret_engine == "kv-v2":
                response = self._client.secrets.kv.v2.read_secret_version(
                    mount_point=self.config.secret_mount, path=path
                )
                record = response["data"]["data"]
            else:
                response = self._client.read(path)
                if response is None:
                    raise hvac_exceptions.InvalidPath()
                record = response["data"]
        except hvac_exceptions.Forbidden:
            raise CredentialError(
                f"Vault denied access to the client credential at {path!r}"
            ) from None
        except hvac_exceptions.InvalidPath:
            raise CredentialError(
                f"No client credential exists at {path!r}"
            ) from None
        except (hvac_exceptions.VaultError, requests.RequestException):
            raise CredentialError(
                f"Vault failed to read the client credential at {path!r}"
            ) from None
        except (KeyError, TypeError):
            raise CredentialError(
                f"Vault returned a malformed client credential at {path!r}"
            ) from None

        if not isinstance(record, dict):
            raise CredentialError(
                f"Vault returned a malformed client credential at {path!r}"
            )
        return record

    def _require_field(self, record: dict, field: str) -> str:
        """Return one non-empty string field, naming only the field on error."""

        value = record.get(field)
        if not isinstance(value, str) or not value.strip():
            raise CredentialError(
                f"The client credential at {self.config.secret_path!r} has no "
                f"usable {field!r} field"
            )
        # A trailing newline here would corrupt the Basic-auth header.
        return value.strip()


def resolve_client_credential(
    config: OAuthConfig | None,
) -> ClientCredential | None:
    """Return the client credential, or None when none is available.

    ``$OAUTH_CREDENTIAL`` short-circuits the Vault read, so a developer
    with a credential in hand needs no Vault access at all.
    """

    direct = os.environ.get("OAUTH_CREDENTIAL", "")
    if direct.strip():
        return ClientCredential(direct.strip())

    if config is None:
        return None

    with CredentialReader(config) as reader:
        return reader.read()
