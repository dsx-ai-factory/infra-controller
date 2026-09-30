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

"""Functions for talking to the nico-admin-cli utility."""

import contextlib
import datetime
import json
import os
import secrets
import subprocess
import sys
import time
from dataclasses import dataclass

from lib import kubectl
from lib.config import GrpcApiConfig
from lib.site_vault import SiteVaultClient

# The admin-cli runs IN-POD via `kubectl exec` into the API deployment, so the
# binary always matches the running build and no local copy is needed. Requires
# kubectl on PATH and the runner service account's pods/exec RBAC in the API
# namespace.
ADMIN_CLI_K8S_NAMESPACE = os.getenv("ADMIN_CLI_K8S_NAMESPACE", "forge-system")
ADMIN_CLI_K8S_DEPLOYMENT = os.getenv("ADMIN_CLI_K8S_DEPLOYMENT", "nico-api")
ADMIN_CLI_IN_POD_PATH = os.getenv("ADMIN_CLI_IN_POD_PATH", "/opt/nico/nico-admin-cli")
# Ceiling on one invocation. Given an address its server certificate does not
# cover, the client retries instead of failing, so without this a misconfigured
# address hangs until the CI job's own limit. Every command here is a single
# request; waiting for a machine to change state loops rather than blocking.
ADMIN_CLI_TIMEOUT_SECONDS = int(os.getenv("ADMIN_CLI_TIMEOUT_SECONDS", "300"))


# /dev/shm is tmpfs, so the private key stays in memory and never reaches the
# node's disk; the container's /tmp is the disk-backed overlay.
_STAGING_ROOT = "/dev/shm"


@dataclass(frozen=True)
class _StagedCertificate:
    """A client certificate written into a pod, and where it was written.

    All three travel together so a partly-populated identity cannot be built.
    """

    pod: str
    cert_path: str
    key_path: str


@dataclass(frozen=True)
class _GrpcApiTarget:
    """The address, and optionally the identity, invocations should carry."""

    api_url: str
    root_ca_path: str
    certificate: _StagedCertificate | None = None


# Set for the duration of `client_identity`. Every public function here routes
# through the prefix helper, so the alternative is a parameter on all of them.
_grpc_api_target: _GrpcApiTarget | None = None

# The certificate material, kept for the duration of the run so the identity can
# be re-staged when the API pod is replaced under us. The in-pod paths in
# `_grpc_api_target` are worthless on their own: a new pod has neither the files
# nor the name we recorded. Held as (certificate, private_key).
_staged_identity: tuple[str, str] | None = None
# The API build seen when the identity was last staged, so a replacement pod
# running a different build can be reported rather than silently accepted.
_staged_api_build: str | None = None

# Every (pod, directory) the identity has been written to during this run, so
# none is left holding a private key. Recorded before the write rather than
# after, because a write that fails part-way still creates the directory.
_staged_locations: list[tuple[str, str]] = []


def _admin_cli_command_prefix() -> list[str]:
    """Return the argv prefix for an admin-cli invocation.

    ``kubectl exec`` into the API pod. Global flags / subcommands are appended
    after this prefix (after the ``--`` and the in-pod binary).
    """
    target = _grpc_api_target
    certificate = None if target is None else target.certificate
    # A deployment reference may resolve to a replica holding no credentials.
    pod_reference = (
        f"deployment/{ADMIN_CLI_K8S_DEPLOYMENT}"
        if certificate is None
        else certificate.pod
    )

    prefix = [
        "kubectl",
        "exec",
        "-n",
        ADMIN_CLI_K8S_NAMESPACE,
        pod_reference,
        "--",
        ADMIN_CLI_IN_POD_PATH,
    ]
    if target is None:
        return prefix

    prefix += ["--api-url", target.api_url, "--root-ca-path", target.root_ca_path]
    if certificate is not None:
        prefix += [
            "--client-cert-path",
            certificate.cert_path,
            "--client-key-path",
            certificate.key_path,
        ]
    return prefix


@contextlib.contextmanager
def client_identity(grpc_api_config: GrpcApiConfig | None):
    """Address the gRPC API explicitly, and present a client certificate to it.

    Where a certificate is configured, mints one and stages it into the API
    pod, because the caller's credential has to be readable by the binary
    executed there. It is removed afterwards, including when a stage raises.
    Yields unchanged when nothing is configured.
    """
    global _grpc_api_target, _staged_identity, _staged_api_build

    if grpc_api_config is None:
        yield
        return

    if grpc_api_config.client_certificate is None:
        _grpc_api_target = _GrpcApiTarget(
            api_url=grpc_api_config.url, root_ca_path=grpc_api_config.root_ca_path
        )
        try:
            yield
        finally:
            _grpc_api_target = None
        return

    certificate_config = grpc_api_config.client_certificate
    with SiteVaultClient() as vault_client:
        certificate = vault_client.issue_client_certificate(
            pki_mount=certificate_config.vault_pki_mount,
            pki_role=certificate_config.vault_pki_role,
            common_name=certificate_config.common_name,
            ttl=certificate_config.ttl,
        )

    pod = kubectl.get_deployment_pod(ADMIN_CLI_K8S_NAMESPACE, ADMIN_CLI_K8S_DEPLOYMENT)
    directory = f"{_STAGING_ROOT}/mlt-{secrets.token_hex(8)}"
    staged = _StagedCertificate(
        pod=pod,
        cert_path=f"{directory}/client.crt",
        key_path=f"{directory}/client.key",
    )

    print(f"Staging an admin-cli client identity in {ADMIN_CLI_K8S_NAMESPACE}/{pod}")
    _staged_locations.clear()
    _staged_locations.append((pod, directory))
    try:
        kubectl.write_pod_file(
            ADMIN_CLI_K8S_NAMESPACE, pod, staged.key_path, certificate.private_key
        )
        kubectl.write_pod_file(
            ADMIN_CLI_K8S_NAMESPACE, pod, staged.cert_path, certificate.certificate
        )
        _grpc_api_target = _GrpcApiTarget(
            api_url=grpc_api_config.url,
            root_ca_path=grpc_api_config.root_ca_path,
            certificate=staged,
        )
        _staged_identity = (certificate.certificate, certificate.private_key)
        _staged_api_build = _read_api_build_version()
        yield
    finally:
        # Every location, not just the one the run ended on: a re-stage moves
        # the identity, and the pod it moved off may still be running.
        locations = list(_staged_locations)
        _staged_locations.clear()
        _grpc_api_target = None
        _staged_identity = None
        _staged_api_build = None
        for staged_pod, staged_directory in locations:
            _discard_staged_location(staged_pod, staged_directory)


def assert_host_restart_requested(machine_id: str, since: datetime.datetime) -> None:
    """Assert NICo requested a host reboot during the current ingestion."""
    machine = get_machine_from_mh_show(machine_id)
    requested = machine.get("host_last_reboot_requested_time_and_mode")
    try:
        requested_time, requested_mode = requested.rsplit("/", 1)
        if requested_mode != "Reboot" or not requested_time.endswith(" UTC"):
            raise ValueError
        requested_at = datetime.datetime.fromisoformat(requested_time.removesuffix(" UTC")).replace(
            tzinfo=datetime.timezone.utc
        )
    except (AttributeError, ValueError):
        raise AssertionError(
            f"NICo did not record a managed Reboot request for host {machine_id}: {requested!r}"
        ) from None
    if requested_at < since.astimezone(datetime.timezone.utc):
        raise AssertionError(
            f"NICo's managed Reboot request for host {machine_id} predates this ingestion: "
            f"{requested!r}"
        )

    print(f"NICo recorded the ingestion-triggered Reboot request for host {machine_id}")


def wait_for_machine_ready(machine_id: str, timeout: int) -> None:
    """Check repeatedly until the specified machine is in Ready state,
    for up to `timeout` seconds.
    """
    wait_for_state(machine_id, "Ready", timeout, allow_missing_machine=True)


def wait_for_machine_assigned_ready(machine_id: str, timeout: int) -> None:
    """Check repeatedly until the specified machine is in Assigned/Ready
    state, for up to `timeout` seconds.
    """
    wait_for_state(machine_id, "Assigned/Ready", timeout, allow_missing_machine=True)


def wait_for_machine_hostinitializing(machine_id: str, timeout: int) -> None:
    """Check repeatedly until the specified machine reaches any
    HostInitializing state, for up to `timeout` seconds.
    """
    wait_for_state(machine_id, "HostInitializing", timeout, allow_missing_machine=True)


def wait_for_machine_not_in_maintenance(machine_id: str, timeout: int) -> None:
    """Check repeatedly until the specified machine is not in
    maintenance, for up to `timeout` seconds.
    """
    end = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=timeout)
    while (now := datetime.datetime.now(datetime.timezone.utc)) < end:
        now_formatted = now.strftime("%Y-%m-%d %H:%M:%S")
        not_in_maintenance = check_machine_not_in_maintenance(machine_id)
        if not_in_maintenance:
            print(f"{now_formatted}: machine {machine_id} is not in maintenance!")
            return
        else:
            print(f"{now_formatted}: machine {machine_id} not out of maintenance yet")
            time.sleep(60)
    else:
        raise TimeoutError(f"Machine id {machine_id} still in maintenance after {timeout} seconds")


def wait_for_machine_not_updating(machine_id: str, timeout: int) -> None:
    """Check repeatedly until the specified host is not receiving a DPU
    FW update from NICo, for up to `timeout` seconds.
    """
    end = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=timeout)
    while (now := datetime.datetime.now(datetime.timezone.utc)) < end:
        now_formatted = now.strftime("%Y-%m-%d %H:%M:%S")
        not_updating = check_machine_not_updating(machine_id)
        if not_updating:
            print(f"{now_formatted}: machine is not receiving a DPU FW update.")
            return
        else:
            print(f"{now_formatted}: machine is receiving a DPU FW update.")
            time.sleep(60)
    else:
        raise TimeoutError(f"Machine still receiving DPU FW update after {timeout} seconds.")


def wait_for_state(
    machine_id: str, desired_state: str, timeout: int, allow_missing_machine: bool = False
) -> None:
    """Check repeatedly until the specified machine is in a specific
    state, for up to `timeout` seconds.

    desired_state: Can be a partial state name or a full state name.
    "Failed/Discovery" is a (bad) terminal state.
    If we get in any state starting "Failed", raise an exception to fail
      fast.
    """
    if not desired_state:
        raise ValueError("No desired state specified")
    end = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(seconds=timeout)
    while (now := datetime.datetime.now(datetime.timezone.utc)) < end:
        now_formatted = now.strftime("%Y-%m-%d %H:%M:%S")
        state = get_machine_state(machine_id, allow_missing_machine)
        if state.startswith("Failed"):
            raise Exception(f"Failure! Machine went into {state}.")
        # Allow partial state names to be queried
        if desired_state in state:
            print(f"{now_formatted}: machine reached desired state ({desired_state})!")
            return
        else:
            print(
                f"{now_formatted}: machine not in desired state ({desired_state}) "
                f"yet, current state: {state}"
            )
            time.sleep(60)
    else:
        raise TimeoutError(
            f"Machine did not get to desired state ({desired_state}) within {timeout} seconds"
        )


def check_machine_ready(machine_id: str) -> bool:
    """Check once if the specified machine is in ready state."""
    state = get_machine_state(machine_id)
    print(f"{machine_id} state '{state}'")
    return state == "Ready"


def get_machine_state(machine_id: str, allow_missing_machine: bool = False) -> str:
    """Get the current state for the specified machine."""
    machine = get_machine_from_m_show(machine_id, allow_missing_machine)
    if machine is None and allow_missing_machine:
        return "<Missing>"
    return machine["state"]


def get_machine_vendor(machine_id: str) -> str:
    """Get the vendor name of the specified machine."""
    result = run_admin_cli(["machine", "show", machine_id])
    return result["discovery_info"]["dmi_data"]["sys_vendor"]


def _get_machine_from_json(machine_id: str, machine_json: dict) -> dict | None:
    """Given JSON managed-host show output, return just the machine we
    want.

    Can provide any type of machine_id (Host, DPU, PredictedHost).
    If the machine is not found, return None.
    """
    if len(machine_id) < 6:
        raise ValueError(f"Invalid machine id: '{machine_id}'")
    if machine_id[5] in "hp":
        # Host or PredictedHost
        try:
            return [mach for mach in machine_json if mach["machine_id"] == machine_id][0]
        except IndexError:
            return None

    elif machine_id[5] == "d":
        # DPU
        for mach in machine_json:
            for dpu in mach["dpus"]:
                if dpu["machine_id"] == machine_id:
                    return mach
        else:
            return None
    return None


def check_machine_not_updating(host_id: str) -> bool:
    """Check once if the specified host is receiving a DPU FW update
    from NICo. This is indicated by a health alert labelled with id
    'HostUpdateInProgress'.

    :param host_id: Note: must not be the DPU ID
    """
    update_alert = "HostUpdateInProgress"
    machine = get_machine_from_mh_show(host_id)
    health_alerts = [alert["id"] for alert in machine["health"]["alerts"]]
    return update_alert not in health_alerts


def check_machine_not_in_maintenance(machine_id: str) -> bool:
    """Check once if the specified machine is in ready state."""
    machine = get_machine_from_mh_show(machine_id)
    print(
        f"{machine_id} maintenance_start_time '{machine['maintenance_start_time']}'"
        f" maintenance_reference '{machine['maintenance_reference']}'"
    )
    return machine["maintenance_start_time"] is None


def put_machine_into_maintenance_mode(machine_id: str) -> None:
    """Put the specified machine into maintenance mode.

    Note: the cloud can also put a machine into maintenance mode but that
    turned out not to be very useful for machine lifecycle testing
    because it can only do so for a machine it knows about. There are many states we can get into where
    this is not the case.
    """
    if os.environ.get("CI", "false") == "true":
        job_name = os.environ.get("CI_JOB_NAME", "Unknown Job Name")
        job_url = os.environ.get("CI_JOB_URL", "Unknown Job URL")
        reason = f"CI job '{job_name}' requested maintenance mode ({job_url})"
    else:
        reason = "Maintenance requested via tests/machine-lifecycle/admin_cli.py"
    run_admin_cli(
        ["managed-host", "maintenance", "on", "--host", machine_id, "--reference", reason],
        no_json=True,
    )


def get_machine_from_mh_show(
    machine_id: str, allow_missing: bool = False, timeout: int | None = None
) -> dict | None:
    """Get JSON formatted machine information from `managed-host show`
    output. This will only work after the host and DPU have been paired
    to create a managed host.
    """
    try:
        result = run_admin_cli(
            ["managed-host", "show", machine_id], timeout=timeout
        )
    except subprocess.CalledProcessError as error:
        if "managed host not found" not in (error.stderr or "").lower():
            raise
        if allow_missing:
            return None
        raise Exception(f"Machine with id {machine_id} not found.") from error
    if isinstance(result, dict) and "managed_hosts" in result:
        result = result["managed_hosts"]
    if isinstance(result, list):
        machine = _get_machine_from_json(machine_id, result)
    else:
        machine = result or None
    if machine is None:
        if not allow_missing:
            raise Exception(f"Machine with id {machine_id} not found.")
    return machine


def get_machine_from_m_show(
    machine_id: str, allow_missing: bool = False, timeout: int | None = None
) -> dict | None:
    """Get JSON formatted machine information from `machine show`
    output. This will work at any point once the machine has been
    discovered and given an ID.
    """
    try:
        machine = run_admin_cli(["machine", "show", machine_id], timeout=timeout)
    except subprocess.CalledProcessError as error:
        if allow_missing:
            stderr = (error.stderr or "").lower()
            if "machine" in stderr and "not found" in stderr:
                return None
            raise
        raise Exception(f"Machine with id {machine_id} not found.")
    return machine


def get_dpu_network_config(machine_id: str, timeout: int | None = None) -> dict:
    """Return the desired network configuration for one DPU."""
    result = run_admin_cli(
        ["dpu", "network", "config", "--machine-id", machine_id], timeout=timeout
    )
    if not isinstance(result, dict):
        raise ValueError(f"No network configuration returned for DPU {machine_id}")
    return result


def get_dpu_network_status_text(timeout: int | None = None) -> str:
    """Return the reported network-status table for all DPUs.

    The current admin-cli command renders this command as a table even when a
    global JSON output format is selected. Diagnostics filter the returned text
    down to the target DPU rows before persisting it.
    """
    return run_admin_cli_text(["dpu", "network", "status"], timeout=timeout)


def force_delete_machine(machine_id: str, delete_creds: bool = False) -> None:
    """Force-delete the specified machine.

    Enable `delete_creds` if the machine has been factory-reset or when a
    test intentionally needs to remove its per-BMC Vault entries. Always
    print out the machine information first.
    """
    print("Machine information before force-delete:")
    run_admin_cli(["managed-host", "show", machine_id], no_json=True)

    print("Performing force-delete...")
    args = ["machine", "force-delete"]
    if delete_creds:
        args.extend(["--delete-bmc-credentials"])
    args.extend(["--machine", machine_id])
    run_admin_cli(args, no_json=True)


def power_off_host(host_bmc_ip: str, host_bmc_username: str, host_bmc_password: str) -> None:
    """Power off a machine using redfish."""
    print("Performing host redfish force-off")
    run_admin_cli(
        [
            "redfish",
            "--address",
            host_bmc_ip,
            "--username",
            host_bmc_username,
            "--password",
            host_bmc_password,
            "force-off",
        ],
        no_json=True,
    )


def power_on_host(host_bmc_ip: str, host_bmc_username: str, host_bmc_password: str) -> None:
    """Power on a machine using redfish."""
    print("Performing host redfish on")
    run_admin_cli(
        [
            "redfish",
            "--address",
            host_bmc_ip,
            "--username",
            host_bmc_username,
            "--password",
            host_bmc_password,
            "on",
        ],
        no_json=True,
    )


def restart_machine(machine_id: str) -> None:
    """Restart a machine (host or DPU) via redfish ForceRestart"""
    run_admin_cli(
        [
            "machine",
            "reboot",
            "--machine",
            machine_id,
        ],
        no_json=True,
    )


def clear_host_bios_password(machine_id: str) -> None:
    """Remove the BIOS password from a host"""
    run_admin_cli(["host", "clear-uefi-password", "--query", machine_id], no_json=True)


def restart_bmc(machine_id: str, retries: int = 3, delay: int = 10) -> None:
    """Restart a BMC (DPU or host). Retries on failure since the BMC may
    not be fully ready after prior operations.

    :param machine_id: The ID of the machine whose BMC to restart.
    :param retries: Number of retry attempts (default 3).
    :param delay: Seconds to wait between retries (default 10).
    """
    for attempt in range(1, retries + 1):
        try:
            run_admin_cli(
                [
                    "bmc-machine",
                    "bmc-reset",
                    "--machine",
                    machine_id,
                ],
                no_json=True,
            )
            return
        except subprocess.CalledProcessError:
            if attempt == retries:
                raise
            print(f"BMC restart attempt {attempt}/{retries} failed, retrying in {delay}s...")
            time.sleep(delay)


def factory_reset_bmc(bmc_ip: str, bmc_username: str, bmc_password: str) -> None:
    """Factory-reset a BMC (DPU or host) to defaults via redfish."""
    # --address/--username/--password are flags on the `redfish` group and must
    # precede the `bmc-reset-to-defaults` subcommand (matches power_off_host).
    args = [
        "redfish",
        "--address",
        bmc_ip,
        "--username",
        bmc_username,
        "--password",
        bmc_password,
        "bmc-reset-to-defaults",
    ]
    try:
        run_admin_cli(args, no_json=True)
    except subprocess.CalledProcessError as e:
        stderr = e.stderr or ""
        # Workaround for occasional failure mode where the admin-cli
        # call surfaces a transport-level error even though the reset
        # actually succeeded.
        if "Connection reset by peer" in stderr:
            print(
                f"BMC at {bmc_ip} dropped the connection while executing ResetToDefaults. Caller "
                f"must verify the BMC comes back via wait_for_redfish_endpoint."
            )
            return
        raise


def get_bmc_accounts(bmc_ip: str, bmc_username: str, bmc_password: str) -> None:
    """List BMC accounts to validate that the supplied credentials authenticate.

    The command's account listing is diagnostic only; callers rely on its exit
    status to prove that the supplied password still works.
    """
    _ = run_admin_cli(
        [
            "redfish",
            "--address",
            bmc_ip,
            "--username",
            bmc_username,
            "--password",
            bmc_password,
            "get-bmc-accounts",
        ],
        no_json=True,
    )


def get_bmc_rotation_status(bmc_mac: str | None = None) -> dict:
    """Return site-wide BMC credential rotation status.

    Supply `bmc_mac` to add a `device` report for that BMC alongside the
    site-wide aggregate.

    :raises subprocess.CalledProcessError: If the command fails.
    :raises ValueError: If the command returns no status object.
    """
    args = ["credential", "rotation-status", "--type=bmc"]
    if bmc_mac is not None:
        args.extend(["--mac-address", bmc_mac])

    result = run_admin_cli(args)
    if not isinstance(result, dict):
        raise ValueError("credential rotation-status returned no status object")
    return result


def get_bmc_rotation_device_status(bmc_mac: str) -> dict:
    """Return one BMC's rotation status: converged, current_version, and so on.

    :raises subprocess.CalledProcessError: If the command fails.
    :raises ValueError: If the response carries no device report.
    """
    result = get_bmc_rotation_status(bmc_mac)
    device = result.get("device")
    if not isinstance(device, dict):
        raise ValueError(f"credential rotation-status returned no device report for {bmc_mac}")
    # The caller needs the site target to explain a device that lags it.
    device["target_version"] = result.get("target_version")
    return device


def get_sitewide_bmc_rotation_target_version() -> int:
    """Return the live version of the site-wide BMC root credential.

    Site Explorer resolves this before every site-wide credential read, so a
    site that has rotated serves the version the fleet moved to. Version 0 means
    no rotation has happened and the credential is at the unversioned path.

    :raises subprocess.CalledProcessError: If the command fails.
    :raises ValueError: If the response carries no usable target version.
    """
    target_version = get_bmc_rotation_status().get("target_version")
    if not isinstance(target_version, int) or isinstance(target_version, bool):
        raise ValueError(
            f"credential rotation-status returned no usable target_version: {target_version!r}"
        )
    if target_version < 0:
        raise ValueError(f"credential rotation-status returned a negative version: {target_version}")
    return target_version


def _redact_command(command: list[str]) -> list[str]:
    """Return a copy of an admin-cli command with the --password value masked.

    The admin-cli redfish subcommands take credentials as ``--password
    <value>``; this masks that value so it can't leak into the "Executing ..."
    echo or a CalledProcessError traceback.
    """
    redacted = list(command)
    for i, token in enumerate(redacted[:-1]):
        if token == "--password":
            redacted[i + 1] = "***"
    return redacted


def _discard_staged_location(pod: str, directory: str, *, warn: bool = True) -> bool:
    """Best-effort removal of one staged identity. True when it is gone.

    Never raises: this runs on paths that must not replace whatever the run
    itself reported.
    """
    try:
        kubectl.remove_pod_path(ADMIN_CLI_K8S_NAMESPACE, pod, directory)
    except Exception as error:
        if warn:
            print(
                f"Warning: could not remove the staged client identity at {directory} "
                f"in {ADMIN_CLI_K8S_NAMESPACE}/{pod}: {error}",
                file=sys.stderr,
            )
        return False
    return True


def _lost_staged_identity(stderr: str) -> bool:
    """Whether a failure looks like the pod holding our identity has gone.

    A site upgrade replaces the API pod, taking both the name we exec into and
    the /dev/shm directory we staged into. Matched narrowly: the admin-cli
    reports its own "not found" conditions, and those must not be mistaken for
    this one.
    """
    if not stderr:
        return False
    if "Error from server (NotFound)" in stderr and "pods " in stderr:
        return True
    if "unable to upgrade connection" in stderr:
        return True
    return f"{_STAGING_ROOT}/mlt-" in stderr and "No such file or directory" in stderr


def _read_api_build_version() -> str | None:
    """Best-effort read of the API's build version. Never raises."""
    command = _admin_cli_command_prefix() + ["--format", "json", "version"]
    try:
        result = subprocess.run(
            command, capture_output=True, text=True, timeout=ADMIN_CLI_TIMEOUT_SECONDS
        )
        if result.returncode:
            return None
        return json.loads(result.stdout).get("build_version")
    except Exception:
        return None


def _restage_client_identity(attempts: int = 3, delay: int = 20) -> bool:
    """Re-resolve the API pod and write the retained identity into it.

    Returns False when there is nothing to re-stage or no pod becomes ready.
    A rollout leaves a window with no ready replica, hence the retries.
    """
    global _grpc_api_target, _staged_api_build

    target = _grpc_api_target
    if target is None or target.certificate is None or _staged_identity is None:
        return False
    certificate, private_key = _staged_identity

    for attempt in range(1, attempts + 1):
        pod = directory = None
        try:
            pod = kubectl.get_deployment_pod(
                ADMIN_CLI_K8S_NAMESPACE, ADMIN_CLI_K8S_DEPLOYMENT
            )
            directory = f"{_STAGING_ROOT}/mlt-{secrets.token_hex(8)}"
            _staged_locations.append((pod, directory))
            staged = _StagedCertificate(
                pod=pod,
                cert_path=f"{directory}/client.crt",
                key_path=f"{directory}/client.key",
            )
            kubectl.write_pod_file(
                ADMIN_CLI_K8S_NAMESPACE, pod, staged.key_path, private_key
            )
            kubectl.write_pod_file(
                ADMIN_CLI_K8S_NAMESPACE, pod, staged.cert_path, certificate
            )
        except Exception as error:
            print(
                f"Re-staging the admin-cli identity failed "
                f"(attempt {attempt}/{attempts}): {error}",
                file=sys.stderr,
            )
            # The key is written first, so a write that failed part-way has
            # already left one behind. Drop it now rather than at the end of
            # the run. Quietly: the usual reason to be here is that the pod
            # went away, and then there is nothing to remove. Anything not
            # removed stays on the list for the final sweep to retry.
            if pod and directory and _discard_staged_location(pod, directory, warn=False):
                _staged_locations.remove((pod, directory))
            if attempt < attempts:
                time.sleep(delay)
            continue

        _grpc_api_target = _GrpcApiTarget(
            api_url=target.api_url,
            root_ca_path=target.root_ca_path,
            certificate=staged,
        )
        print(
            f"The API pod was replaced during this run; re-staged the admin-cli "
            f"identity into {ADMIN_CLI_K8S_NAMESPACE}/{pod}"
        )
        build = _read_api_build_version()
        if build and _staged_api_build and build != _staged_api_build:
            print(
                "WARNING: the API build changed during this run: "
                f"{_staged_api_build} -> {build}. Results before and after this "
                "point were produced against different builds.",
                file=sys.stderr,
            )
        if build:
            _staged_api_build = build
        return True
    return False


def _run_admin_cli_process(
    args: list[str], *, json_output: bool, timeout: int | None
) -> subprocess.CompletedProcess[str]:
    """Run one admin-cli process, recovering once if the API pod was replaced."""
    try:
        return _invoke_admin_cli(args, json_output=json_output, timeout=timeout)
    except subprocess.CalledProcessError as error:
        if not _lost_staged_identity(error.stderr or ""):
            raise
        if not _restage_client_identity():
            raise
    return _invoke_admin_cli(args, json_output=json_output, timeout=timeout)


def _invoke_admin_cli(
    args: list[str], *, json_output: bool, timeout: int | None
) -> subprocess.CompletedProcess[str]:
    """Run one admin-cli process and return its captured output."""
    command = _admin_cli_command_prefix()
    if json_output:
        command.extend(["--format", "json"])
    command.extend(args)

    print(f"Executing {_redact_command(command)}")
    try:
        result = subprocess.run(
            command,
            capture_output=True,
            text=True,
            timeout=ADMIN_CLI_TIMEOUT_SECONDS if timeout is None else timeout,
        )
    except subprocess.TimeoutExpired as expired:
        # Re-raise with the redacted command: TimeoutExpired's repr includes it.
        raise subprocess.TimeoutExpired(
            _redact_command(command), expired.timeout, output=expired.output
        ) from None
    if result.stderr and "not found" not in result.stderr:
        print(f"stderr: {result.stderr}")
    if result.returncode:
        # Raise with the redacted command so a --password value can't leak into
        # the traceback (CalledProcessError repr includes the command args).
        raise subprocess.CalledProcessError(
            result.returncode,
            _redact_command(command),
            output=result.stdout,
            stderr=result.stderr,
        )
    return result


def run_admin_cli(
    args: list[str], no_json: bool = False, timeout: int | None = None
) -> dict | None:
    """Run the specified nico-admin-cli command.

    Specify arguments as a list. Set ``no_json`` for commands that do not
    support JSON output; their stdout is printed and discarded. ``timeout``
    defaults to ``ADMIN_CLI_TIMEOUT_SECONDS``.

    :raises subprocess.CalledProcessError: If the command fails.
    :raises subprocess.TimeoutExpired: If the command does not return in time.
    """
    result = _run_admin_cli_process(args, json_output=not no_json, timeout=timeout)

    if no_json:
        print(result.stdout)
        return None

    return json.loads(result.stdout)


def run_admin_cli_text(args: list[str], timeout: int | None = None) -> str:
    """Run a command without a JSON format flag and return its stdout."""
    result = _run_admin_cli_process(args, json_output=False, timeout=timeout)
    return result.stdout


def _get_site_explorer_report(result: dict, bmc_ip: str) -> dict | None:
    # The v2 single-endpoint response is a flat {"address", "report", ...}
    # envelope; older/multi-endpoint responses nest matches under "endpoints".
    # Tolerate both.
    report = result.get("report")
    if report is None:
        for endpoint in result.get("endpoints", []):
            if endpoint.get("address") == bmc_ip:
                report = endpoint.get("report", {})
                break
    if not report:
        return None
    return report


def _get_site_explorer_error_type(report: dict) -> str | None:
    """Return a normalized error type from a Site Explorer report."""

    error = report.get("last_exploration_error")
    if not error:
        return None
    # v2 encodes the error as a JSON string, e.g. '{"Type":"AvoidLockout"}'.
    # Decode it and surface the Type; fall back to the raw value otherwise.
    if isinstance(error, str):
        try:
            error = json.loads(error)
        except json.JSONDecodeError:
            return error
    if isinstance(error, dict):
        return error.get("Type") or error.get("type") or json.dumps(error)
    return str(error)


def get_site_explorer_endpoint_error(bmc_ip: str) -> str | None:
    """Get the last exploration error type for the specified BMC IP.

    Returns the error type string (e.g. "Unauthorized", "AvoidLockout")
    or None if there is no error or the endpoint doesn't exist yet.
    """
    _, error_type = get_site_explorer_endpoint_status(bmc_ip)
    return error_type


def get_site_explorer_endpoint_status(bmc_ip: str) -> tuple[bool, str | None]:
    """Return whether an endpoint has a successful report and its error type.

    A missing endpoint/report returns ``(False, None)``. A persisted report
    with no exploration error returns ``(True, None)``.
    """
    try:
        result = run_admin_cli(["site-explorer", "get-report", "endpoint", bmc_ip])
    except subprocess.CalledProcessError:
        return False, None

    report = _get_site_explorer_report(result, bmc_ip)
    if report is None:
        return False, None
    error_type = _get_site_explorer_error_type(report)
    return error_type is None, error_type


def clear_site_explorer_error(bmc_ip: str) -> None:
    """Clear the last known site explorer error for the specified BMC IP.

    This removes the AvoidLockout state (or Unauthorized state) that
    site explorer enters after failed authentication attempts (e.g.
    after a factory reset).
    """
    run_admin_cli(
        ["site-explorer", "clear-error", bmc_ip],
        no_json=True,
    )


def refresh_site_explorer_endpoint(bmc_ip: str) -> dict:
    """Synchronously refresh and validate one Site Explorer endpoint.

    The admin CLI can exit successfully after persisting a report whose
    exploration itself failed, so inspect ``last_exploration_error`` rather
    than relying on the process exit status alone.
    """
    result = run_admin_cli(["site-explorer", "refresh", bmc_ip])
    if result is None:
        raise RuntimeError(f"Site Explorer refresh for {bmc_ip} returned no result")

    report = _get_site_explorer_report(result, bmc_ip)
    if report is None:
        raise RuntimeError(f"Site Explorer refresh for {bmc_ip} returned no report")

    error_type = _get_site_explorer_error_type(report)
    if error_type:
        raise RuntimeError(f"Site Explorer refresh for {bmc_ip} reported {error_type}")
    return report


def get_expected_machines(host_bmc_mac: str) -> dict:
    """Get the expected machines from the nico-admin-cli."""
    result = run_admin_cli(["expected-machine", "show", host_bmc_mac])
    return result
