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

"""Lenovo host factory-reset driver."""

import time

import requests

from lib import admin_cli, network

from .base import ResetDriverError, ResetTarget

REDFISH_TIMEOUT_SECONDS = 60
TASK_DEADLINE_SECONDS = 300


class LenovoHostResetDriver:
    """Perform the existing Lenovo BIOS, reboot, and BMC reset sequence."""

    def reset_host(self, target: ResetTarget) -> None:
        print("Resetting BIOS settings on the Lenovo host")
        url = (
            f"https://{target.bmc_ip}/redfish/v1/Systems/1/Bios/Actions/"
            "Bios.ResetBios"
        )
        data = {"ResetType": "default"}
        print(f"Executing redfish request. \nData: {data} \nURL: {url}")
        try:
            response = requests.post(
                url,
                json=data,
                auth=(target.credentials.username, target.credentials.password),
                verify=False,
                timeout=REDFISH_TIMEOUT_SECONDS,
            )
        except requests.RequestException as error:
            raise ResetDriverError(
                f"Failed to reach the {target.label} BMC for a BIOS reset: {error}",
                set_maintenance=True,
            ) from error
        if response.status_code == 202:
            task_id = response.json()["Id"]
            # One deadline covering request time and sleeps, so the bound is the
            # five minutes the error reports rather than 30 slow polls.
            deadline = time.monotonic() + TASK_DEADLINE_SECONDS
            print(f"Waiting for async redfish task {task_id} to complete")
            while True:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ResetDriverError(
                        "Redfish task did not complete in 5 minutes",
                        set_maintenance=True,
                    )
                task_url = (
                    f"https://{target.bmc_ip}/redfish/v1/TaskService/Tasks/{task_id}"
                )
                try:
                    response = requests.get(
                        task_url,
                        auth=(target.credentials.username, target.credentials.password),
                        verify=False,
                        timeout=min(REDFISH_TIMEOUT_SECONDS, remaining),
                    )
                except requests.RequestException as error:
                    raise ResetDriverError(
                        f"Lost contact with the {target.label} BMC while waiting for "
                        f"Redfish task {task_id}: {error}",
                        set_maintenance=True,
                    ) from error
                if response.status_code != 200:
                    print(response.text)
                    raise ResetDriverError(
                        "Failed to get redfish task status. "
                        f"Status code: {response.status_code}",
                        set_maintenance=True,
                    )
                if response.json()["TaskState"] == "Completed":
                    print("Redfish task completed.")
                    break
                print(
                    "Redfish task not yet completed, state "
                    f"{response.json()['TaskState']}"
                )
                time.sleep(min(10, max(0.0, deadline - time.monotonic())))
        else:
            print(response.text)
            raise ResetDriverError(
                "Failed to reset BIOS settings on the Lenovo host. "
                f"Status code: {response.status_code}",
                set_maintenance=True,
            )

        print("Removing the BIOS password from the Lenovo host")
        admin_cli.clear_host_bios_password(target.machine_id)
        print("Restarting the host")
        admin_cli.restart_machine(target.machine_id)
        time.sleep(10)
        try:
            network.wait_for_redfish_endpoint(hostname=target.bmc_ip)
        except Exception as error:
            raise ResetDriverError(
                f"Error while waiting for Lenovo host to recover: {error}",
                set_maintenance=True,
            ) from error

        print("Factory-resetting the Lenovo BMC")
        admin_cli.factory_reset_bmc(
            target.bmc_ip,
            target.credentials.username,
            target.credentials.password,
        )
        time.sleep(5)
        try:
            network.wait_for_redfish_endpoint(hostname=target.bmc_ip)
        except Exception as error:
            raise ResetDriverError(
                f"Error while waiting for Lenovo BMC to recover: {error}",
                set_maintenance=True,
            ) from error
