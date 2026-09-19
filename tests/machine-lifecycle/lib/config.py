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

"""Portable, validated configuration for the machine lifecycle test."""

from __future__ import annotations

import os
import tomllib
from collections.abc import Mapping
from dataclasses import dataclass, field
from enum import StrEnum
from pathlib import Path
from urllib.parse import urlsplit
from typing import Any, ClassVar


_SERVICE_ACCOUNT_TOKEN_PATH = "/var/run/secrets/kubernetes.io/serviceaccount/token"


class ConfigError(ValueError):
    """Raised when test configuration is missing or invalid."""


class LifecycleMode(StrEnum):
    """The portion of the machine lifecycle that the test should run."""

    FULL = "full"
    INGESTION_ONLY = "ingestion-only"
    PROVISION_ONLY = "provision-only"


@dataclass(frozen=True)
class SiteReference:
    """The NICo site against which the test should run."""

    name: str


@dataclass(frozen=True)
class TargetConfig:
    """The machine selected by the operator and its expected inventory."""

    machine_id: str
    expected_dpu_count: int


@dataclass(frozen=True)
class NetworkResourcesConfig:
    """Desired NICo network resources and their required capabilities."""

    vpc_name: str
    vpc_prefix_name: str
    ip_block_name: str
    vpc_prefix_length: int
    cleanup: bool = True
    # False where the VPC and prefix are provisioned externally, making their
    # absence an error rather than something MLT resolves by building its own.
    create_missing: bool = True


@dataclass(frozen=True)
class OAuthConfig:
    """Where the OAuth client credential is read from, and how it is exchanged.

    Every field is a property of the deployment, not of the test. See
    :mod:`lib.credentials` and :mod:`lib.oauth`.
    """

    token_url: str
    scope: str
    # Empty when OAUTH_CREDENTIAL supplies the credential and no Vault is read.
    vault_address: str = ""
    secret_path: str = ""
    vault_namespace: str = ""
    vault_cacert: str = ""
    auth_method: str = "kubernetes"
    auth_mount: str = "kubernetes"
    auth_role: str = ""
    auth_jwt_source: str = f"file:{_SERVICE_ACCOUNT_TOKEN_PATH}"
    secret_engine: str = "kv-v2"
    secret_mount: str = "secrets"
    client_id_field: str = "client_id"
    client_secret_field: str = "secret"


@dataclass(frozen=True)
class ClientCertificateConfig:
    """Where to mint the client certificate the gRPC API requires of callers."""

    vault_pki_mount: str
    vault_pki_role: str
    common_name: str
    ttl: str = "12h"


@dataclass(frozen=True)
class GrpcApiConfig:
    """How to reach the gRPC API, and the identity to present to it.

    This is the API the admin-cli speaks, not the REST API of
    :mod:`lib.nico_rest`.
    """

    url: str
    root_ca_path: str
    client_certificate: ClientCertificateConfig | None = None


@dataclass(frozen=True)
class LifecycleConfig:
    """Options controlling which lifecycle operations the test performs."""

    mode: LifecycleMode = LifecycleMode.FULL
    provision_cycles: int = 1
    skip_factory_reset: bool = False
    test_sitewide_bmc_fallback: bool = False


@dataclass(frozen=True)
class DebugConfig:
    """Optional break-glass access to an instance left behind by a failed run.

    All default off, so a run configures nothing secret unless the operator
    asks for it and no credential ships in this repository.
    """

    ssh_public_key: str | None = None
    enable_console_password: bool = False
    keep_instance: bool = False


@dataclass(frozen=True)
class KubernetesLogWorkload:
    """A Kubernetes deployment whose pod logs should be collected."""

    namespace: str
    deployment: str


@dataclass(frozen=True)
class KubernetesDiagnosticsConfig:
    """Controls bounded Kubernetes pod-log collection."""

    enabled: bool = False
    lookback_minutes: int | None = None
    max_bytes_per_container: int = 5_000_000
    workloads: tuple[KubernetesLogWorkload, ...] = ()


@dataclass(frozen=True)
class DiagnosticsConfig:
    """Controls best-effort snapshots collected after lifecycle timeouts."""

    enabled: bool = True
    output_directory: str = "artifacts/diagnostics"
    kubernetes: KubernetesDiagnosticsConfig = field(default_factory=KubernetesDiagnosticsConfig)


@dataclass(frozen=True)
class OSJanitorConfig:
    """Controls cleanup of stale temporary operating-system definitions."""

    enabled: bool = False
    minimum_age_hours: int = 24
    dry_run: bool = True


@dataclass(frozen=True)
class Config:
    """Complete portable configuration for one test run."""

    __test__: ClassVar[bool] = False

    site: SiteReference
    target: TargetConfig
    resources: NetworkResourcesConfig | None
    lifecycle: LifecycleConfig
    diagnostics: DiagnosticsConfig
    os_janitor: OSJanitorConfig = OSJanitorConfig()
    # Absent when the run supplies its own NICo bearer.
    oauth: OAuthConfig | None = None
    # Safe as a shared default: DebugConfig is frozen, so it cannot be mutated
    # by one run and observed by another.
    debug: DebugConfig = DebugConfig()
    # Absent where the gRPC API needs neither an explicit address nor an identity.
    grpc_api: GrpcApiConfig | None = None

    # Compatibility properties keep the lifecycle implementation focused on
    # adopting one validated input boundary rather than mixing that change with
    # an unrelated rewrite of every call site.
    @property
    def site_under_test(self) -> str:
        return self.site.name

    @property
    def machine_under_test(self) -> str:
        return self.target.machine_id

    @property
    def expected_dpu_count(self) -> int:
        return self.target.expected_dpu_count

    @property
    def provision_cycles(self) -> int:
        return self.lifecycle.provision_cycles

    @property
    def skip_factory_reset(self) -> bool:
        return self.lifecycle.skip_factory_reset

    @property
    def test_sitewide_bmc_fallback(self) -> bool:
        return self.lifecycle.test_sitewide_bmc_fallback


_MISSING = object()
_DEFAULT_CONFIG_PATH = Path("config/config.toml")
_AUTH_METHODS = frozenset({"kubernetes", "jwt", "token"})
_SECRET_ENGINES = frozenset({"kv-v2", "raw"})

# Matched by exact name rather than by an "OAUTH_" prefix: that prefix is common
# enough in other tooling that a stray variable would look like a half-written
# MLT section and fail on a missing token_url.
_OAUTH_VARIABLES = frozenset(
    {
        "OAUTH_TOKEN_URL",
        "OAUTH_SCOPE",
        "OAUTH_CREDENTIAL",
        "OAUTH_VAULT_ADDR",
        "OAUTH_VAULT_NAMESPACE",
        "OAUTH_VAULT_CACERT",
        "OAUTH_AUTH_METHOD",
        "OAUTH_AUTH_MOUNT",
        "OAUTH_AUTH_ROLE",
        "OAUTH_AUTH_JWT_SOURCE",
        "OAUTH_SECRET_ENGINE",
        "OAUTH_SECRET_MOUNT",
        "OAUTH_SECRET_PATH",
        "OAUTH_CLIENT_ID_FIELD",
        "OAUTH_CLIENT_SECRET_FIELD",
    }
)
_TOP_LEVEL_KEYS = frozenset(
    {
        "site",
        "target",
        "resources",
        "lifecycle",
        "debug",
        "diagnostics",
        "os_janitor",
        "grpc_api",
        "oauth",
    }
)
_RESOURCE_IDENTITY_KEYS = frozenset(
    {"vpc_name", "vpc_prefix_name", "ip_block_name", "vpc_prefix_length"}
)
_RESOURCE_IDENTITY_ENVIRONMENT_VARIABLES = frozenset(
    {
        "MLT_VPC_NAME",
        "MLT_VPC_PREFIX_NAME",
        "MLT_IP_BLOCK_NAME",
        "MLT_VPC_PREFIX_LENGTH",
    }
)
_CLIENT_CERTIFICATE_KEYS = frozenset(
    {"vault_pki_mount", "vault_pki_role", "common_name", "ttl"}
)
_KUBERNETES_DIAGNOSTICS_KEYS = frozenset(
    {"enabled", "lookback_minutes", "max_bytes_per_container", "workloads"}
)
_KUBERNETES_WORKLOAD_KEYS = frozenset({"namespace", "deployment"})
_SECTION_KEYS = {
    "site": frozenset({"name"}),
    "target": frozenset({"machine_id", "expected_dpu_count"}),
    "resources": frozenset(
        {
            "vpc_name",
            "vpc_prefix_name",
            "ip_block_name",
            "vpc_prefix_length",
            "cleanup",
            "create_missing",
        }
    ),
    "lifecycle": frozenset(
        {"mode", "provision_cycles", "skip_factory_reset", "test_sitewide_bmc_fallback"}
    ),
    "oauth": frozenset(
        {
            "token_url",
            "scope",
            "vault_address",
            "vault_namespace",
            "vault_cacert",
            "auth_method",
            "auth_mount",
            "auth_role",
            "auth_jwt_source",
            "secret_engine",
            "secret_mount",
            "secret_path",
            "client_id_field",
            "client_secret_field",
        }
    ),
    "debug": frozenset(
        {"ssh_public_key", "enable_console_password", "keep_instance"}
    ),
    "diagnostics": frozenset({"enabled", "output_directory", "kubernetes"}),
    "os_janitor": frozenset({"enabled", "minimum_age_hours", "dry_run"}),
    "grpc_api": frozenset({"url", "root_ca_path", "client_certificate"}),
}


def _is_set(
    section: Mapping[str, Any],
    key: str,
    environ: Mapping[str, str],
    environment_variable: str,
) -> bool:
    """Whether a setting was written down, as opposed to taking its default."""

    return key in section or environment_variable in environ


def _reject_unknown_keys(data: Mapping[str, Any], allowed: frozenset[str], location: str) -> None:
    unknown = sorted(set(data) - allowed)
    if unknown:
        rendered = ", ".join(repr(key) for key in unknown)
        raise ConfigError(f"Unknown configuration key(s) in {location}: {rendered}")


def _section(data: Mapping[str, Any], name: str) -> Mapping[str, Any]:
    value = data.get(name, {})
    if not isinstance(value, Mapping):
        raise ConfigError(f"Configuration section [{name}] must be a table")
    _reject_unknown_keys(value, _SECTION_KEYS[name], f"[{name}]")
    return value


def _load_toml(path: str | Path | None) -> Mapping[str, Any]:
    if path is None:
        return {}

    config_path = Path(path)
    try:
        with config_path.open("rb") as config_file:
            data = tomllib.load(config_file)
    except FileNotFoundError as error:
        raise ConfigError(f"Configuration file not found: {config_path}") from error
    except OSError as error:
        raise ConfigError(f"Could not read configuration file {config_path}: {error}") from error
    except tomllib.TOMLDecodeError as error:
        raise ConfigError(f"Invalid TOML in configuration file {config_path}: {error}") from error

    _reject_unknown_keys(data, _TOP_LEVEL_KEYS, "the top level")
    return data


def _value(
    section: Mapping[str, Any],
    key: str,
    environ: Mapping[str, str],
    environment_variable: str,
    default: Any = _MISSING,
) -> Any:
    if environment_variable in environ:
        return environ[environment_variable]
    if key in section:
        return section[key]
    if default is not _MISSING:
        return default
    raise ConfigError(
        f"TOML key {key!r} or ${environment_variable} must be provided"
    )


def _non_empty_string(value: Any, field: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ConfigError(f"{field} must be a non-empty string")
    return value.strip()


def _optional_string(value: Any, field: str) -> str | None:
    """Accept an absent value, rejecting one that is present but unusable."""
    if value is None:
        return None
    if not isinstance(value, str):
        raise ConfigError(f"{field} must be a string")
    stripped = value.strip()
    return stripped or None


def _positive_integer(value: Any, field: str) -> int:
    if isinstance(value, str):
        try:
            value = int(value)
        except ValueError as error:
            raise ConfigError(f"{field} must be a positive integer") from error
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
        raise ConfigError(f"{field} must be a positive integer")
    return value


def _boolean(value: Any, field: str) -> bool:
    if isinstance(value, bool):
        return value
    if isinstance(value, str) and value.strip().lower() in {"true", "false"}:
        return value.strip().lower() == "true"
    raise ConfigError(f"{field} must be true or false")


def _lifecycle_mode(
    section: Mapping[str, Any], environ: Mapping[str, str]
) -> LifecycleMode:
    configured_mode = _value(
        section,
        "mode",
        environ,
        "MLT_MODE",
        default=LifecycleMode.FULL.value,
    )
    if not isinstance(configured_mode, str):
        raise ConfigError("lifecycle.mode must be a string")
    try:
        mode = LifecycleMode(configured_mode.strip().lower())
    except ValueError as error:
        supported = ", ".join(profile.value for profile in LifecycleMode)
        raise ConfigError(f"lifecycle.mode must be one of: {supported}") from error
    return mode


def _vpc_prefix_name(
    section: Mapping[str, Any],
    environ: Mapping[str, str],
) -> str | None:
    configured_name = _value(
        section,
        "vpc_prefix_name",
        environ,
        "VPC_PREFIX_NAME",
        default=None,
    )
    if configured_name is None:
        return None
    return _non_empty_string(configured_name, "resources.vpc_prefix_name")


def _choice(value: Any, allowed: frozenset[str], field: str) -> str:
    """Return a validated member of a small closed vocabulary."""

    text = _non_empty_string(value, field)
    if text not in allowed:
        supported = ", ".join(sorted(allowed))
        raise ConfigError(f"{field} must be one of: {supported}")
    return text


def _require_secure_url(value: Any, field: str) -> str:
    """Return a URL, rejecting plaintext HTTP to anywhere but the loopback.

    The credential crosses both the Vault connection and the token request.
    Loopback is allowed for local port-forwards.
    """

    url = _non_empty_string(value, field)
    parsed = urlsplit(url)
    if not parsed.hostname:
        raise ConfigError(f"{field} must include a host; got {url!r}")
    if parsed.scheme == "https":
        return url
    if parsed.scheme == "http" and parsed.hostname in {"localhost", "127.0.0.1", "::1"}:
        return url
    raise ConfigError(
        f"{field} must use https, or http only via localhost; got {url!r}"
    )


def _jwt_source(value: Any, field: str) -> str:
    """Return a validated ``env:<NAME>`` or ``file:<PATH>`` JWT source."""

    source = _non_empty_string(value, field)
    scheme, separator, location = source.partition(":")
    if not separator or not location.strip() or scheme not in {"env", "file"}:
        raise ConfigError(f"{field} must be 'env:<NAME>' or 'file:<PATH>'")
    return source


def _client_credential_pair(value: str, field: str) -> str:
    """Return a credential that is actually a ``client_id:client_secret`` pair."""

    client_id, separator, client_secret = value.partition(":")
    if not separator or not client_id.strip() or not client_secret.strip():
        # Never echo the value; it is the credential.
        raise ConfigError(f"{field} must be 'client_id:client_secret'")
    return value


def _oauth_configured(data: Mapping[str, Any], environ: Mapping[str, str]) -> bool:
    """Whether this run describes how to obtain a NICo bearer token."""

    if "oauth" in data:
        return True
    return not _OAUTH_VARIABLES.isdisjoint(environ)


def _oauth_config(
    data: Mapping[str, Any], environ: Mapping[str, str]
) -> OAuthConfig | None:
    """Build the token-exchange settings, or None when none are configured."""

    if not _oauth_configured(data, environ):
        return None

    oauth = _section(data, "oauth")

    def setting(key: str, variable: str, default: Any = _MISSING) -> Any:
        return _value(oauth, key, environ, variable, default=default)

    token_url = _require_secure_url(
        setting("token_url", "OAUTH_TOKEN_URL"), "oauth.token_url"
    )
    scope = _non_empty_string(setting("scope", "OAUTH_SCOPE"), "oauth.scope")

    # A credential supplied directly needs only the token endpoint.
    direct_credential = environ.get("OAUTH_CREDENTIAL", "").strip()
    if direct_credential:
        _client_credential_pair(direct_credential, "$OAUTH_CREDENTIAL")
        return OAuthConfig(token_url=token_url, scope=scope)

    auth_method = _choice(
        setting("auth_method", "OAUTH_AUTH_METHOD", default="kubernetes"),
        _AUTH_METHODS,
        "oauth.auth_method",
    )
    secret_engine = _choice(
        setting("secret_engine", "OAUTH_SECRET_ENGINE", default="kv-v2"),
        _SECRET_ENGINES,
        "oauth.secret_engine",
    )

    auth_role = _optional_string(
        setting("auth_role", "OAUTH_AUTH_ROLE", default=""), "oauth.auth_role"
    ) or ""
    # Only a Kubernetes mount can derive its role from the token itself.
    if auth_method == "jwt" and not auth_role:
        raise ConfigError(
            "oauth.auth_role must be set when oauth.auth_method is 'jwt'"
        )
    if secret_engine == "raw" and _is_set(oauth, "secret_mount", environ, "OAUTH_SECRET_MOUNT"):
        raise ConfigError(
            "oauth.secret_mount applies only to secret_engine 'kv-v2'; a raw "
            "read takes the full path in oauth.secret_path"
        )

    return OAuthConfig(
        token_url=token_url,
        scope=scope,
        vault_address=_require_secure_url(
            setting("vault_address", "OAUTH_VAULT_ADDR"), "oauth.vault_address"
        ),
        secret_path=_non_empty_string(
            setting("secret_path", "OAUTH_SECRET_PATH"), "oauth.secret_path"
        ),
        vault_namespace=_optional_string(
            setting("vault_namespace", "OAUTH_VAULT_NAMESPACE", default=""),
            "oauth.vault_namespace",
        ) or "",
        vault_cacert=_optional_string(
            setting("vault_cacert", "OAUTH_VAULT_CACERT", default=""),
            "oauth.vault_cacert",
        ) or "",
        auth_method=auth_method,
        auth_mount=_non_empty_string(
            setting("auth_mount", "OAUTH_AUTH_MOUNT", default="kubernetes"),
            "oauth.auth_mount",
        ),
        auth_role=auth_role,
        auth_jwt_source=_jwt_source(
            setting(
                "auth_jwt_source",
                "OAUTH_AUTH_JWT_SOURCE",
                default=f"file:{_SERVICE_ACCOUNT_TOKEN_PATH}",
            ),
            "oauth.auth_jwt_source",
        ),
        secret_engine=secret_engine,
        secret_mount=_non_empty_string(
            setting("secret_mount", "OAUTH_SECRET_MOUNT", default="secrets"),
            "oauth.secret_mount",
        ),
        client_id_field=_non_empty_string(
            setting("client_id_field", "OAUTH_CLIENT_ID_FIELD", default="client_id"),
            "oauth.client_id_field",
        ),
        client_secret_field=_non_empty_string(
            setting(
                "client_secret_field", "OAUTH_CLIENT_SECRET_FIELD", default="secret"
            ),
            "oauth.client_secret_field",
        ),
    )


def _subsection(
    section: Mapping[str, Any], parent: str, name: str, allowed: frozenset[str]
) -> Mapping[str, Any]:
    """Validate and return a nested table, which may be absent."""

    value = section.get(name, {})
    if not isinstance(value, Mapping):
        raise ConfigError(f"Configuration section [{parent}.{name}] must be a table")
    _reject_unknown_keys(value, allowed, f"[{parent}.{name}]")
    return value


def _any_variable_with_prefix(environ: Mapping[str, str], prefix: str) -> bool:
    """Whether the environment overrides any key belonging to a section."""

    return any(name.startswith(prefix) for name in environ)


def _client_certificate_config(
    grpc_api: Mapping[str, Any], environ: Mapping[str, str]
) -> ClientCertificateConfig | None:
    """Build the certificate-minting settings, or None when none are given."""

    certificate = _subsection(
        grpc_api, "grpc_api", "client_certificate", _CLIENT_CERTIFICATE_KEYS
    )
    # Presence of the table, not its contents: written down but empty is an
    # incomplete configuration, not an absent one.
    configured = "client_certificate" in grpc_api or _any_variable_with_prefix(
        environ, "GRPC_API_CLIENT_CERT_"
    )
    if not configured:
        return None

    def setting(key: str, variable: str, default: Any = _MISSING) -> Any:
        return _value(certificate, key, environ, variable, default=default)

    return ClientCertificateConfig(
        vault_pki_mount=_non_empty_string(
            setting("vault_pki_mount", "GRPC_API_CLIENT_CERT_VAULT_PKI_MOUNT"),
            "grpc_api.client_certificate.vault_pki_mount",
        ),
        vault_pki_role=_non_empty_string(
            setting("vault_pki_role", "GRPC_API_CLIENT_CERT_VAULT_PKI_ROLE"),
            "grpc_api.client_certificate.vault_pki_role",
        ),
        common_name=_non_empty_string(
            setting("common_name", "GRPC_API_CLIENT_CERT_COMMON_NAME"),
            "grpc_api.client_certificate.common_name",
        ),
        ttl=_non_empty_string(
            setting("ttl", "GRPC_API_CLIENT_CERT_TTL", default="12h"),
            "grpc_api.client_certificate.ttl",
        ),
    )


def _grpc_api_config(
    data: Mapping[str, Any], environ: Mapping[str, str]
) -> GrpcApiConfig | None:
    """Build the gRPC API address and identity, or None when none are given."""

    if "grpc_api" not in data and not _any_variable_with_prefix(environ, "GRPC_API_"):
        return None

    grpc_api = _section(data, "grpc_api")
    # The address is required, not defaulted: on a name the server certificate
    # does not cover, the client retries rather than failing, so an omission
    # presents as a hang.
    return GrpcApiConfig(
        url=_non_empty_string(
            _value(grpc_api, "url", environ, "GRPC_API_URL"), "grpc_api.url"
        ),
        root_ca_path=_non_empty_string(
            _value(grpc_api, "root_ca_path", environ, "GRPC_API_ROOT_CA_PATH"),
            "grpc_api.root_ca_path",
        ),
        client_certificate=_client_certificate_config(grpc_api, environ),
    )


def _kubernetes_log_deployments(
    kubernetes: Mapping[str, Any], environ: Mapping[str, str]
) -> tuple[KubernetesLogWorkload, ...]:
    """Load deployment references from TOML or a comma-separated override."""
    variable = "MLT_DIAGNOSTICS_KUBERNETES_DEPLOYMENTS"
    if variable in environ:
        configured = environ[variable]
        if not configured.strip():
            raise ConfigError(f"${variable} must contain namespace/deployment entries")
        raw_workloads: list[Mapping[str, Any]] = []
        for reference in configured.split(","):
            namespace, separator, deployment = reference.strip().partition("/")
            if not separator or not namespace or not deployment or "/" in deployment:
                raise ConfigError(f"${variable} entries must use the namespace/deployment format")
            raw_workloads.append({"namespace": namespace, "deployment": deployment})
    else:
        configured_workloads = kubernetes.get("workloads", [])
        if not isinstance(configured_workloads, list):
            raise ConfigError("diagnostics.kubernetes.workloads must be an array of tables")
        raw_workloads = configured_workloads

    workloads = []
    for index, workload in enumerate(raw_workloads):
        location = f"diagnostics.kubernetes.workloads[{index}]"
        if not isinstance(workload, Mapping):
            raise ConfigError(f"{location} must be a table")
        _reject_unknown_keys(workload, _KUBERNETES_WORKLOAD_KEYS, location)
        parsed = KubernetesLogWorkload(
            namespace=_non_empty_string(workload.get("namespace"), f"{location}.namespace"),
            deployment=_non_empty_string(workload.get("deployment"), f"{location}.deployment"),
        )
        if parsed in workloads:
            raise ConfigError(
                f"Duplicate Kubernetes diagnostics workload {parsed.namespace}/{parsed.deployment}"
            )
        workloads.append(parsed)
    return tuple(workloads)


def _kubernetes_diagnostics_config(
    diagnostics: Mapping[str, Any], environ: Mapping[str, str]
) -> KubernetesDiagnosticsConfig:
    kubernetes = _subsection(
        diagnostics,
        "diagnostics",
        "kubernetes",
        _KUBERNETES_DIAGNOSTICS_KEYS,
    )
    enabled = _boolean(
        _value(
            kubernetes,
            "enabled",
            environ,
            "MLT_DIAGNOSTICS_KUBERNETES_ENABLED",
            default=False,
        ),
        "diagnostics.kubernetes.enabled",
    )
    if not enabled:
        return KubernetesDiagnosticsConfig()

    configured_lookback = _value(
        kubernetes,
        "lookback_minutes",
        environ,
        "MLT_DIAGNOSTICS_KUBERNETES_LOOKBACK_MINUTES",
        default=None,
    )
    config = KubernetesDiagnosticsConfig(
        enabled=enabled,
        lookback_minutes=(
            None
            if configured_lookback is None
            else _positive_integer(
                configured_lookback,
                "diagnostics.kubernetes.lookback_minutes",
            )
        ),
        max_bytes_per_container=_positive_integer(
            _value(
                kubernetes,
                "max_bytes_per_container",
                environ,
                "MLT_DIAGNOSTICS_KUBERNETES_MAX_BYTES_PER_CONTAINER",
                default=5_000_000,
            ),
            "diagnostics.kubernetes.max_bytes_per_container",
        ),
        workloads=_kubernetes_log_deployments(kubernetes, environ),
    )
    if config.enabled and not config.workloads:
        raise ConfigError(
            "diagnostics.kubernetes.workloads must contain at least one deployment "
            "when Kubernetes log collection is enabled"
        )
    return config


def _network_resources_config(
    resources: Mapping[str, Any],
    environ: Mapping[str, str],
    *,
    required: bool,
    configured: bool,
) -> NetworkResourcesConfig | None:
    """Build network settings when provisioning needs or receives them."""

    if not required and not configured:
        return None

    vpc_prefix_length = _value(
        resources,
        "vpc_prefix_length",
        environ,
        "MLT_VPC_PREFIX_LENGTH",
    )
    if isinstance(vpc_prefix_length, str):
        try:
            vpc_prefix_length = int(vpc_prefix_length)
        except ValueError as error:
            raise ConfigError(
                "resources.vpc_prefix_length must be an integer from 8 through 31"
            ) from error
    if (
        isinstance(vpc_prefix_length, bool)
        or not isinstance(vpc_prefix_length, int)
        or not 8 <= vpc_prefix_length <= 31
    ):
        raise ConfigError("resources.vpc_prefix_length must be an integer from 8 through 31")

    return NetworkResourcesConfig(
        vpc_name=_non_empty_string(
            _value(resources, "vpc_name", environ, "MLT_VPC_NAME"),
            "resources.vpc_name",
        ),
        vpc_prefix_name=_non_empty_string(
            _value(
                resources,
                "vpc_prefix_name",
                environ,
                "MLT_VPC_PREFIX_NAME",
            ),
            "resources.vpc_prefix_name",
        ),
        ip_block_name=_non_empty_string(
            _value(
                resources,
                "ip_block_name",
                environ,
                "MLT_IP_BLOCK_NAME",
            ),
            "resources.ip_block_name",
        ),
        vpc_prefix_length=vpc_prefix_length,
        cleanup=_boolean(
            _value(
                resources,
                "cleanup",
                environ,
                "MLT_CLEANUP_NETWORK_RESOURCES",
                default=True,
            ),
            "resources.cleanup",
        ),
        create_missing=_boolean(
            _value(
                resources,
                "create_missing",
                environ,
                "MLT_CREATE_MISSING_NETWORK_RESOURCES",
                default=True,
            ),
            "resources.create_missing",
        ),
    )


def load_config(
    *,
    config_path: str | Path | None = None,
    environ: Mapping[str, str] | None = None,
) -> Config:
    """Load and validate test configuration from TOML and environment.

    Environment variables override TOML values. If ``config_path`` is omitted,
    ``MLT_CONFIG`` selects the file, followed by ``config/config.toml`` when it
    exists. A file is optional when all required values are supplied through
    the environment.
    """

    environment = os.environ if environ is None else environ
    selected_path = config_path
    if selected_path is None:
        selected_path = environment.get("MLT_CONFIG") or None
        if selected_path is None and _DEFAULT_CONFIG_PATH.is_file():
            selected_path = _DEFAULT_CONFIG_PATH
    data = _load_toml(selected_path)

    site = _section(data, "site")
    target = _section(data, "target")
    resources = _section(data, "resources")
    lifecycle = _section(data, "lifecycle")
    debug = _section(data, "debug")
    diagnostics = _section(data, "diagnostics")
    os_janitor = _section(data, "os_janitor")

    mode = _lifecycle_mode(lifecycle, environment)
    test_sitewide_bmc_fallback = _boolean(
        _value(
            lifecycle,
            "test_sitewide_bmc_fallback",
            environment,
            "TEST_SITEWIDE_BMC_FALLBACK",
            default=False,
        ),
        "lifecycle.test_sitewide_bmc_fallback",
    )
    if test_sitewide_bmc_fallback and mode is LifecycleMode.PROVISION_ONLY:
        raise ConfigError(
            "lifecycle.test_sitewide_bmc_fallback cannot be true when "
            "lifecycle.mode is provision-only"
        )

    keep_instance = _boolean(
        _value(
            debug,
            "keep_instance",
            environment,
            "ENABLE_MLT_DEBUG_KEEP_INSTANCE",
            default=False,
        ),
        "debug.keep_instance",
    )
    provision_cycles = _positive_integer(
        _value(
            lifecycle,
            "provision_cycles",
            environment,
            "PROVISION_CYCLES",
            default=1,
        ),
        "lifecycle.provision_cycles",
    )
    if keep_instance and provision_cycles > 1:
        raise ConfigError(
            "debug.keep_instance cannot be true when "
            "lifecycle.provision_cycles is greater than 1: every cycle after "
            "the first would strand another instance"
        )

    diagnostics_enabled = _boolean(
        _value(
            diagnostics,
            "enabled",
            environment,
            "MLT_DIAGNOSTICS_ENABLED",
            default=True,
        ),
        "diagnostics.enabled",
    )
    kubernetes_diagnostics = (
        _kubernetes_diagnostics_config(diagnostics, environment)
        if diagnostics_enabled
        else KubernetesDiagnosticsConfig()
    )

    resources_config = _network_resources_config(
        resources,
        environment,
        required=mode is not LifecycleMode.INGESTION_ONLY,
        configured=any(name in resources for name in _RESOURCE_IDENTITY_KEYS)
        or any(
            name in environment
            for name in _RESOURCE_IDENTITY_ENVIRONMENT_VARIABLES
        ),
    )

    return Config(
        site=SiteReference(
            name=_non_empty_string(
                _value(site, "name", environment, "SITE_UNDER_TEST"),
                "site.name",
            )
        ),
        target=TargetConfig(
            machine_id=_non_empty_string(
                _value(target, "machine_id", environment, "MACHINE_UNDER_TEST"),
                "target.machine_id",
            ),
            expected_dpu_count=_positive_integer(
                _value(target, "expected_dpu_count", environment, "DPU_COUNT"),
                "target.expected_dpu_count",
            ),
        ),
        resources=resources_config,
        lifecycle=LifecycleConfig(
            mode=mode,
            provision_cycles=provision_cycles,
            skip_factory_reset=_boolean(
                _value(
                    lifecycle,
                    "skip_factory_reset",
                    environment,
                    "SKIP_FACTORY_RESET",
                    default=False,
                ),
                "lifecycle.skip_factory_reset",
            ),
            test_sitewide_bmc_fallback=test_sitewide_bmc_fallback,
        ),
        diagnostics=DiagnosticsConfig(
            enabled=diagnostics_enabled,
            output_directory=_non_empty_string(
                _value(
                    diagnostics,
                    "output_directory",
                    environment,
                    "MLT_DIAGNOSTICS_OUTPUT_DIRECTORY",
                    default="artifacts/diagnostics",
                ),
                "diagnostics.output_directory",
            ),
            kubernetes=kubernetes_diagnostics,
        ),
        os_janitor=OSJanitorConfig(
            enabled=_boolean(
                _value(
                    os_janitor,
                    "enabled",
                    environment,
                    "MLT_OS_JANITOR_ENABLED",
                    default=False,
                ),
                "os_janitor.enabled",
            ),
            minimum_age_hours=_positive_integer(
                _value(
                    os_janitor,
                    "minimum_age_hours",
                    environment,
                    "MLT_OS_JANITOR_MINIMUM_AGE_HOURS",
                    default=24,
                ),
                "os_janitor.minimum_age_hours",
            ),
            dry_run=_boolean(
                _value(
                    os_janitor,
                    "dry_run",
                    environment,
                    "MLT_OS_JANITOR_DRY_RUN",
                    default=True,
                ),
                "os_janitor.dry_run",
            ),
        ),
        oauth=_oauth_config(data, environment),
        debug=DebugConfig(
            ssh_public_key=_optional_string(
                _value(
                    debug,
                    "ssh_public_key",
                    environment,
                    "MLT_DEBUG_SSH_PUBLIC_KEY",
                    default=None,
                ),
                "debug.ssh_public_key",
            ),
            enable_console_password=_boolean(
                _value(
                    debug,
                    "enable_console_password",
                    environment,
                    "ENABLE_MLT_DEBUG_CONSOLE_PASSWORD",
                    default=False,
                ),
                "debug.enable_console_password",
            ),
            keep_instance=keep_instance,
        ),
        grpc_api=_grpc_api_config(data, environment),
    )
