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

"""BlueField DPU factory-reset driver."""

import time

import requests

from lib import admin_cli, network

from .base import ResetDriverError, ResetTarget

REDFISH_TIMEOUT_SECONDS = 60


class BlueFieldDpuResetDriver:
    """Perform the existing BlueField BIOS and BMC reset sequence."""

    def reset_dpu(self, target: ResetTarget) -> None:
        print(f"Resetting BIOS settings on {target.label}")
        url = f"https://{target.bmc_ip}/redfish/v1/Systems/Bluefield/Bios/Settings"
        data = {"Attributes": {"ResetEfiVars": True}}
        print(f"Executing redfish request. \nPayload: {data} \nURL: {url}")
        try:
            response = requests.patch(
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
        if response.status_code != 200:
            print(response.text)
            raise ResetDriverError(
                f"Failed to reset BIOS settings on {target.label}. "
                f"Status code: {response.status_code}",
                set_maintenance=True,
            )
        print(f"Resetting BIOS settings on {target.label} was successful.")

        print(f"Restarting {target.label} BMC")
        admin_cli.restart_bmc(target.machine_id)
        time.sleep(5)
        try:
            network.wait_for_redfish_endpoint(hostname=target.bmc_ip)
        except Exception as error:
            raise ResetDriverError(
                f"Error while waiting for {target.label} BMC restart: {error}",
                set_maintenance=True,
            ) from error

        print(f"Factory-resetting {target.label} BMC")
        admin_cli.factory_reset_bmc(
            target.bmc_ip,
            target.credentials.username,
            target.credentials.password,
        )
        time.sleep(5)
        try:
            network.wait_for_redfish_endpoint(hostname=target.bmc_ip, sleep_time=10)
        except Exception as error:
            raise ResetDriverError(
                f"Error while waiting for {target.label} BMC factory reset: {error}",
                set_maintenance=True,
            ) from error
