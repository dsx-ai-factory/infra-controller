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

"""Read per-machine BMC credentials from a NICo site's Vault instance."""

from __future__ import annotations

import base64
import binascii
import json
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable

import hvac
import requests
from hvac import exceptions as hvac_exceptions

DEFAULT_SITE_VAULT_ADDR = "https://vault.vault.svc.cluster.local:8200"
DEFAULT_SITE_VAULT_CACERT = "/tmp/site-vault-ca.crt"
DEFAULT_SITE_VAULT_AUTH_MOUNT = "kubernetes"
DEFAULT_SITE_VAULT_KV_MOUNT = "secrets"
DEFAULT_SERVICE_ACCOUNT_TOKEN_PATH = (
    "/var/run/secrets/kubernetes.io/serviceaccount/token"
)

_BMC_MAC_PATTERN = re.compile(r"^(?:[0-9A-Fa-f]{2}:){5}[0-9A-Fa-f]{2}$")

# The site-wide BMC root credential, used by Site Explorer as a fallback when a
# machine has no per-BMC record.
SITEWIDE_CREDENTIAL_PATH = "machines/bmc/site/root"


def sitewide_credential_path(version: int) -> str:
    """Return the KV-relative path for one version of the site-wide credential.
    """

    if version < 0:
        raise SiteVaultError("Site-wide BMC credential version cannot be negative")
    if version == 0:
        return SITEWIDE_CREDENTIAL_PATH
    return f"{SITEWIDE_CREDENTIAL_PATH}/v{version}"


class SiteVaultError(Exception):
    """A safe-to-display Site Vault error that never contains secret material."""


class SiteVaultCredentialNotFound(SiteVaultError):
    """The requested credential record does not exist."""


@dataclass(frozen=True, repr=False)
class BmcCredentials:
    """A BMC username/password pair whose repr intentionally redacts both values."""

    username: str
    password: str

    def __repr__(self) -> str:
        return "BmcCredentials(username=<redacted>, password=<redacted>)"


@dataclass(frozen=True, repr=False)
class ClientCertificate:
    """A minted client certificate whose repr intentionally redacts the key."""

    certificate: str
    private_key: str

    def __repr__(self) -> str:
        return "ClientCertificate(certificate=<redacted>, private_key=<redacted>)"


def normalize_bmc_mac(bmc_mac: str) -> str:
    """Validate a colon-separated BMC MAC and return its canonical Vault form."""

    normalized = bmc_mac.strip()
    if not _BMC_MAC_PATTERN.fullmatch(normalized):
        raise SiteVaultError("BMC MAC address is not in XX:XX:XX:XX:XX:XX format")
    return normalized.upper()


def _credential_description(path: str) -> str:
    """Name a credential record from its path, for use in error messages."""

    if path.startswith(SITEWIDE_CREDENTIAL_PATH):
        return "site-wide BMC credentials"
    return "BMC credentials"


def _require_value(record: dict, field: str, path: str) -> str:
    """Return a non-empty string field, or raise a secret-free error."""

    value = record.get(field)
    if not isinstance(value, str) or not value:
        raise SiteVaultError(f"Site Vault returned malformed {_credential_description(path)}")
    return value


def service_account_role_from_jwt(jwt: str) -> str:
    """Extract the Kubernetes service-account name used as the Vault role name."""

    try:
        encoded_payload = jwt.strip().split(".")[1]
        encoded_payload += "=" * (-len(encoded_payload) % 4)
        payload = json.loads(base64.urlsafe_b64decode(encoded_payload))
        role = payload["kubernetes.io"]["serviceaccount"]["name"]
    except (
        IndexError,
        KeyError,
        TypeError,
        ValueError,
        UnicodeDecodeError,
        binascii.Error,
        json.JSONDecodeError,
    ):
        raise SiteVaultError(
            "Kubernetes service-account token does not contain a service-account name"
        ) from None

    if not isinstance(role, str) or not role.strip():
        raise SiteVaultError(
            "Kubernetes service-account token does not contain a service-account name"
        )
    return role.strip()


class SiteVaultClient:
    """Authenticate to in-cluster Site Vault and read per-BMC credentials."""

    def __init__(
        self,
        *,
        address: str | None = None,
        ca_cert_path: str | None = None,
        auth_mount: str | None = None,
        kv_mount: str | None = None,
        role: str | None = None,
        service_account_token_path: str | None = None,
    ) -> None:
        self.address = address or os.getenv("SITE_VAULT_ADDR", DEFAULT_SITE_VAULT_ADDR)
        self.ca_cert_path = ca_cert_path or os.getenv(
            "SITE_VAULT_CACERT", DEFAULT_SITE_VAULT_CACERT
        )
        self.auth_mount = auth_mount or os.getenv(
            "SITE_VAULT_K8S_AUTH_MOUNT", DEFAULT_SITE_VAULT_AUTH_MOUNT
        )
        self.kv_mount = kv_mount or os.getenv(
            "SITE_VAULT_KV_MOUNT", DEFAULT_SITE_VAULT_KV_MOUNT
        )
        self.role = role or os.getenv("SITE_VAULT_K8S_ROLE")
        self.service_account_token_path = service_account_token_path or os.getenv(
            "SITE_VAULT_SERVICE_ACCOUNT_TOKEN_PATH", DEFAULT_SERVICE_ACCOUNT_TOKEN_PATH
        )
        self._client: hvac.Client | None = None

    def __enter__(self) -> SiteVaultClient:
        self.authenticate()
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:
        self.logout()

    def authenticate(self) -> None:
        """Exchange the mounted Kubernetes JWT for a short-lived Vault token."""

        ca_path = Path(self.ca_cert_path)
        token_path = Path(self.service_account_token_path)
        if not ca_path.is_file():
            raise SiteVaultError("Site Vault CA certificate is unavailable")
        if not token_path.is_file():
            raise SiteVaultError("Kubernetes service-account token is unavailable")

        try:
            jwt = token_path.read_text(encoding="utf-8").strip()
        except OSError:
            raise SiteVaultError("Kubernetes service-account token could not be read") from None
        if not jwt:
            raise SiteVaultError("Kubernetes service-account token is empty")

        role = self.role or service_account_role_from_jwt(jwt)
        client = hvac.Client(url=self.address, verify=str(ca_path))
        try:
            client.auth.kubernetes.login(
                role=role,
                jwt=jwt,
                mount_point=self.auth_mount,
            )
        except (hvac_exceptions.VaultError, requests.RequestException, OSError):
            raise SiteVaultError("Site Vault Kubernetes authentication failed") from None
        self._client = client

    def logout(self) -> None:
        """Revoke the current short-lived Vault token, if one was obtained."""

        if self._client is None:
            return
        try:
            # hvac's logout defaults to revoke_token=False, which drops only
            # the local copy and leaves the token live for its whole lease.
            self._client.logout(revoke_token=True)
        except (hvac_exceptions.VaultError, requests.RequestException):
            # Cleanup must not hide the result of the credential read or probe.
            pass
        finally:
            self._client = None

    def credential_path(self, bmc_mac: str) -> str:
        """Return the KV-relative path for a machine's root BMC credentials."""

        return f"machines/bmc/{normalize_bmc_mac(bmc_mac)}/root"

    def credential_api_path(self, bmc_mac: str) -> str:
        """Return the full KV-v2 API policy path for capability checks."""

        return f"{self.kv_mount}/data/{self.credential_path(bmc_mac)}"

    def get_bmc_credentials(self, bmc_mac: str) -> BmcCredentials:
        """Read and validate the root credentials for one host or DPU BMC."""

        path = self.credential_path(bmc_mac)
        record = self._read_credential_record(path)
        return BmcCredentials(
            username=_require_value(record, "username", path),
            password=_require_value(record, "password", path),
        )

    def get_sitewide_bmc_password(self, version: int = 0) -> str:
        """Read and validate one version of the site-wide BMC root password.

        Only the password is meaningful here. Site Explorer pairs it with each
        BMC's own expected/factory username and never reads this record's
        username field.
        """

        path = sitewide_credential_path(version)
        record = self._read_credential_record(path)
        return _require_value(record, "password", path)

    def _read_credential_record(self, path: str) -> dict:
        """Read one credential record, redacting Vault's error detail."""

        description = _credential_description(path)
        client = self._authenticated_client()
        try:
            response = client.secrets.kv.v2.read_secret_version(
                mount_point=self.kv_mount,
                path=path,
            )
        except hvac_exceptions.Forbidden:
            raise SiteVaultError(f"Site Vault denied access to {description}") from None
        except hvac_exceptions.InvalidPath:
            raise SiteVaultCredentialNotFound(
                f"{description} are missing from Site Vault"
            ) from None
        except (hvac_exceptions.VaultError, requests.RequestException):
            raise SiteVaultError(f"Site Vault failed to read {description}") from None

        try:
            record = response["data"]["data"]["UsernamePassword"]
        except (KeyError, TypeError):
            raise SiteVaultError(f"Site Vault returned malformed {description}") from None
        if not isinstance(record, dict):
            raise SiteVaultError(f"Site Vault returned malformed {description}")
        return record

    def issue_client_certificate(
        self, *, pki_mount: str, pki_role: str, common_name: str, ttl: str
    ) -> ClientCertificate:
        """Issue a client certificate from a Vault PKI role.

        The role governs the subject fields an API may authorize on, so the
        role name is a deployment setting rather than something derived here.
        """

        description = f"a client certificate from {pki_mount}/issue/{pki_role}"
        client = self._authenticated_client()
        try:
            response = client.secrets.pki.generate_certificate(
                name=pki_role,
                common_name=common_name,
                extra_params={"ttl": ttl},
                mount_point=pki_mount,
            )
        except hvac_exceptions.Forbidden:
            raise SiteVaultError(f"Site Vault denied {description}") from None
        except hvac_exceptions.InvalidPath:
            raise SiteVaultCredentialNotFound(
                f"Site Vault has no PKI role at {pki_mount}/issue/{pki_role}"
            ) from None
        except (hvac_exceptions.VaultError, requests.RequestException):
            raise SiteVaultError(f"Site Vault failed to issue {description}") from None

        data = response.get("data") if isinstance(response, dict) else None
        if not isinstance(data, dict):
            raise SiteVaultError(f"Site Vault returned a malformed response for {description}")
        certificate = data.get("certificate")
        private_key = data.get("private_key")
        if not isinstance(certificate, str) or not certificate.strip():
            raise SiteVaultError(f"Site Vault returned no certificate for {description}")
        if not isinstance(private_key, str) or not private_key.strip():
            raise SiteVaultError(f"Site Vault returned no private key for {description}")
        return ClientCertificate(certificate=certificate, private_key=private_key)

    def get_capabilities(self, paths: Iterable[str]) -> dict[str, set[str]]:
        """Return this token's capabilities for each requested Vault API path."""

        requested_paths = list(paths)
        try:
            response = self._authenticated_client().sys.get_capabilities(paths=requested_paths)
        except (hvac_exceptions.VaultError, requests.RequestException):
            raise SiteVaultError("Site Vault capability check failed") from None

        data = response.get("data", response)
        capabilities: dict[str, set[str]] = {}
        for path in requested_paths:
            path_capabilities = data.get(path)
            if path_capabilities is None and len(requested_paths) == 1:
                path_capabilities = data.get("capabilities")
            if not isinstance(path_capabilities, list) or not all(
                isinstance(capability, str) for capability in path_capabilities
            ):
                raise SiteVaultError("Site Vault returned a malformed capability response")
            capabilities[path] = set(path_capabilities)
        return capabilities

    def _authenticated_client(self) -> hvac.Client:
        if self._client is None:
            raise SiteVaultError("Site Vault client is not authenticated")
        return self._client
