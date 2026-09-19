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

import re
from pathlib import Path

import pytest

from lib import config as config_module
from lib.config import ConfigError, LifecycleMode, load_config
from tests.lifecycle import machine_lifecycle_test as lifecycle


REQUIRED_ENVIRONMENT = {
    "SITE_UNDER_TEST": "test-site",
    "MACHINE_UNDER_TEST": "fm100ht-test-machine",
    "DPU_COUNT": "2",
    "MLT_VPC_NAME": "test-vpc",
    "MLT_VPC_PREFIX_NAME": "test-prefix",
    "MLT_IP_BLOCK_NAME": "test-ip-block",
    "MLT_VPC_PREFIX_LENGTH": "29",
}


def _write_config(path: Path, contents: str) -> Path:
    path.write_text(contents, encoding="utf-8")
    return path


@pytest.fixture(autouse=True)
def isolate_default_config_path(monkeypatch, tmp_path):
    """Prevent a developer's local config from affecting unit tests."""
    monkeypatch.setattr(
        config_module,
        "_DEFAULT_CONFIG_PATH",
        tmp_path / "missing-config.toml",
    )


def test_loads_complete_toml_configuration(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[site]
name = "toml-site"

[target]
machine_id = "fm100ht-from-toml"
expected_dpu_count = 4

[resources]
vpc_name = "toml-vpc"
vpc_prefix_name = "toml-prefix"
ip_block_name = "toml-ip-block"
vpc_prefix_length = 28
cleanup = false

[lifecycle]
mode = "ingestion-only"
provision_cycles = 3
skip_factory_reset = true
test_sitewide_bmc_fallback = false

[diagnostics]
enabled = false
output_directory = "custom-diagnostics"

[os_janitor]
enabled = true
minimum_age_hours = 48
dry_run = false
""",
    )

    loaded = load_config(config_path=config_path, environ={})

    assert loaded.site.name == "toml-site"
    assert loaded.target.machine_id == "fm100ht-from-toml"
    assert loaded.target.expected_dpu_count == 4
    assert loaded.resources.vpc_name == "toml-vpc"
    assert loaded.resources.vpc_prefix_name == "toml-prefix"
    assert loaded.resources.ip_block_name == "toml-ip-block"
    assert loaded.resources.vpc_prefix_length == 28
    assert loaded.resources.cleanup is False
    assert loaded.lifecycle.mode is LifecycleMode.INGESTION_ONLY
    assert loaded.lifecycle.provision_cycles == 3
    assert loaded.lifecycle.skip_factory_reset is True
    assert loaded.lifecycle.test_sitewide_bmc_fallback is False
    assert loaded.diagnostics.enabled is False
    assert loaded.diagnostics.output_directory == "custom-diagnostics"
    assert loaded.os_janitor.enabled is True
    assert loaded.os_janitor.minimum_age_hours == 48
    assert loaded.os_janitor.dry_run is False


def test_loads_default_config_when_present(tmp_path, monkeypatch):
    default_path = _write_config(
        tmp_path / "config.toml",
        """
[site]
name = "default-site"
[target]
machine_id = "fm100ht-from-default"
expected_dpu_count = 2
[resources]
vpc_name = "default-vpc"
vpc_prefix_name = "default-prefix"
ip_block_name = "default-ip-block"
vpc_prefix_length = 29
""",
    )
    monkeypatch.setattr(config_module, "_DEFAULT_CONFIG_PATH", default_path)

    loaded = load_config(environ={})

    assert loaded.site.name == "default-site"
    assert loaded.target.machine_id == "fm100ht-from-default"
    assert loaded.resources.vpc_name == "default-vpc"


def test_loads_environment_only_configuration():
    loaded = load_config(environ=REQUIRED_ENVIRONMENT)

    assert loaded.site_under_test == "test-site"
    assert loaded.machine_under_test == "fm100ht-test-machine"
    assert loaded.expected_dpu_count == 2
    assert loaded.resources.vpc_name == "test-vpc"
    assert loaded.resources.vpc_prefix_name == "test-prefix"
    assert loaded.resources.ip_block_name == "test-ip-block"
    assert loaded.resources.vpc_prefix_length == 29
    assert loaded.resources.cleanup is True
    assert loaded.lifecycle.mode is LifecycleMode.FULL
    assert loaded.provision_cycles == 1
    assert loaded.skip_factory_reset is False
    assert loaded.diagnostics.enabled is True
    assert loaded.diagnostics.output_directory == "artifacts/diagnostics"
    assert loaded.diagnostics.kubernetes.lookback_minutes is None
    assert loaded.os_janitor.enabled is False
    assert loaded.os_janitor.minimum_age_hours == 24
    assert loaded.os_janitor.dry_run is True


def test_ingestion_only_does_not_require_network_resources():
    loaded = load_config(
        environ={
            "SITE_UNDER_TEST": "test-site",
            "MACHINE_UNDER_TEST": "fm100ht-test-machine",
            "DPU_COUNT": "2",
            "MLT_MODE": "ingestion-only",
        }
    )

    assert loaded.lifecycle.mode is LifecycleMode.INGESTION_ONLY
    assert loaded.resources is None


@pytest.mark.parametrize("cleanup_source", ["environment", "toml"])
def test_ingestion_only_cleanup_alone_does_not_configure_network_resources(
    cleanup_source, tmp_path
):
    environment = {
        "SITE_UNDER_TEST": "test-site",
        "MACHINE_UNDER_TEST": "fm100ht-test-machine",
        "DPU_COUNT": "2",
        "MLT_MODE": "ingestion-only",
    }
    config_path = None
    if cleanup_source == "environment":
        environment["MLT_CLEANUP_NETWORK_RESOURCES"] = "false"
    else:
        config_path = _write_config(
            tmp_path / "mlt.toml",
            "[resources]\ncleanup = false\n",
        )

    loaded = load_config(config_path=config_path, environ=environment)

    assert loaded.resources is None


@pytest.mark.parametrize("create_missing_source", ["environment", "toml"])
def test_ingestion_only_create_missing_alone_does_not_configure_network_resources(
    create_missing_source, tmp_path
):
    environment = {
        "SITE_UNDER_TEST": "test-site",
        "MACHINE_UNDER_TEST": "fm100ht-test-machine",
        "DPU_COUNT": "2",
        "MLT_MODE": "ingestion-only",
    }
    config_path = None
    if create_missing_source == "environment":
        environment["MLT_CREATE_MISSING_NETWORK_RESOURCES"] = "false"
    else:
        config_path = _write_config(
            tmp_path / "mlt.toml",
            "[resources]\ncreate_missing = false\n",
        )

    loaded = load_config(config_path=config_path, environ=environment)

    assert loaded.resources is None


def test_create_missing_defaults_to_true():
    environment = {
        "SITE_UNDER_TEST": "test-site",
        "MACHINE_UNDER_TEST": "fm100ht-test-machine",
        "DPU_COUNT": "2",
        "MLT_MODE": "full",
        "MLT_VPC_NAME": "mlt-vpc",
        "MLT_VPC_PREFIX_NAME": "mlt-prefix",
        "MLT_IP_BLOCK_NAME": "tenant-ip-block",
        "MLT_VPC_PREFIX_LENGTH": "30",
    }

    assert load_config(environ=environment).resources.create_missing is True

    environment["MLT_CREATE_MISSING_NETWORK_RESOURCES"] = "false"

    assert load_config(environ=environment).resources.create_missing is False


def test_ingestion_only_validates_partial_network_configuration():
    environment = {
        "SITE_UNDER_TEST": "test-site",
        "MACHINE_UNDER_TEST": "fm100ht-test-machine",
        "DPU_COUNT": "2",
        "MLT_MODE": "ingestion-only",
        "MLT_VPC_NAME": "test-vpc",
    }

    with pytest.raises(ConfigError, match=r"\$MLT_VPC_PREFIX_LENGTH"):
        load_config(environ=environment)


def test_provision_only_requires_network_resources():
    environment = {
        "SITE_UNDER_TEST": "test-site",
        "MACHINE_UNDER_TEST": "fm100ht-test-machine",
        "DPU_COUNT": "2",
        "MLT_MODE": "provision-only",
    }

    with pytest.raises(ConfigError, match=r"\$MLT_VPC_PREFIX_LENGTH"):
        load_config(environ=environment)


def test_environment_overrides_toml(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[site]
name = "toml-site"
[target]
machine_id = "toml-machine"
expected_dpu_count = 1
[resources]
vpc_name = "toml-vpc"
vpc_prefix_name = "toml-prefix"
ip_block_name = "toml-ip-block"
vpc_prefix_length = 28
cleanup = true
[lifecycle]
mode = "ingestion-only"
provision_cycles = 2
skip_factory_reset = false

[os_janitor]
enabled = false
minimum_age_hours = 12
dry_run = true
""",
    )
    environment = {
        **REQUIRED_ENVIRONMENT,
        "MLT_CONFIG": str(config_path),
        "MLT_MODE": "provision-only",
        "PROVISION_CYCLES": "5",
        "SKIP_FACTORY_RESET": "true",
        "MLT_DIAGNOSTICS_ENABLED": "false",
        "MLT_DIAGNOSTICS_OUTPUT_DIRECTORY": "env-diagnostics",
        "MLT_CLEANUP_NETWORK_RESOURCES": "false",
        "MLT_OS_JANITOR_ENABLED": "true",
        "MLT_OS_JANITOR_MINIMUM_AGE_HOURS": "36",
        "MLT_OS_JANITOR_DRY_RUN": "false",
    }

    loaded = load_config(environ=environment)

    assert loaded.site.name == "test-site"
    assert loaded.target.machine_id == "fm100ht-test-machine"
    assert loaded.target.expected_dpu_count == 2
    assert loaded.resources.vpc_name == "test-vpc"
    assert loaded.resources.vpc_prefix_name == "test-prefix"
    assert loaded.resources.ip_block_name == "test-ip-block"
    assert loaded.resources.vpc_prefix_length == 29
    assert loaded.resources.cleanup is False
    assert loaded.lifecycle.mode is LifecycleMode.PROVISION_ONLY
    assert loaded.lifecycle.provision_cycles == 5
    assert loaded.lifecycle.skip_factory_reset is True
    assert loaded.diagnostics.enabled is False
    assert loaded.diagnostics.output_directory == "env-diagnostics"
    assert loaded.os_janitor.enabled is True
    assert loaded.os_janitor.minimum_age_hours == 36
    assert loaded.os_janitor.dry_run is False


@pytest.mark.parametrize(
    "missing_variable",
    [
        "SITE_UNDER_TEST",
        "MACHINE_UNDER_TEST",
        "DPU_COUNT",
        "MLT_VPC_NAME",
        "MLT_VPC_PREFIX_NAME",
        "MLT_IP_BLOCK_NAME",
        "MLT_VPC_PREFIX_LENGTH",
    ],
)
def test_rejects_missing_required_values(missing_variable):
    environment = dict(REQUIRED_ENVIRONMENT)
    del environment[missing_variable]

    with pytest.raises(ConfigError, match=rf"\${missing_variable}"):
        load_config(environ=environment)


@pytest.mark.parametrize("value", ["0", "-1", "not-a-number", ""])
def test_rejects_invalid_dpu_count(value):
    environment = {**REQUIRED_ENVIRONMENT, "DPU_COUNT": value}

    with pytest.raises(ConfigError, match="target.expected_dpu_count"):
        load_config(environ=environment)


@pytest.mark.parametrize("value", ["7", "32", "not-a-number", ""])
def test_rejects_invalid_vpc_prefix_length(value):
    environment = {**REQUIRED_ENVIRONMENT, "MLT_VPC_PREFIX_LENGTH": value}

    with pytest.raises(ConfigError, match="resources.vpc_prefix_length"):
        load_config(environ=environment)


@pytest.mark.parametrize("value", ["yes", "1", "", "not-a-boolean"])
def test_rejects_non_boolean_environment_values(value):
    environment = {**REQUIRED_ENVIRONMENT, "SKIP_FACTORY_RESET": value}

    with pytest.raises(ConfigError, match="must be true or false"):
        load_config(environ=environment)


@pytest.mark.parametrize(
    ("environment_variable", "value", "message"),
    [
        ("MLT_DIAGNOSTICS_ENABLED", "yes", "diagnostics.enabled"),
        ("MLT_DIAGNOSTICS_OUTPUT_DIRECTORY", "", "diagnostics.output_directory"),
    ],
)
def test_rejects_invalid_diagnostics_configuration(
    environment_variable, value, message
):
    environment = {**REQUIRED_ENVIRONMENT, environment_variable: value}

    with pytest.raises(ConfigError, match=message):
        load_config(environ=environment)


@pytest.mark.parametrize(
    ("environment_variable", "value", "message"),
    [
        ("MLT_OS_JANITOR_ENABLED", "yes", "os_janitor.enabled"),
        (
            "MLT_OS_JANITOR_MINIMUM_AGE_HOURS",
            "0",
            "os_janitor.minimum_age_hours",
        ),
        ("MLT_OS_JANITOR_DRY_RUN", "yes", "os_janitor.dry_run"),
    ],
)
def test_rejects_invalid_os_janitor_configuration(
    environment_variable, value, message
):
    environment = {**REQUIRED_ENVIRONMENT, environment_variable: value}

    with pytest.raises(ConfigError, match=message):
        load_config(environ=environment)


def test_loads_kubernetes_diagnostics_from_toml(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[site]
name = "test-site"
[target]
machine_id = "fm100ht-test-machine"
expected_dpu_count = 2
[resources]
vpc_name = "test-vpc"
vpc_prefix_name = "test-prefix"
ip_block_name = "test-ip-block"
vpc_prefix_length = 29
[diagnostics.kubernetes]
enabled = true
lookback_minutes = 15
max_bytes_per_container = 2048
[[diagnostics.kubernetes.workloads]]
namespace = "forge-system"
deployment = "nico-api"
[[diagnostics.kubernetes.workloads]]
namespace = "nico-rest"
deployment = "nico-rest-api"
""",
    )

    loaded = load_config(config_path=config_path, environ={})

    assert loaded.diagnostics.kubernetes.enabled is True
    assert loaded.diagnostics.kubernetes.lookback_minutes == 15
    assert loaded.diagnostics.kubernetes.max_bytes_per_container == 2048
    assert [
        (workload.namespace, workload.deployment)
        for workload in loaded.diagnostics.kubernetes.workloads
    ] == [
        ("forge-system", "nico-api"),
        ("nico-rest", "nico-rest-api"),
    ]


def test_environment_configures_kubernetes_diagnostics():
    loaded = load_config(
        environ={
            **REQUIRED_ENVIRONMENT,
            "MLT_DIAGNOSTICS_KUBERNETES_ENABLED": "true",
            "MLT_DIAGNOSTICS_KUBERNETES_LOOKBACK_MINUTES": "10",
            "MLT_DIAGNOSTICS_KUBERNETES_MAX_BYTES_PER_CONTAINER": "4096",
            "MLT_DIAGNOSTICS_KUBERNETES_DEPLOYMENTS": (
                "forge-system/nico-api,nico-rest/nico-rest-api"
            ),
        }
    )

    assert loaded.diagnostics.kubernetes.enabled is True
    assert loaded.diagnostics.kubernetes.lookback_minutes == 10
    assert loaded.diagnostics.kubernetes.max_bytes_per_container == 4096
    assert len(loaded.diagnostics.kubernetes.workloads) == 2


def test_disabled_diagnostics_ignore_kubernetes_configuration():
    loaded = load_config(
        environ={
            **REQUIRED_ENVIRONMENT,
            "MLT_DIAGNOSTICS_ENABLED": "false",
            "MLT_DIAGNOSTICS_KUBERNETES_ENABLED": "true",
        }
    )

    assert loaded.diagnostics.enabled is False
    assert loaded.diagnostics.kubernetes.enabled is False
    assert loaded.diagnostics.kubernetes.workloads == ()


def test_disabled_kubernetes_diagnostics_ignore_kubernetes_configuration():
    loaded = load_config(
        environ={
            **REQUIRED_ENVIRONMENT,
            "MLT_DIAGNOSTICS_KUBERNETES_DEPLOYMENTS": "missing-namespace",
            "MLT_DIAGNOSTICS_KUBERNETES_LOOKBACK_MINUTES": "0",
            "MLT_DIAGNOSTICS_KUBERNETES_MAX_BYTES_PER_CONTAINER": "invalid",
        }
    )

    assert loaded.diagnostics.kubernetes.enabled is False
    assert loaded.diagnostics.kubernetes.lookback_minutes is None
    assert loaded.diagnostics.kubernetes.max_bytes_per_container == 5_000_000
    assert loaded.diagnostics.kubernetes.workloads == ()


@pytest.mark.parametrize(
    ("environment", "message"),
    [
        (
            {"MLT_DIAGNOSTICS_KUBERNETES_ENABLED": "true"},
            "must contain at least one deployment",
        ),
        (
            {
                "MLT_DIAGNOSTICS_KUBERNETES_ENABLED": "true",
                "MLT_DIAGNOSTICS_KUBERNETES_DEPLOYMENTS": "missing-namespace",
            },
            "namespace/deployment format",
        ),
        (
            {
                "MLT_DIAGNOSTICS_KUBERNETES_ENABLED": "true",
                "MLT_DIAGNOSTICS_KUBERNETES_LOOKBACK_MINUTES": "0",
            },
            "lookback_minutes",
        ),
    ],
)
def test_rejects_invalid_kubernetes_diagnostics(environment, message):
    with pytest.raises(ConfigError, match=message):
        load_config(environ={**REQUIRED_ENVIRONMENT, **environment})


def test_rejects_sitewide_fallback_for_provision_only_mode():
    environment = {
        **REQUIRED_ENVIRONMENT,
        "MLT_MODE": "provision-only",
        "TEST_SITEWIDE_BMC_FALLBACK": "true",
    }

    with pytest.raises(ConfigError, match="cannot be true"):
        load_config(environ=environment)


@pytest.mark.parametrize(
    "contents, message",
    [
        ("unexpected = true", "top level"),
        ("[target]\nmachine_id = 'id'\nunexpected = true", r"\[target\]"),
        ("site = 'not-a-table'", r"\[site\] must be a table"),
    ],
)
def test_rejects_unknown_keys_and_invalid_sections(tmp_path, contents, message):
    config_path = _write_config(tmp_path / "invalid.toml", contents)

    with pytest.raises(ConfigError, match=message):
        load_config(config_path=config_path, environ=REQUIRED_ENVIRONMENT)


def test_reports_invalid_toml(tmp_path):
    config_path = _write_config(tmp_path / "invalid.toml", "[target\n")

    with pytest.raises(ConfigError, match="Invalid TOML"):
        load_config(config_path=config_path, environ=REQUIRED_ENVIRONMENT)


def test_rejects_removed_interface_selection_key(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        "[resources]\ninterface_selection = 'auto'\n",
    )

    with pytest.raises(ConfigError, match="Unknown configuration key.*interface_selection"):
        load_config(config_path=config_path, environ=REQUIRED_ENVIRONMENT)


def test_debug_access_defaults_to_disabled():
    config = load_config(environ=dict(REQUIRED_ENVIRONMENT))

    assert config.debug.ssh_public_key is None
    assert config.debug.enable_console_password is False


def test_debug_section_is_read_from_toml(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[debug]
ssh_public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA operator@example"
enable_console_password = true
""",
    )

    config = load_config(
        config_path=config_path, environ=dict(REQUIRED_ENVIRONMENT)
    )

    assert config.debug.ssh_public_key == (
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAA operator@example"
    )
    assert config.debug.enable_console_password is True


def test_debug_section_is_read_from_environment():
    environment = dict(REQUIRED_ENVIRONMENT)
    environment["MLT_DEBUG_SSH_PUBLIC_KEY"] = "ssh-ed25519 AAAAB3 operator@example"
    environment["ENABLE_MLT_DEBUG_CONSOLE_PASSWORD"] = "true"

    config = load_config(environ=environment)

    assert config.debug.ssh_public_key == "ssh-ed25519 AAAAB3 operator@example"
    assert config.debug.enable_console_password is True


def test_blank_debug_ssh_public_key_is_treated_as_absent():
    """An unset CI variable arrives as an empty string, not a missing key."""
    environment = dict(REQUIRED_ENVIRONMENT)
    environment["MLT_DEBUG_SSH_PUBLIC_KEY"] = "   "

    config = load_config(environ=environment)

    assert config.debug.ssh_public_key is None


def test_unknown_debug_key_is_rejected(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[debug]
ssh_private_key = "definitely not this"
""",
    )

    with pytest.raises(ConfigError, match="ssh_private_key"):
        load_config(config_path=config_path, environ=dict(REQUIRED_ENVIRONMENT))


def test_keep_instance_defaults_to_disabled():
    config = load_config(environ=dict(REQUIRED_ENVIRONMENT))

    assert config.debug.keep_instance is False


def test_keep_instance_is_read_from_environment():
    environment = dict(REQUIRED_ENVIRONMENT)
    environment["ENABLE_MLT_DEBUG_KEEP_INSTANCE"] = "true"

    assert load_config(environ=environment).debug.keep_instance is True


def test_keep_instance_is_read_from_toml(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[debug]
keep_instance = true
""",
    )

    config = load_config(
        config_path=config_path, environ=dict(REQUIRED_ENVIRONMENT)
    )

    assert config.debug.keep_instance is True


def test_keep_instance_rejects_more_than_one_provision_cycle():
    """Every cycle after the first would strand another instance."""
    environment = dict(REQUIRED_ENVIRONMENT)
    environment["ENABLE_MLT_DEBUG_KEEP_INSTANCE"] = "true"
    environment["PROVISION_CYCLES"] = "2"

    with pytest.raises(ConfigError, match="keep_instance"):
        load_config(environ=environment)


def test_keep_instance_allows_a_single_provision_cycle():
    environment = dict(REQUIRED_ENVIRONMENT)
    environment["ENABLE_MLT_DEBUG_KEEP_INSTANCE"] = "true"
    environment["PROVISION_CYCLES"] = "1"

    config = load_config(environ=environment)

    assert config.debug.keep_instance is True
    assert config.provision_cycles == 1


def test_provision_cycles_still_parse_without_keep_instance():
    """The cycle count was hoisted for the cross-check; it must still work."""
    environment = dict(REQUIRED_ENVIRONMENT)
    environment["PROVISION_CYCLES"] = "3"

    assert load_config(environ=environment).provision_cycles == 3


def test_retaining_an_instance_fails_the_run(monkeypatch, capsys):
    """A run that skipped deprovisioning must not report success."""
    monkeypatch.setattr(
        lifecycle.nico_rest, "wait_for_instance_ip", lambda *a, **k: "10.0.0.9"
    )
    monkeypatch.delenv("PYTEST_VERSION", raising=False)
    ngc_uuids = lifecycle.NGCUUIDs(
        site_uuid="s",
        vpc_uuid="v",
        network_interface={"vpcPrefixId": "n"},
        os_uuid="o",
    )

    with pytest.raises(SystemExit) as exit_info:
        lifecycle._retain_instance_and_exit(ngc_uuids, "instance-uuid-1")

    assert exit_info.value.code != 0
    output = capsys.readouterr()
    assert "instance-uuid-1" in output.out
    assert "10.0.0.9" in output.out
    assert "NOT deleting the instance" in output.out


def test_retaining_an_instance_still_reports_when_the_ip_lookup_fails(
    monkeypatch, capsys
):
    """A failed lookup must not swallow the notice that the box is still up."""
    def _boom(*args, **kwargs):
        raise RuntimeError("no route")

    monkeypatch.setattr(lifecycle.nico_rest, "wait_for_instance_ip", _boom)
    monkeypatch.delenv("PYTEST_VERSION", raising=False)
    ngc_uuids = lifecycle.NGCUUIDs(
        site_uuid="s",
        vpc_uuid="v",
        network_interface={"vpcPrefixId": "n"},
        os_uuid="o",
    )

    with pytest.raises(SystemExit) as exit_info:
        lifecycle._retain_instance_and_exit(ngc_uuids, "instance-uuid-2")

    assert exit_info.value.code != 0
    output = capsys.readouterr().out
    assert "instance-uuid-2" in output
    assert "NOT deleting the instance" in output


GRPC_API_TOML = """
[site]
name = "toml-site"

[target]
machine_id = "fm100ht-from-toml"
expected_dpu_count = 4

[resources]
vpc_name = "toml-vpc"
vpc_prefix_name = "toml-prefix"
ip_block_name = "toml-ip-block"
vpc_prefix_length = 29

[grpc_api]
url = "https://api.example.test:1079"
root_ca_path = "/var/run/secrets/roots/ca.crt"
"""

CLIENT_CERTIFICATE_TOML = GRPC_API_TOML + """
[grpc_api.client_certificate]
vault_pki_mount = "pki"
vault_pki_role = "client-role"
common_name = "client"
"""


# A bare value where a table belongs. It must precede the tables: TOML would
# otherwise read it as a key of whichever table came last.
SCALAR_GRPC_API_TOML = "grpc_api = 1\n" + GRPC_API_TOML.split("[grpc_api]")[0]


def test_grpc_api_section_is_optional():
    """A deployment whose API accepts an unauthenticated caller configures none."""
    loaded = load_config(config_path=None, environ=REQUIRED_ENVIRONMENT)

    assert loaded.grpc_api is None


def test_loads_grpc_api_section_without_a_client_certificate(tmp_path):
    config_path = _write_config(tmp_path / "mlt.toml", GRPC_API_TOML)

    loaded = load_config(config_path=config_path, environ={})

    assert loaded.grpc_api.url == "https://api.example.test:1079"
    assert loaded.grpc_api.root_ca_path == "/var/run/secrets/roots/ca.crt"
    assert loaded.grpc_api.client_certificate is None


def test_loads_client_certificate_settings(tmp_path):
    config_path = _write_config(tmp_path / "mlt.toml", CLIENT_CERTIFICATE_TOML)

    loaded = load_config(config_path=config_path, environ={})
    certificate = loaded.grpc_api.client_certificate

    assert certificate.vault_pki_mount == "pki"
    assert certificate.vault_pki_role == "client-role"
    assert certificate.common_name == "client"
    # A run can take several hours, so the default must outlast one.
    assert certificate.ttl == "12h"


def test_environment_configures_the_grpc_api_without_toml():
    loaded = load_config(
        config_path=None,
        environ={
            **REQUIRED_ENVIRONMENT,
            "GRPC_API_URL": "https://api.from-env.test:1079",
            "GRPC_API_ROOT_CA_PATH": "/var/run/secrets/roots/ca.crt",
            "GRPC_API_CLIENT_CERT_VAULT_PKI_MOUNT": "pki",
            "GRPC_API_CLIENT_CERT_VAULT_PKI_ROLE": "client-role",
            "GRPC_API_CLIENT_CERT_COMMON_NAME": "client",
            "GRPC_API_CLIENT_CERT_TTL": "6h",
        },
    )

    assert loaded.grpc_api.url == "https://api.from-env.test:1079"
    assert loaded.grpc_api.client_certificate.ttl == "6h"


def test_environment_overrides_grpc_api_toml(tmp_path):
    config_path = _write_config(tmp_path / "mlt.toml", CLIENT_CERTIFICATE_TOML)

    loaded = load_config(
        config_path=config_path,
        environ={"GRPC_API_URL": "https://api.override.test:1079"},
    )

    assert loaded.grpc_api.url == "https://api.override.test:1079"
    assert loaded.grpc_api.client_certificate.vault_pki_role == "client-role"


@pytest.mark.parametrize(
    ("contents", "message"),
    [
        (GRPC_API_TOML.replace('url = "https://api.example.test:1079"\n', ""), "$GRPC_API_URL"),
        (
            GRPC_API_TOML.replace('root_ca_path = "/var/run/secrets/roots/ca.crt"\n', ""),
            "$GRPC_API_ROOT_CA_PATH",
        ),
        (
            CLIENT_CERTIFICATE_TOML.replace('vault_pki_role = "client-role"\n', ""),
            "vault_pki_role",
        ),
        (GRPC_API_TOML + '\n[grpc_api.client_certificate]\nvault_pki_mount = "pki"\n', "vault_pki_role"),
        (GRPC_API_TOML + "\nextra_key = 1\n", "Unknown configuration key"),
        (
            CLIENT_CERTIFICATE_TOML + 'unexpected = "value"\n',
            "[grpc_api.client_certificate]",
        ),
        (SCALAR_GRPC_API_TOML, "[grpc_api] must be a table"),
        (GRPC_API_TOML + "\n[grpc_api.client_certificate]\n", "$GRPC_API_CLIENT_CERT_VAULT_PKI_MOUNT"),
    ],
)
def test_rejects_incomplete_or_unknown_grpc_api_settings(tmp_path, contents, message):
    config_path = _write_config(tmp_path / "mlt.toml", contents)

    with pytest.raises(ConfigError, match=re.escape(message)):
        load_config(config_path=config_path, environ={})


def test_oauth_section_is_absent_when_nothing_configures_a_vault():
    config = load_config(environ=dict(REQUIRED_ENVIRONMENT))

    assert config.oauth is None


def test_an_unrelated_oauth_variable_does_not_configure_the_section():
    # "OAUTH_" is a common prefix in other tooling, so only the names this
    # module defines may pull a run into the token-exchange path.
    environment = dict(
        REQUIRED_ENVIRONMENT,
        OAUTH_REDIRECT_URI="https://unrelated.example.test/callback",
        OAUTH_CLIENT_ID="some-other-tool",
    )

    assert load_config(environ=environment).oauth is None


def test_a_direct_credential_still_needs_a_token_endpoint():
    environment = dict(REQUIRED_ENVIRONMENT, OAUTH_CREDENTIAL="id:secret")

    # The credential says nothing about which authorization server issues
    # tokens the site's API will trust.
    with pytest.raises(ConfigError, match="token_url"):
        load_config(environ=environment)


def test_loads_the_oauth_section_from_toml(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[oauth]
vault_address = "https://vault.example.test"
vault_namespace = "dgxc-dsx"
auth_method = "jwt"
auth_mount = "jwt/k8s/dsx-example"
auth_role = "mlt"
auth_jwt_source = "file:/var/run/secrets/kubernetes.io/serviceaccount/token"
secret_engine = "raw"
secret_path = "services/dsx/clients/mlt/issue/creds"
client_id_field = "client_id"
client_secret_field = "secret"
token_url = "https://issuer.example.test/token"
scope = "carbide"
""",
    )

    oauth = load_config(
        config_path=config_path, environ=dict(REQUIRED_ENVIRONMENT)
    ).oauth

    assert oauth.vault_namespace == "dgxc-dsx"
    assert oauth.auth_method == "jwt"
    assert oauth.auth_mount == "jwt/k8s/dsx-example"
    assert oauth.secret_engine == "raw"
    assert oauth.secret_path == "services/dsx/clients/mlt/issue/creds"
    assert oauth.token_url == "https://issuer.example.test/token"


def test_oauth_defaults_suit_an_in_cluster_pod_reading_kv_v2():
    environment = dict(
        REQUIRED_ENVIRONMENT,
        OAUTH_VAULT_ADDR="https://vault.example.test",
        OAUTH_SECRET_PATH="nico/credential",
        OAUTH_TOKEN_URL="https://issuer.example.test/token",
        OAUTH_SCOPE="carbide",
    )

    oauth = load_config(environ=environment).oauth

    assert oauth.auth_method == "kubernetes"
    assert oauth.auth_mount == "kubernetes"
    # An empty role means "derive it from the service-account token".
    assert oauth.auth_role == ""
    assert oauth.auth_jwt_source == (
        "file:/var/run/secrets/kubernetes.io/serviceaccount/token"
    )
    assert oauth.secret_engine == "kv-v2"
    assert oauth.secret_mount == "secrets"


def test_environment_overrides_the_oauth_toml_values(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[oauth]
vault_address = "https://from-toml.example.test"
secret_path = "from/toml"
token_url = "https://toml-issuer.example.test/token"
scope = "carbide"
""",
    )
    environment = dict(
        REQUIRED_ENVIRONMENT, OAUTH_SECRET_PATH="from/environment"
    )

    oauth = load_config(config_path=config_path, environ=environment).oauth

    assert oauth.vault_address == "https://from-toml.example.test"
    assert oauth.secret_path == "from/environment"


def test_an_incomplete_oauth_section_is_rejected(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[oauth]
token_url = "https://issuer.example.test/token"
scope = "carbide"
vault_address = "https://vault.example.test"
""",
    )

    with pytest.raises(ConfigError, match="secret_path"):
        load_config(config_path=config_path, environ=dict(REQUIRED_ENVIRONMENT))


@pytest.mark.parametrize(
    ("variable", "value", "expected"),
    [
        ("OAUTH_AUTH_METHOD", "oidc", "oauth.auth_method must be one of"),
        ("OAUTH_SECRET_ENGINE", "kv-v1", "oauth.secret_engine must be one of"),
    ],
)
def test_oauth_rejects_values_outside_its_vocabulary(variable, value, expected):
    environment = dict(
        REQUIRED_ENVIRONMENT,
        OAUTH_VAULT_ADDR="https://vault.example.test",
        OAUTH_SECRET_PATH="nico/credential",
        OAUTH_TOKEN_URL="https://issuer.example.test/token",
        OAUTH_SCOPE="carbide",
        **{variable: value},
    )

    with pytest.raises(ConfigError, match=expected):
        load_config(environ=environment)


def test_an_unknown_oauth_key_is_rejected(tmp_path):
    config_path = _write_config(
        tmp_path / "mlt.toml",
        """
[oauth]
vault_address = "https://vault.example.test"
secret_path = "nico/credential"
token_url = "https://issuer.example.test/token"
scope = "carbide"
vault_token = "never-put-a-secret-here"
""",
    )

    with pytest.raises(ConfigError, match="'vault_token'"):
        load_config(config_path=config_path, environ=dict(REQUIRED_ENVIRONMENT))


def test_the_example_config_documents_every_supported_oauth_key():
    example_path = Path(__file__).resolve().parents[2] / "config" / "config.example.toml"
    example = example_path.read_text(encoding="utf-8")
    documented = {
        line.split("=", 1)[0].strip()
        for line in example.splitlines()
        if "=" in line and not line.strip().startswith("#")
    }

    assert config_module._SECTION_KEYS["oauth"] <= documented


def test_a_direct_credential_needs_only_the_token_endpoint():
    environment = dict(
        REQUIRED_ENVIRONMENT,
        OAUTH_CREDENTIAL="id:secret",
        OAUTH_TOKEN_URL="https://issuer.example.test/token",
        OAUTH_SCOPE="carbide",
    )

    oauth = load_config(environ=environment).oauth

    assert oauth.token_url == "https://issuer.example.test/token"
    # No Vault is read, so nothing needs to locate one.
    assert oauth.vault_address == ""
    assert oauth.secret_path == ""


def _oauth_environment(**overrides):
    environment = dict(
        REQUIRED_ENVIRONMENT,
        OAUTH_VAULT_ADDR="https://vault.example.test",
        OAUTH_SECRET_PATH="nico/credential",
        OAUTH_TOKEN_URL="https://issuer.example.test/token",
        OAUTH_SCOPE="carbide",
    )
    environment.update(overrides)
    return environment


@pytest.mark.parametrize(
    "variable", ["OAUTH_TOKEN_URL", "OAUTH_VAULT_ADDR"]
)
def test_plaintext_http_endpoints_are_rejected(variable):
    """The credential crosses both connections."""
    environment = _oauth_environment(**{variable: "http://insecure.example.test"})

    with pytest.raises(ConfigError, match="must use https"):
        load_config(environ=environment)


@pytest.mark.parametrize(
    "variable", ["OAUTH_TOKEN_URL", "OAUTH_VAULT_ADDR"]
)
def test_plaintext_http_is_allowed_to_the_loopback_for_port_forwards(variable):
    environment = _oauth_environment(**{variable: "http://localhost:8200"})

    assert load_config(environ=environment).oauth is not None


def test_jwt_authentication_requires_a_role_at_configuration_time():
    """Only a Kubernetes mount can derive its role from the token."""
    environment = _oauth_environment(OAUTH_AUTH_METHOD="jwt")

    with pytest.raises(ConfigError, match="oauth.auth_role must be set"):
        load_config(environ=environment)


@pytest.mark.parametrize("source", ["vault-token", "env:", "http:/x", "file"])
def test_a_malformed_jwt_source_is_rejected(source):
    environment = _oauth_environment(OAUTH_AUTH_JWT_SOURCE=source)

    with pytest.raises(ConfigError, match="env:<NAME>' or 'file:<PATH>"):
        load_config(environ=environment)


def test_secret_mount_is_rejected_rather_than_ignored_for_a_raw_read():
    """A raw read takes the full path, so a mount would do nothing."""
    environment = _oauth_environment(
        OAUTH_SECRET_ENGINE="raw", OAUTH_SECRET_MOUNT="secrets"
    )

    with pytest.raises(ConfigError, match="applies only to secret_engine"):
        load_config(environ=environment)


def test_a_raw_read_without_a_mount_is_accepted():
    environment = _oauth_environment(OAUTH_SECRET_ENGINE="raw")

    assert load_config(environ=environment).oauth.secret_engine == "raw"


@pytest.mark.parametrize("value", ["notapair", ":secretonly", "idonly:", ":"])
def test_a_direct_credential_that_is_not_a_pair_is_rejected(value):
    environment = _oauth_environment(OAUTH_CREDENTIAL=value)

    with pytest.raises(ConfigError, match="client_id:client_secret"):
        load_config(environ=environment)


def test_rejecting_a_direct_credential_does_not_echo_it():
    """The rejected value is the credential."""
    environment = _oauth_environment(OAUTH_CREDENTIAL="hunter2-not-a-pair")

    with pytest.raises(ConfigError) as error:
        load_config(environ=environment)

    assert "hunter2" not in str(error.value)


@pytest.mark.parametrize("url", ["https:///token", "https:token", "https://"])
def test_a_url_without_a_host_is_rejected(url):
    """These parse as https but cannot be requested."""
    environment = _oauth_environment(OAUTH_TOKEN_URL=url)

    with pytest.raises(ConfigError, match="must include a host"):
        load_config(environ=environment)
