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

import datetime
import os
import pprint
import subprocess
import sys
import time
import uuid
from dataclasses import dataclass
from typing import Literal, NoReturn, get_args

import paramiko
import requests
import urllib3

from lib import (
    admin_cli,
    diagnostics,
    debug_artifacts,
    network,
    network_resources,
    nico_rest,
    os_janitor,
    output,
    timeouts,
)
from lib.config import (
    Config,
    ConfigError,
    LifecycleMode,
    NetworkResourcesConfig,
    load_config,
)
from lib.ephemeral_os import (
    TEMPORARY_OS_DESCRIPTION,
    build_ephemeral_operating_system,
)
from lib.reset_drivers import (
    BlueFieldDpuResetDriver,
    DellHostResetDriver,
    DpuResetDriver,
    GB200HostResetDriver,
    HostResetDriver,
    LenovoHostResetDriver,
    ResetDriverError,
    ResetTarget,
)
from lib.site_vault import (
    BmcCredentials,
    SiteVaultClient,
    SiteVaultError,
    SiteVaultCredentialNotFound,
)
from lib.credentials import CredentialError, resolve_client_credential

urllib3.disable_warnings()

# Vendor support configuration
SupportedVendor = Literal["lenovo", "dell", "supermicro", "nvidia"]
SUPPORTED_VENDORS = set(get_args(SupportedVendor))
SUPPORTED_FOR_FACTORY_RESET = {"lenovo", "dell", "nvidia"}
VENDOR_BMC_USERNAMES = {
    "lenovo": "USERID",
    "dell": "root",
    "supermicro": "ADMIN",
    "nvidia": "admin",
}

HOST_RESET_DRIVERS: dict[str, HostResetDriver] = {
    "lenovo": LenovoHostResetDriver(),
    "dell": DellHostResetDriver(),
    "nvidia": GB200HostResetDriver(),
}
DPU_RESET_DRIVER: DpuResetDriver = BlueFieldDpuResetDriver()

TIMEOUT_EXCEPTIONS = (TimeoutError, subprocess.TimeoutExpired, requests.Timeout)
BMC_PROBE_TIMEOUT_SECONDS = 30


def _restart_request_expected(test_config: Config) -> bool:
    """Whether this lifecycle run must observe NICo's ingestion reboot request."""
    return not test_config.skip_factory_reset and not test_config.test_sitewide_bmc_fallback


@dataclass
class SiteConfig:
    """Configuration for the site under test."""

    site: nico_rest.Site
    host_bmc_credentials: BmcCredentials
    dpu_bmc_credentials: dict[str, BmcCredentials]
    sitewide_bmc_password: str | None = None


@dataclass
class MachineInfo:
    """Information about the machine under test."""

    machine: dict
    vendor: SupportedVendor
    host_bmc_ip: str
    host_bmc_mac: str
    dpu_ids: list[str]
    dpu_info_map: dict[str, dict[str, str]]
    machine_under_test_dpu: str
    machine_under_test_predicted_host: str


@dataclass(frozen=True)
class BmcEndpoint:
    """A host or DPU BMC endpoint involved in lifecycle recovery."""

    name: str
    ip: str
    mac: str


@dataclass(frozen=True)
class RecoveryFailure:
    """Why one BMC endpoint failed to recover, and at which stage."""

    message: str
    # Set when Site Explorer refreshed the endpoint cleanly but NICo had not
    # restored its per-MAC Site Vault record. Kept separate so the caller can say
    # "the credential never came back" rather than blaming Site Explorer.
    missing_site_vault_credential: bool = False


@dataclass
class NGCUUIDs:
    """UUIDs required for cloud operations."""

    site_uuid: str
    vpc_uuid: str
    network_interface: dict[str, object]
    # Filled in once the per-run ephemeral OS definition has been created.
    os_uuid: str | None


def test_machine_lifecycle():
    """
    Main entry point for the machine lifecycle test.
    """

    output.start_run()
    run_started_at = time.monotonic()
    try:
        output.start_stage("Configuration")
        test_config = setup_test_config()
        pprint.pprint(test_config)
        # Holds the gRPC API client identity for the run and removes it afterwards.
        output.start_stage("API client setup")
        with admin_cli.client_identity(test_config.grpc_api):
            _run_machine_lifecycle(test_config, run_started_at)
    except Exception as error:
        output.print_failure(f"Unhandled {type(error).__name__}: {error}")
        raise
    output.print_success(time.monotonic() - run_started_at)


def _run_machine_lifecycle(test_config: Config, run_started_at: float) -> None:
    """Run the lifecycle stages selected by the configuration."""

    ##################################
    # 1. Setup and log initial state #
    ##################################
    output.start_stage("Machine discovery and preflight")
    machine_info = collect_machine_info(test_config)
    site_config = setup_site_config(test_config, machine_info)
    pprint.pprint(_mask_site_config_creds(site_config))
    _run_os_janitor(test_config)
    if machine_info.vendor == "lenovo":
        _resolve_lenovo_host_bmc_username(machine_info, site_config)

    # Gate for vendor-specific functionality that's not yet implemented
    if machine_info.vendor not in SUPPORTED_FOR_FACTORY_RESET:
        if not (
            test_config.skip_factory_reset or test_config.test_sitewide_bmc_fallback
        ):
            _error_and_exit(
                f"Factory reset is not yet supported for {machine_info.vendor}. "
                "Set lifecycle.skip_factory_reset=true or SKIP_FACTORY_RESET=true"
            )

    # Check machine is in a testable state
    verify_initial_machine_state(test_config, site_config)

    # Fail before factory reset if MACHINE_UNDER_TEST carries an instance type.
    # MLT no longer assigns or clears types and NICo does not allow force-delete
    # of assigned machines.
    verify_machine_has_no_instance_type(test_config)

    if test_config.lifecycle.mode is not LifecycleMode.PROVISION_ONLY:
        _run_ingestion_phase(
            test_config, site_config, machine_info, run_started_at
        )
    else:
        print("Skipping ingestion portion because lifecycle mode is provision-only")

    if test_config.lifecycle.mode is not LifecycleMode.INGESTION_ONLY:
        _run_provisioning_cycles(
            test_config, site_config, machine_info, run_started_at
        )
    else:
        print("Skipping instance provision portion because lifecycle mode is ingestion-only")


def _run_os_janitor(test_config: Config) -> None:
    """Run stale-OS cleanup as a best-effort preflight when enabled."""
    janitor_config = test_config.os_janitor
    if not janitor_config.enabled:
        return

    print(
        "Running temporary OS janitor "
        f"(minimum age {janitor_config.minimum_age_hours}h, dry_run={janitor_config.dry_run})"
    )
    try:
        os_janitor.cleanup_stale_operating_systems(
            minimum_age=datetime.timedelta(hours=janitor_config.minimum_age_hours),
            dry_run=janitor_config.dry_run,
        )
    except Exception as error:
        print(
            f"WARNING: temporary OS janitor failed; continuing MLT: {error}",
            file=sys.stderr,
        )


def _run_ingestion_phase(
    test_config: Config,
    site_config: SiteConfig,
    machine_info: MachineInfo,
    run_started_at: float,
) -> None:
    """Reset, reingest, and refresh credentials for the target machine."""
    ####################################
    # 2. Factory reset host and DPU(s) #
    ####################################
    output.start_stage("Hardware factory reset")
    if test_config.test_sitewide_bmc_fallback:
        print(
            "Testing the site-wide BMC credential fallback: verifying the DPU BMCs "
            "already use the site-wide password and skipping factory reset"
        )
        # Ensure BMC is already using site-wide password
        verify_bmc_rotation_converged(machine_info)
        verify_bmc_sitewide_credentials(site_config, machine_info)
    elif not test_config.skip_factory_reset:
        perform_factory_reset(test_config, site_config, machine_info)
    else:
        print("Skipping hardware factory reset because skip_factory_reset is enabled")

    ###########################
    # 3. Force delete machine #
    ###########################
    # Perform force delete and wait for reingestion to Ready state
    output.start_stage("Machine reingestion")
    force_delete_and_await_reingestion(
        test_config,
        site_config,
        machine_info,
        run_started_at=run_started_at,
    )

    # NICo recreates the authoritative per-MAC credential records during
    # reingestion. Replace the pre-delete copies before any later direct
    # BMC access rather than assuming they stayed valid.
    try:
        _refresh_site_vault_bmc_credentials(site_config, machine_info)
    except SiteVaultError as error:
        _error_and_exit(
            f"Site Vault credential refresh after reingestion failed: {error}"
        )


def _run_provisioning_cycles(
    test_config: Config,
    site_config: SiteConfig,
    machine_info: MachineInfo,
    run_started_at: float,
) -> None:
    """Reconcile networking, run provisioning cycles, and clean up owned resources."""
    resources = test_config.resources
    if resources is None:
        raise ConfigError("Network resources are required for provisioning")

    ownership = network_resources.NetworkResourceOwnership()
    try:
        output.start_stage("Network resource reconciliation")
        site_uuid = nico_rest.get_site_uuid(site_config.site.name)
        # Reconciliation records resources created by this run in the ownership
        # object for cleanup.
        reconciled = network_resources.reconcile_network_resources(
            resources, site_uuid, ownership
        )
        ngc_uuids = collect_ngc_uuids(resources, site_uuid, reconciled)

        ####################################
        # 4. Create and delete an instance #
        ####################################
        temporary_os_uuid = None
        temporary_os_name = None
        try:
            output.start_stage("Temporary operating-system creation")
            ephemeral_os = build_ephemeral_operating_system(
                debug_public_key=test_config.debug.ssh_public_key,
                enable_console_password=test_config.debug.enable_console_password,
            )
            temporary_os_name = ephemeral_os.name
            if test_config.debug.ssh_public_key:
                print("Authorizing the operator's debug SSH key on this instance")
            if ephemeral_os.console_password is not None:
                debug_artifacts.publish_console_password(
                    ephemeral_os.console_password
                )
            print(f"Creating temporary operating system {ephemeral_os.name}")
            created_os = nico_rest.create_operating_system(
                ephemeral_os.name,
                ephemeral_os.ipxe_script,
                ephemeral_os.user_data,
                description=TEMPORARY_OS_DESCRIPTION,
            )
            temporary_os_uuid = created_os["id"]
            nico_rest.wait_for_operating_system_ready(temporary_os_uuid)
            ngc_uuids.os_uuid = temporary_os_uuid

            for i in range(test_config.provision_cycles):
                cycle = i + 1
                output.start_stage(
                    f"Provisioning cycle {cycle}/{test_config.provision_cycles}: "
                    "instance creation and verification"
                )
                instance_uuid = create_instance_and_verify(
                    test_config,
                    site_config,
                    machine_info,
                    ngc_uuids,
                    ephemeral_os.ssh_private_key,
                    run_started_at=run_started_at,
                )
                if test_config.debug.keep_instance:
                    _retain_instance_and_exit(ngc_uuids, instance_uuid)
                output.start_stage(
                    f"Provisioning cycle {cycle}/{test_config.provision_cycles}: "
                    "instance deletion and deprovisioning"
                )
                delete_instance_and_verify(
                    test_config, site_config, ngc_uuids, instance_uuid
                )
        finally:
            if temporary_os_name is not None:
                _cleanup_temporary_operating_system(
                    temporary_os_name, temporary_os_uuid
                )
    finally:
        if resources.cleanup:
            network_resources.cleanup_network_resources(ownership)
        elif ownership.vpc_uuid is not None or ownership.vpc_prefix_uuid is not None:
            print(
                "WARNING: Resources created by this run were not deleted "
                + "because cleanup was disabled."
            )


def _cleanup_temporary_operating_system(
    operating_system_name: str, operating_system_uuid: str | None
) -> None:
    """Best-effort cleanup, recovering the UUID by the unique name if needed."""
    if operating_system_uuid is None:
        try:
            operating_system_uuid = nico_rest.get_operating_system_uuid(
                operating_system_name
            )
        except Exception as lookup_error:
            print(
                "WARNING: could not find temporary operating system "
                f"{operating_system_name!r} for cleanup: {lookup_error}",
                file=sys.stderr,
            )
            return

    print(f"Deleting temporary operating system {operating_system_uuid}")
    try:
        deleted = nico_rest.delete_operating_system(
            operating_system_uuid, strict=False
        )
    except Exception as cleanup_error:
        deleted = False
        print(
            f"WARNING: temporary operating system cleanup raised: {cleanup_error}",
            file=sys.stderr,
        )
    if not deleted:
        print(
            "WARNING: failed to delete temporary operating system "
            f"{operating_system_uuid}",
            file=sys.stderr,
        )


def _mask_site_config_creds(site_config: SiteConfig) -> dict:
    """Create a printable version of site_config with sensitive fields
    masked.
    """
    return {
        "site": site_config.site,
        "host_bmc_credentials": "***masked***",
        "dpu_bmc_credentials": {
            dpu_id: "***masked***" for dpu_id in site_config.dpu_bmc_credentials
        },
    }


def _error_and_exit(
    message: str, set_maintenance: bool = False, machine_id: str | None = None
) -> NoReturn:
    """Print error message and exit, optionally putting machine in
    maintenance mode first.
    If running via pytest, use pytest.fail() instead of exit(1).

    Args:
        message: The error message to print
        set_maintenance: Whether to put the machine into maintenance
          mode before exiting
        machine_id: The ID of the machine to put into maintenance mode
          if set_maintenance is True
    """
    output.print_failure(message)
    if set_maintenance and machine_id:
        try:
            admin_cli.put_machine_into_maintenance_mode(machine_id)
        except Exception as e:
            print(f"Failed to put machine into maintenance mode: {e}", file=sys.stderr)
    # Different exit mode if test is run by pytest
    if os.environ.get("PYTEST_VERSION") is not None:
        import pytest
        pytest.fail(message)
    else:
        sys.exit(1)


def setup_test_config() -> Config:
    """Load and validate portable test configuration before preflight."""
    try:
        return load_config()
    except ConfigError as error:
        _error_and_exit(str(error))


def setup_site_config(test_config: Config, machine_info: MachineInfo) -> SiteConfig:
    """Set up configuration for the site under test.

    The cloud half of the test runs against the site-local NICo REST API.
    Auth is a short-lived bearer obtained by exchanging an OAuth client
    credential. lib.nico_rest reads NICO_BASE_URL, NICO_ORG and NICO_API_NAME from the
    environment it inherits; this only checks NICO_BASE_URL is present and
    defaults the other two.
    Per-machine BMC credentials come from Site Vault. The OAuth client
    credential comes from whichever Vault the ``[oauth]`` configuration names.

    Args:
        test_config: The test configuration containing site information
        machine_info: The discovered host and DPU identifiers
    Returns:
        SiteConfig: Configuration for the site
    """

    site = nico_rest.Site(test_config.site_under_test)

    try:
        host_bmc_credentials, dpu_bmc_credentials = (
            _read_site_vault_bmc_credentials(machine_info)
        )
        # Read during preflight alongside the per-MAC records, so the run stops
        # here rather than partway through the destructive phase.
        sitewide_bmc_password = (
            _read_site_vault_sitewide_bmc_password()
            if test_config.test_sitewide_bmc_fallback
            else None
        )
    except SiteVaultError as error:
        _error_and_exit(f"Site Vault credential preflight failed: {error}")
    # A run given a bearer outright needs no credential and no Vault.
    if not os.environ.get("NICO_TOKEN"):
        if test_config.oauth is None:
            _error_and_exit(
                "No NICo credential is available: set NICO_TOKEN, or configure "
                "the [oauth] section to say where one comes from"
            )
        try:
            credential = resolve_client_credential(test_config.oauth)
        except CredentialError as error:
            _error_and_exit(f"Client credential preflight failed: {error}")
        if credential is None:
            _error_and_exit(
                "No NICo credential is available: set NICO_TOKEN, set "
                "OAUTH_CREDENTIAL, or configure a Vault to read one from"
            )
        # Retain the credential and where to exchange it so lib.nico_rest can
        # refresh mid-run: the bearer is short-lived and the re-ingestion wait
        # alone runs ~90 min, after which cloud calls would otherwise fail with
        # "Authorization token ... has expired". In memory, not the
        # environment, which every subprocess inherits.
        nico_rest.configure_token_refresh(
            credential, test_config.oauth.token_url, test_config.oauth.scope
        )
        nico_rest.refresh_token()

    # Endpoint must be provided by the environment (in-cluster service URL in
    # CI, or a kubectl port-forward when developing locally). Checked here as
    # well as on every call so a URL the bearer must not cross stops the run in
    # preflight rather than on the first request.
    if not os.environ.get("NICO_BASE_URL"):
        _error_and_exit(f"Site {site.name} requires NICO_BASE_URL")
    try:
        nico_rest.check_base_url()
    except nico_rest.NicoError as error:
        _error_and_exit(str(error))
    os.environ.setdefault("NICO_ORG", "ncx")
    os.environ.setdefault("NICO_API_NAME", "nico")

    return SiteConfig(
        site=site,
        host_bmc_credentials=host_bmc_credentials,
        dpu_bmc_credentials=dpu_bmc_credentials,
        sitewide_bmc_password=sitewide_bmc_password,
    )


def _read_site_vault_bmc_credentials(
    machine_info: MachineInfo,
) -> tuple[BmcCredentials, dict[str, BmcCredentials]]:
    """Read the authoritative host and DPU credentials for one machine."""

    with SiteVaultClient() as vault_client:
        host_credentials = vault_client.get_bmc_credentials(machine_info.host_bmc_mac)
        dpu_credentials = {
            dpu_id: vault_client.get_bmc_credentials(
                machine_info.dpu_info_map[dpu_id]["bmc_mac"]
            )
            for dpu_id in machine_info.dpu_ids
        }
    return host_credentials, dpu_credentials


def _read_site_vault_sitewide_bmc_password() -> str:
    """Read the site-wide BMC root password Site Explorer falls back to.
    """

    try:
        version = admin_cli.get_sitewide_bmc_rotation_target_version()
    except (subprocess.CalledProcessError, ValueError) as error:
        _error_and_exit(f"Could not resolve the live site-wide BMC credential version: {error}")
    print(f"Site-wide BMC root credential is at rotation version {version}")

    with SiteVaultClient() as vault_client:
        return vault_client.get_sitewide_bmc_password(version)


def _refresh_site_vault_bmc_credentials(
    site_config: SiteConfig,
    machine_info: MachineInfo,
) -> None:
    """Replace pre-delete credentials with the records NICo recreated."""

    host_credentials, dpu_credentials = _read_site_vault_bmc_credentials(machine_info)
    site_config.host_bmc_credentials = host_credentials
    site_config.dpu_bmc_credentials = dpu_credentials


def collect_machine_info(test_config: Config) -> MachineInfo:
    """Collect and validate information about the machine before running
    the test.

    Args:
        test_config: The test configuration containing machine
          information
    Returns:
        MachineInfo: Information about the machine under test
    """
    machine = admin_cli.get_machine_from_mh_show(test_config.machine_under_test)
    machine_vendor = admin_cli.get_machine_vendor(test_config.machine_under_test)

    machine_vendor_lower = machine_vendor.lower()
    vendor = next(
        (v for v in SUPPORTED_VENDORS if v in machine_vendor_lower),
        None
    )

    if vendor is None:
        _error_and_exit(
            f"{machine_vendor=} is not valid. Expected to contain one of: "
            f"{', '.join(sorted(SUPPORTED_VENDORS))}"
        )

    print(f"Machine vendor is {vendor}")

    host_bmc_ip = machine["host_bmc_ip"]
    host_bmc_mac = machine["host_bmc_mac"]
    # Create a dictionary of DPU IDs to their BMC and OOB IPs
    dpu_ids: list[str] = []
    dpu_info_map: dict[str, dict[str, str]] = {}

    for dpu in machine["dpus"]:
        dpu_id = dpu.get("machine_id")
        bmc_ip = dpu.get("bmc_ip")
        bmc_mac = dpu.get("bmc_mac")
        oob_ip = dpu.get("oob_ip")
        if dpu_id and bmc_ip and bmc_mac and oob_ip:
            dpu_ids.append(dpu_id)
            dpu_info_map[dpu_id] = {
                "bmc_ip": bmc_ip,
                "bmc_mac": bmc_mac,
                "oob_ip": oob_ip,
            }
        else:
            _error_and_exit(f"Missing data for DPU {dpu_id}")

    # Confirm we found the expected number of DPUs
    if len(dpu_info_map) != test_config.expected_dpu_count:
        _error_and_exit(
            f"Found {len(dpu_info_map)} DPU(s) but expected {test_config.expected_dpu_count}"
        )
    print(f"DPUs in this machine: {dpu_info_map}")

    # After force-delete, we'll use the (first) DPU to track state until the host is fully ingested
    machine_under_test_dpu = dpu_ids[0]
    machine_under_test_predicted_host = (
        machine_under_test_dpu[0:5] + "p" + machine_under_test_dpu[6:]
    )

    return MachineInfo(
        machine=machine,
        vendor=vendor,
        host_bmc_ip=host_bmc_ip,
        host_bmc_mac=host_bmc_mac,
        dpu_ids=dpu_ids,
        dpu_info_map=dpu_info_map,
        machine_under_test_dpu=machine_under_test_dpu,
        machine_under_test_predicted_host=machine_under_test_predicted_host,
    )


def _resolve_lenovo_host_bmc_username(
    machine_info: MachineInfo, site_config: SiteConfig
) -> None:
    """Probe the Lenovo host BMC to confirm the default "USERID" works
    and fall back to "root" if not. Some Lenovo BMCs in our fleet may
    still be set to "root".
    """
    url = f"https://{machine_info.host_bmc_ip}/redfish/v1/Systems/1"
    credentials = site_config.host_bmc_credentials
    response = requests.get(
        url,
        auth=(credentials.username, credentials.password),
        verify=False,
        timeout=BMC_PROBE_TIMEOUT_SECONDS,
    )
    if response.status_code == 401 and credentials.username == "USERID":
        print(
            f"Lenovo BMC at {machine_info.host_bmc_ip} returned 401 for 'USERID', "
            f"falling back to 'root'"
        )
        credentials = BmcCredentials(username="root", password=credentials.password)
        site_config.host_bmc_credentials = credentials
        response = requests.get(
            url,
            auth=(credentials.username, credentials.password),
            verify=False,
            timeout=BMC_PROBE_TIMEOUT_SECONDS,
        )
    if not response.ok:
        _error_and_exit(
            f"Failed to authenticate to Lenovo BMC at {machine_info.host_bmc_ip} with the "
            f"configured username. Status code: {response.status_code}"
        )
    print("Lenovo BMC credentials validated")


def _dpu_bmc_credentials(site_config: SiteConfig, dpu_id: str) -> BmcCredentials:
    """Return the preloaded credentials for one DPU without logging them."""

    try:
        return site_config.dpu_bmc_credentials[dpu_id]
    except KeyError:
        _error_and_exit(f"No preloaded BMC credentials for DPU {dpu_id}")


def collect_ngc_uuids(
    resources: NetworkResourcesConfig,
    site_uuid: str,
    reconciled: network_resources.NetworkResourceIDs,
) -> NGCUUIDs:
    """Collect the cloud resource IDs used for provisioning.

    Args:
        resources: Validated network-resource configuration
        site_uuid: UUID of the NICo site under test
        reconciled: Validated VPC and VPC-prefix IDs
    Returns:
        NGCUUIDs: Object containing all required UUIDs
    """

    vpc_name = resources.vpc_name
    vpc_uuid = reconciled.vpc_uuid
    network_interface_name = resources.vpc_prefix_name
    network_interface_uuid = reconciled.vpc_prefix_uuid
    network_interface = {"vpcPrefixId": network_interface_uuid}

    print(f"{site_uuid=}")
    print(f"{vpc_name=}")
    print(f"{vpc_uuid=}")
    print(f"{network_interface_name=}")
    print(f"{network_interface=}")
    print("os_name='<ephemeral: created before provisioning>'")

    return NGCUUIDs(
        site_uuid=site_uuid,
        vpc_uuid=vpc_uuid,
        network_interface=network_interface,
        os_uuid=None,
    )


def verify_machine_has_no_instance_type(test_config: Config) -> None:
    """Fail before hardware changes if the target has an instance type.

    Targeted instance creation uses ``machineId``. NICo does not allow
    force-delete of a machine that has an instance-type assigned, so we have
    to remove any first. Require callers to resolve it explicitly rather than
    clearing it automatically to avoid unsafe deletion.
    """
    machine = admin_cli.get_machine_from_m_show(
        test_config.machine_under_test, allow_missing=True
    )
    if machine is None:
        _error_and_exit(
            f"Machine {test_config.machine_under_test} was not found; cannot verify " +
            "its instance-type assignment."
        )
    if "instance_type_id" not in machine:
        # Fail loud rather than treating a missing key as "unassigned" (None),
        # which would make this guard a silent no-op if the `machine show` shape
        # ever changed. A present key with value None is a valid unassigned tray.
        _error_and_exit(
            f"'machine show {test_config.machine_under_test}' response has no " +
            f"'instance_type_id' field (keys: {sorted(machine)}); cannot verify " +
            "the machine doesn't have an assigned instance type."
        )
    current = machine.get("instance_type_id")
    if current is not None:
        _error_and_exit(
            f"Machine {test_config.machine_under_test} is associated with instance " +
            f"type {current}. Point MLT at a different machine, or dissociate " +
            "this instance type before re-running."
        )
    print(f"Machine {test_config.machine_under_test} has no instance type; safe to proceed.")


def verify_initial_machine_state(test_config: Config, site_config: SiteConfig) -> None:
    """Check that the machine is in a good state before starting the
    test.

    Args:
        test_config: The test configuration containing machine
          information
        site_config: The site configuration
    """
    print("Checking machine is 'Ready'")
    if not admin_cli.check_machine_ready(test_config.machine_under_test):
        _error_and_exit("Machine is not Ready!")

    print("Checking machine is not in maintenance mode")
    if not admin_cli.check_machine_not_in_maintenance(test_config.machine_under_test):
        _error_and_exit("Machine is in maintenance mode!")

    print("Checking machine is not receiving a DPU FW update")
    if not admin_cli.check_machine_not_updating(test_config.machine_under_test):
        _error_and_exit("Machine is receiving a DPU FW update!")

    # The admin-cli (provider) and REST API (tenant) views
    # of the machine can diverge — e.g. admin-cli maintenance cleared while the
    # tenant machine is still 'Maintenance', which then fails instance creation
    # mid-provision with a confusing error. Since provisioning goes through the
    # REST API, verify the tenant view is 'Ready' too.
    print("Checking the cloud reports the machine 'Ready'")
    cloud_status = nico_rest.get_machine_status(test_config.machine_under_test, site_config.site)
    if cloud_status != "Ready":
        _error_and_exit(
            f"Cloud reports machine status '{cloud_status}', expected 'Ready' "
            f"(the provider view may differ — check the provider machine view)."
        )


def _factory_reset_dpu(
    test_config: Config, site_config: SiteConfig, machine_info: MachineInfo
) -> None:
    """Perform factory reset on DPU(s).

    Runs BIOS reset and BMC restart, then executes the factory reset.

    Args:
        test_config: The test configuration containing test settings
        site_config: The site configuration containing credentials
        machine_info: Information about the machine under test
    """
    for i, dpu_id in enumerate(machine_info.dpu_ids, start=1):
        target = ResetTarget(
            machine_id=dpu_id,
            bmc_ip=machine_info.dpu_info_map[dpu_id]["bmc_ip"],
            credentials=_dpu_bmc_credentials(site_config, dpu_id),
            label=f"DPU{i}",
        )
        try:
            DPU_RESET_DRIVER.reset_dpu(target)
        except ResetDriverError as error:
            _error_and_exit(
                str(error),
                set_maintenance=error.set_maintenance,
                machine_id=test_config.machine_under_test,
            )


def _factory_reset_host(
    test_config: Config, site_config: SiteConfig, machine_info: MachineInfo
) -> None:
    """Perform factory reset on host. Currently only supports Lenovo,
    Dell, and NVIDIA (GB200 compute trays).

    Args:
        test_config: The test configuration containing test settings
        site_config: The site configuration containing credentials
        machine_info: Information about the machine under test
    """
    driver = HOST_RESET_DRIVERS.get(machine_info.vendor)
    if driver is None:
        _error_and_exit(f"Factory reset not yet implemented for {machine_info.vendor} machines.")

    target = ResetTarget(
        machine_id=test_config.machine_under_test,
        bmc_ip=machine_info.host_bmc_ip,
        credentials=site_config.host_bmc_credentials,
        label="host",
    )
    try:
        driver.reset_host(target)
    except ResetDriverError as error:
        _error_and_exit(
            str(error),
            set_maintenance=error.set_maintenance,
            machine_id=test_config.machine_under_test,
        )


def perform_factory_reset(
    test_config: Config, site_config: SiteConfig, machine_info: MachineInfo
) -> None:
    """Perform a factory-reset on the DPU(s) and host.

    Args:
        test_config: The test configuration containing test settings
        site_config: The site configuration containing credentials
        machine_info: Information about the machine under test
    """
    _factory_reset_dpu(test_config, site_config, machine_info)
    _factory_reset_host(test_config, site_config, machine_info)


def _machine_bmc_endpoints(machine_info: MachineInfo) -> list[BmcEndpoint]:
    """Return the host and every DPU BMC endpoint."""

    endpoints = [
        BmcEndpoint(
            name="host",
            ip=machine_info.host_bmc_ip,
            mac=machine_info.host_bmc_mac,
        )
    ]
    endpoints.extend(
        BmcEndpoint(
            name=f"DPU {dpu_id}",
            ip=machine_info.dpu_info_map[dpu_id]["bmc_ip"],
            mac=machine_info.dpu_info_map[dpu_id]["bmc_mac"],
        )
        for dpu_id in machine_info.dpu_ids
    )
    return endpoints


def _verify_site_vault_credential(
    endpoint: BmcEndpoint,
    vault_client: SiteVaultClient,
) -> None:
    """Confirm NICo recreated one per-MAC credential without logging it."""

    vault_client.get_bmc_credentials(endpoint.mac)


def _refresh_bmc_endpoints(
    endpoints: set[BmcEndpoint],
) -> dict[BmcEndpoint, RecoveryFailure]:
    """Clear, refresh and verify endpoints, returning failures by endpoint."""

    failures: dict[BmcEndpoint, RecoveryFailure] = {}
    refreshed_endpoints: list[BmcEndpoint] = []
    for endpoint in sorted(endpoints, key=lambda item: item.ip):
        print(f"Clearing Site Explorer error for {endpoint.name} BMC at {endpoint.ip}")
        try:
            admin_cli.clear_site_explorer_error(endpoint.ip)
            print(f"Refreshing Site Explorer report for {endpoint.name} BMC at {endpoint.ip}")
            admin_cli.refresh_site_explorer_endpoint(endpoint.ip)
        except Exception as error:
            failures[endpoint] = RecoveryFailure(str(error))
            print(f"Recovery attempt failed for {endpoint.name} BMC at {endpoint.ip}: {error}")
            continue

        refreshed_endpoints.append(endpoint)

    if not refreshed_endpoints:
        return failures

    try:
        with SiteVaultClient() as vault_client:
            for endpoint in refreshed_endpoints:
                try:
                    _verify_site_vault_credential(endpoint, vault_client)
                except Exception as error:
                    failures[endpoint] = RecoveryFailure(
                        str(error), missing_site_vault_credential=True
                    )
                    print(
                        f"Recovery attempt failed for {endpoint.name} BMC at "
                        f"{endpoint.ip}: {error}"
                    )
                    continue

                print(
                    f"Site Vault credential was recreated for {endpoint.name} BMC at "
                    f"{endpoint.ip}"
                )
                print(
                    f"Site Explorer refresh succeeded for {endpoint.name} BMC at "
                    f"{endpoint.ip}"
                )
    except Exception as error:
        # Site Vault itself was unreachable, so nothing can be said about whether
        # NICo restored the individual records.
        for endpoint in refreshed_endpoints:
            failures[endpoint] = RecoveryFailure(str(error))
            print(
                f"Recovery attempt failed for {endpoint.name} BMC at "
                f"{endpoint.ip}: {error}"
            )

    return failures


def _wait_for_bmc_lockout_and_recover(
    machine_info: MachineInfo,
) -> None:
    """Recover from the BMC lockout race after factory reset.

    After a factory reset + force-delete with credential deletion, a NICo
    service may probe the factory-reset BMC with the (now stale) vault
    credential before the force-delete removes it. Enough failed logins lock
    the BMC root account for ~600s and put site explorer into an Unauthorized/
    AvoidLockout state, which blocks re-exploration. When that happens we wait
    out the lockout, clear the Site Explorer error and explicitly refresh the
    endpoint so exploration re-authenticates with the default credentials.
    A successful refresh is followed by a per-MAC Site Vault read on
    disconnected sites. If recovery fails, wait out one more lockout period
    and retry once before failing.

    This is a *race*, not a guarantee. If credential deletion wins, the BMC is
    never hammered, no lockout occurs, and re-exploration proceeds cleanly with
    the default credential (the good, increasingly common outcome). MLT accepts
    that path only after observing a successful report (and, on disconnected
    sites, the recreated Site Vault credential). Endpoints that enter lockout or
    remain unresolved are explicitly refreshed.

    Site vault credential recovery errors are differentiated from BMC recovery
    errors by log message.

    Args:
        machine_info: Host and DPU BMC addresses and MACs
    """
    BMC_LOCKOUT_SECONDS = 600
    POLL_INTERVAL = 30
    POLL_TIMEOUT = 600  # Account for at least one exploration interval
    AUTH_ERROR_TYPES = ("Unauthorized", "AvoidLockout")

    endpoints = _machine_bmc_endpoints(machine_info)
    endpoints_awaiting_error = {endpoint.ip: endpoint for endpoint in endpoints}
    endpoints_locked_out: set[BmcEndpoint] = set()
    poll_start = time.time()

    print(
        f"Polling Site Explorer for auth errors on "
        f"{len(endpoints_awaiting_error)} BMC endpoint(s)..."
    )
    while endpoints_awaiting_error and (time.time() - poll_start) < POLL_TIMEOUT:
        endpoints_to_verify: list[BmcEndpoint] = []
        for ip, endpoint in list(endpoints_awaiting_error.items()):
            report_succeeded, error_type = admin_cli.get_site_explorer_endpoint_status(ip)
            if error_type and any(t in error_type for t in AUTH_ERROR_TYPES):
                print(f"  Site Explorer reports '{error_type}' for {endpoint.name} BMC at {ip}")
                endpoints_awaiting_error.pop(ip)
                endpoints_locked_out.add(endpoint)
            elif report_succeeded:
                endpoints_to_verify.append(endpoint)
            else:
                print(
                    f"  No auth error yet for {endpoint.name} BMC at {ip} (current: {error_type})"
                )

        if endpoints_to_verify:
            try:
                with SiteVaultClient() as vault_client:
                    for endpoint in endpoints_to_verify:
                        try:
                            _verify_site_vault_credential(endpoint, vault_client)
                        except SiteVaultError as error:
                            print(
                                f"  Site Explorer succeeded for {endpoint.name} BMC at "
                                f"{endpoint.ip}, but its Site Vault credential is not "
                                f"ready: {error}"
                            )
                            continue

                        print(
                            f"  Site Explorer successfully recovered {endpoint.name} "
                            f"BMC at {endpoint.ip}"
                        )
                        endpoints_awaiting_error.pop(endpoint.ip)
            except SiteVaultError as error:
                for endpoint in endpoints_to_verify:
                    if endpoint.ip in endpoints_awaiting_error:
                        print(
                            f"  Site Explorer succeeded for {endpoint.name} BMC at "
                            f"{endpoint.ip}, "
                            f"but its Site Vault credential is not ready: {error}"
                        )
        if endpoints_awaiting_error:
            time.sleep(POLL_INTERVAL)

    if not endpoints_locked_out and not endpoints_awaiting_error:
        print(
            "No BMC lockout observed; "
            "credential deletion beat the stale-cred probe, so re-exploration can "
            "proceed with default creds. Every endpoint recovered successfully."
        )
        return

    if endpoints_locked_out and endpoints_awaiting_error:
        print(
            f"Proceeding: {sorted(endpoint.ip for endpoint in endpoints_locked_out)} "
            f"entered a lockout state; {sorted(endpoints_awaiting_error)} remained unresolved."
        )

    if endpoints_locked_out:
        print(
            f"Waiting {BMC_LOCKOUT_SECONDS}s for BMC account lockout to expire on "
            f"{sorted(endpoint.ip for endpoint in endpoints_locked_out)}..."
        )
        time.sleep(BMC_LOCKOUT_SECONDS)

    endpoints_to_refresh = endpoints_locked_out | set(endpoints_awaiting_error.values())
    failures = _refresh_bmc_endpoints(endpoints_to_refresh)
    if not failures:
        return

    print(
        f"Waiting {BMC_LOCKOUT_SECONDS}s before the final recovery attempt for "
        f"{sorted(endpoint.ip for endpoint in failures)}..."
    )
    time.sleep(BMC_LOCKOUT_SECONDS)

    final_failures = _refresh_bmc_endpoints(set(failures))
    if final_failures:
        details = "; ".join(
            f"{endpoint.name} {endpoint.ip}: {failure.message}"
            for endpoint, failure in sorted(final_failures.items(), key=lambda item: item[0].ip)
        )
        # Site Explorer refreshing cleanly while the per-MAC record stays missing is
        # a different fault from a BMC that never recovered, and on the site-wide
        # fallback path it means NICo re-ingested without restoring the credential.
        # Name it, rather than reporting every failure as a Site Explorer problem.
        if all(failure.missing_site_vault_credential for failure in final_failures.values()):
            _error_and_exit(
                "Site Explorer refreshed every BMC, but NICo did not restore their "
                f"per-MAC Site Vault credentials: {details}"
            )
        _error_and_exit(f"BMC recovery failed after two Site Explorer refresh attempts: {details}")


def verify_bmc_rotation_converged(machine_info: MachineInfo) -> None:
    """Stop unless the host and every DPU BMC are at the live credential version."""

    endpoints = {
        "host": machine_info.host_bmc_mac,
        **{
            f"DPU {dpu_id}": machine_info.dpu_info_map[dpu_id]["bmc_mac"]
            for dpu_id in machine_info.dpu_ids
        },
    }

    print("Checking every BMC is converged on the live site-wide credential version")
    for name, bmc_mac in endpoints.items():
        try:
            status = admin_cli.get_bmc_rotation_device_status(bmc_mac)
        except (subprocess.CalledProcessError, ValueError) as error:
            _error_and_exit(
                f"Could not read rotation status for {name} BMC {bmc_mac}: {error}"
            )

        if status.get("converged"):
            print(
                f"- PASS: {name} BMC {bmc_mac} is converged at version "
                f"{status.get('current_version')}"
            )
            continue

        detail = (
            f"current version {status.get('current_version')}, "
            f"site target version {status.get('target_version')}"
        )
        if status.get("quarantined"):
            detail += f", quarantined until {status.get('quarantined_until')}"
        if status.get("last_error"):
            detail += f", last error: {status.get('last_error')}"
        _error_and_exit(
            f"{name} BMC {bmc_mac} has not converged on the live site-wide BMC credential "
            f"({detail}). Site Explorer's fallback only accepts the live version, so this "
            "machine could not be re-ingested after its credentials are deleted. Re-explore "
            "the endpoint until it converges, then retry."
        )


def verify_bmc_sitewide_credentials(site_config: SiteConfig, machine_info: MachineInfo) -> None:
    """Prove the host and every DPU BMC accept the site-wide root password."""
    password = site_config.sitewide_bmc_password
    if password is None:
        _error_and_exit(
            "No site-wide BMC password was loaded; site-wide fallback verification "
            "requires lifecycle.test_sitewide_bmc_fallback on a disconnected site"
        )

    endpoints = [
        (
            "host",
            machine_info.host_bmc_ip,
            site_config.host_bmc_credentials.username,
        ),
        *[
            (
                f"DPU {dpu_id}",
                machine_info.dpu_info_map[dpu_id]["bmc_ip"],
                _dpu_bmc_credentials(site_config, dpu_id).username,
            )
            for dpu_id in machine_info.dpu_ids
        ],
    ]

    print("Validating site-wide host and DPU BMC credentials")
    for name, bmc_ip, username in endpoints:
        try:
            # Site Explorer pairs the site-wide password with the BMC's own
            # username, so retain each endpoint's per-MAC username.
            admin_cli.get_bmc_accounts(bmc_ip, username, password)
        except subprocess.CalledProcessError as e:
            _error_and_exit(
                f"{name} BMC {bmc_ip} did not accept the configured site-wide "
                f"credentials: {e.stderr or e}"
            )
        print(f"- PASS: {name} BMC {bmc_ip} accepted the site-wide credentials")


def assert_per_mac_credentials_missing(machine_info: MachineInfo) -> None:
    """Probe bmc credentials for host and each dpu from site vault to ensure
    successful deletion.
    """
    bmc_macs = {
        "host": machine_info.host_bmc_mac,
        **{
            f"DPU {dpu_id}": machine_info.dpu_info_map[dpu_id]["bmc_mac"]
            for dpu_id in machine_info.dpu_ids
        },
    }

    with SiteVaultClient() as vault_client:
        for name, bmc_mac in bmc_macs.items():
            try:
                _ = vault_client.get_bmc_credentials(bmc_mac)
            except SiteVaultCredentialNotFound:
                continue
            else:
                _error_and_exit(
                    f"{name} ({bmc_mac}) per-BMC credentials still exist" +
                    " after force-delete"
                    )

    print("All per-BMC credentials were successfully deleted from Site Vault")


def force_delete_and_await_reingestion(
    test_config: Config,
    site_config: SiteConfig,
    machine_info: MachineInfo,
    *,
    run_started_at: float | None = None,
) -> None:
    """Perform force delete operation and wait for machine to reach
    Ready state.

    Args:
        test_config: The test configuration containing test settings
        site_config: The site configuration containing credentials
        machine_info: Information about the machine under test
        run_started_at: Monotonic timestamp captured when this MLT run started
    """
    ingestion_started_at = datetime.datetime.now(datetime.timezone.utc)
    print(f"Force-deleting machine {test_config.machine_under_test}")
    # The fallback test skips the factory reset but still needs the per-BMC Vault
    # entries gone, or Site Explorer finds them on re-ingestion and never reaches
    # the site-wide fallback.
    delete_creds = (
        not test_config.skip_factory_reset or test_config.test_sitewide_bmc_fallback
    )
    admin_cli.force_delete_machine(test_config.machine_under_test, delete_creds=delete_creds)
    if delete_creds:
        if test_config.test_sitewide_bmc_fallback:
            # Must verify that creds have been removed to distinguish "site-wide
            # fallback worked" from "creds were never deleted"
            assert_per_mac_credentials_missing(machine_info)

        # After a force delete with credential deletion, NICo may have tried
        # stale vault credentials against the BMC too many times before the force delete
        # removed them, locking out the root account for 600s and putting site explorer into
        # an AvoidLockout state. Best-effort recovery: if that lockout is observed, wait it
        # out, clear the error, explicitly refresh the endpoint and verify that NICo
        # recreated the credential; if not (credential deletion won the race), proceed
        # without failing. See _wait_for_bmc_lockout_and_recover.
        _wait_for_bmc_lockout_and_recover(machine_info)

    # Machine ingestion
    # If there is a failure anywhere here, attempt to put the machine into maintenance mode
    # for investigation (this will prevent future runs affecting the machine).
    try:
        # Wait for HostInitializing state
        print("Waiting for NICo to report DPU in any 'HostInitializing' state")
        admin_cli.wait_for_machine_hostinitializing(
            machine_info.machine_under_test_dpu, timeout=timeouts.WAIT_FOR_HOSTINIT
        )

        # Wait for 'Ready' state
        print("Waiting for NICo to report DPU 'Ready'")
        admin_cli.wait_for_machine_ready(
            machine_info.machine_under_test_dpu, timeout=timeouts.WAIT_FOR_READY
        )

        # After the DPU state gets to Ready, allow for the possibility that NICo tries to upgrade
        # the DPU FW. Then confirm the managed host and Cloud machine states both show Ready too.
        print(
            "Sleeping for 2 minutes to allow NICo to possibly grab the machine for "
            "a DPU FW upgrade..."
        )
        time.sleep(60 * 2)

        print("Waiting for the machine not to be receiving a DPU FW update from NICo...")
        admin_cli.wait_for_machine_not_updating(test_config.machine_under_test, timeout=60 * 90)

        print("Checking that NICo reports the managed host Ready")
        admin_cli.check_machine_ready(test_config.machine_under_test)

        if _restart_request_expected(test_config):
            print("Checking that NICo requested its ingestion-triggered host reboot")
            admin_cli.assert_host_restart_requested(
                test_config.machine_under_test,
                ingestion_started_at,
            )

        print("Waiting for the Cloud to also report machine Ready")
        nico_rest.wait_for_machine_ready(
            test_config.machine_under_test, site_config.site, timeout=60 * 10
        )

        # Just verifying that the password is unchanged after re-ingestion
        if test_config.test_sitewide_bmc_fallback:
            verify_bmc_rotation_converged(machine_info)
            verify_bmc_sitewide_credentials(site_config, machine_info)

    except Exception as e:
        if isinstance(e, TIMEOUT_EXCEPTIONS):
            diagnostics.collect_timeout_diagnostics(
                config=test_config.diagnostics,
                stage="ingestion",
                host_id=test_config.machine_under_test,
                dpu_ids=machine_info.dpu_ids,
                site=site_config.site,
                timeout_error=e,
                run_started_at=run_started_at,
            )
        # We have to use managed host ID to do this, not DPU ID, but this may not exist
        # in the database yet depending on where the process got to before it failed.
        try:
            _error_and_exit(str(e), set_maintenance=True, machine_id=test_config.machine_under_test)
        except Exception:
            print(
                "Setting maintenance mode failed, trying again using predicted host id",
                file=sys.stderr,
            )
            _error_and_exit(
                str(e),
                set_maintenance=True,
                machine_id=machine_info.machine_under_test_predicted_host,
            )


def create_instance_and_verify(
    test_config: Config,
    site_config: SiteConfig,
    machine_info: MachineInfo,
    ngc_uuids: NGCUUIDs,
    ssh_private_key: paramiko.PKey,
    *,
    run_started_at: float | None = None,
) -> str | None:
    """Create an instance, wait for it to be ready, and verify SSH
    access.

    Args:
        test_config: The test configuration containing test settings
        site_config: The site configuration containing site information
        machine_info: The discovered host and DPU identifiers
        ngc_uuids: Object containing all required NGC UUIDs
        ssh_private_key: Per-run key generated with the ephemeral OS definition
        run_started_at: Monotonic timestamp captured when this MLT run started
    Returns:
        str: The instance UUID
    """
    instance_uuid = None
    try:
        # Create instance
        print("Creating an instance on the machine")
        instance_name = f"mlt-instance-{test_config.expected_dpu_count}-{str(uuid.uuid4())[:8]}"
        if ngc_uuids.os_uuid is None:
            raise RuntimeError("No operating system is configured for instance creation")
        instance = nico_rest.create_instance(
            instance_name=instance_name,
            machine_id=test_config.machine_under_test,
            network_interface=ngc_uuids.network_interface,
            operating_system_uuid=ngc_uuids.os_uuid,
            virtual_private_cloud_uuid=ngc_uuids.vpc_uuid,
        )
        instance_uuid = instance["id"]
        print(f"Instance {instance_uuid} creation success")

        print("Waiting for NICo to report machine 'Assigned/Ready'")
        admin_cli.wait_for_machine_assigned_ready(test_config.machine_under_test, timeout=60 * 10)

        # With phone-home enabled, the REST layer will only report the instance
        # 'Ready' once it has booted and reported back.
        print("Waiting for the cloud to report instance 'Ready' to the tenant")
        nico_rest.wait_for_instance_ready(
            instance_uuid, site_config.site, timeout=timeouts.WAIT_FOR_INSTANCE
        )

        instance_ip_address = nico_rest.wait_for_instance_ip(
            instance_uuid, ngc_uuids.network_interface, timeout=60 * 20
        )

        print(f"Testing SSH connection to the instance {instance_uuid} at {instance_ip_address}")
        network.wait_for_host_port(instance_ip_address, 22, max_retries=40)
        # A listening SSH port can still belong to an intermediate boot
        # environment, or cloud-init may not have installed the per-run key yet.
        ssh_deadline = time.time() + timeouts.WAIT_FOR_SSH
        while True:
            try:
                with paramiko.SSHClient() as ssh_client:
                    ssh_client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
                    ssh_client.connect(
                        instance_ip_address,
                        pkey=ssh_private_key,
                        username="machine-lifecycle-test-user",
                        timeout=30,
                    )

                    command = "uptime"
                    print(
                        f"Executing command: {command} on instance {instance_uuid} "
                        f"at {instance_ip_address}"
                    )
                    i, o, e = ssh_client.exec_command(command)
                    stdout = o.readlines()
                    stderr = e.readlines()
                    exit_status = o.channel.recv_exit_status()
                    print(f"{command!r} stdout: {stdout}")
                    print(f"{command!r} stderr: {stderr}")
                    if exit_status != 0:
                        print(f"{command!r} exited with status {exit_status}", file=sys.stderr)

                return instance_uuid
            except (paramiko.SSHException, OSError, EOFError) as ssh_error:
                if time.time() >= ssh_deadline:
                    raise
                print(
                    f"SSH to {instance_ip_address} not ready yet "
                    f"({type(ssh_error).__name__}: {ssh_error}); retrying in 30s"
                )
                time.sleep(30)

    except Exception as e:
        if isinstance(e, TIMEOUT_EXCEPTIONS):
            diagnostics.collect_timeout_diagnostics(
                config=test_config.diagnostics,
                stage="assignment",
                host_id=test_config.machine_under_test,
                dpu_ids=machine_info.dpu_ids,
                site=site_config.site,
                timeout_error=e,
                instance_id=instance_uuid,
                run_started_at=run_started_at,
            )
        _error_and_exit(
            f"Exception during instance creation/verification: {e}",
            set_maintenance=True,
            machine_id=test_config.machine_under_test,
        )
        return None


def _retain_instance_and_exit(ngc_uuids: NGCUUIDs, instance_uuid: str) -> NoReturn:
    """Leave the instance running for manual debugging and fail the run.

    This deliberately skips deprovisioning, so the machine stays allocated
    until someone deletes it by hand. The run is failed rather than passed
    because it did not exercise the second half of the lifecycle: a green tick
    here would later be mistaken for a full end-to-end validation.
    """
    try:
        instance_ip_address = nico_rest.wait_for_instance_ip(
            instance_uuid, ngc_uuids.network_interface, timeout=60
        )
    except Exception as error:  # noqa: BLE001 - never mask the retain notice
        instance_ip_address = f"<lookup failed: {error}>"

    print("=" * 72)
    print("debug.keep_instance is set: NOT deleting the instance.")
    print(f"  instance: {instance_uuid}")
    print(f"  address:  {instance_ip_address}")
    print("Delete it by hand when you are finished, or the machine stays")
    print("allocated and the next run on this host will not be able to start.")
    print("=" * 72)
    _error_and_exit(
        "Instance deliberately retained by debug.keep_instance; the deletion "
        "half of the lifecycle was not tested"
    )


def delete_instance_and_verify(
    test_config: Config, site_config: SiteConfig, ngc_uuids: NGCUUIDs, instance_uuid: str
) -> None:
    """Delete the instance and wait for deprovisioning to complete.

    Args:
        test_config: The test configuration containing test settings
        site_config: The site configuration
        ngc_uuids: Object containing all required NGC UUIDs
        instance_uuid: UUID of the instance to delete
    """
    try:
        print("Deleting the instance")
        nico_rest.delete_instance(instance_uuid)

        print("Waiting for the instance to be deleted...")
        nico_rest.wait_for_vpc_to_not_contain_instance(
            ngc_uuids.site_uuid, ngc_uuids.vpc_uuid, instance_uuid, timeout=60 * 90
        )

        print("Waiting for NICo to report the managed host 'Ready'...")
        admin_cli.wait_for_machine_ready(test_config.machine_under_test, timeout=60 * 120)
    except Exception as e:
        _error_and_exit(
            f"Exception during instance deletion/deprovisioning: {e}",
            set_maintenance=True,
            machine_id=test_config.machine_under_test,
        )


if __name__ == "__main__":
    test_machine_lifecycle()
