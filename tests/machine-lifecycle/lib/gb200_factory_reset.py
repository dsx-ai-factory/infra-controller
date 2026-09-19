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

"""Factory-reset helpers for GB200 compute trays.

GB200 host BMCs expose the standard Redfish manager/system names
``Managers/BMC_0`` and ``Systems/System_0`` (unlike Dell's
``iDRAC.Embedded.1`` / ``System.Embedded.1`` or Lenovo's ``Systems/1``),
so the reset is a pair of plain Redfish calls rather than an OEM action or
the nico-admin-cli wrapper used for the DPU/Lenovo BMCs.

The two calls (in order) are:

    1. BIOS reset : POST Systems/System_0/Bios/Actions/Bios.ResetBios
    2. BMC reset  : POST Managers/BMC_0/Actions/Manager.ResetToDefaults
                    {"ResetToDefaultsType": "ResetAll"}
"""

import time
from typing import Literal

import requests


class GB200FactoryResetError(Exception):
    pass


class GB200FactoryResetMethods:
    # The BIOS reset may return synchronously (200) or as an async redfish
    # task (202). When async, poll the task for up to ~5 minutes.
    _TASK_POLL_ATTEMPTS = 30
    _TASK_POLL_INTERVAL = 10

    def __init__(self, host_bmc_ip: str, host_bmc_username: str, host_bmc_password: str):
        self.host_bmc_ip = host_bmc_ip
        self.host_bmc_username = host_bmc_username
        self.host_bmc_password = host_bmc_password

    @property
    def _auth(self) -> tuple[str, str]:
        return (self.host_bmc_username, self.host_bmc_password)

    def reset_bios(self) -> None:
        """Reset BIOS/UEFI settings to defaults via redfish.

        POSTs to the System_0 ``Bios.ResetBios`` action. Accepts a
        synchronous 200 or an async 202 (in which case the returned task is
        polled to completion).

        :raises GB200FactoryResetError: on an unexpected status code, or a
          task that fails or times out.
        """
        url = f"https://{self.host_bmc_ip}/redfish/v1/Systems/System_0/Bios/Actions/Bios.ResetBios"
        print(f"Resetting GB200 BIOS settings via redfish.\nURL: {url}")
        response = requests.post(
            url,
            json={},
            headers={"Content-Type": "application/json"},
            auth=self._auth,
            verify=False,
            timeout=60,
        )
        if response.status_code == 200:
            print(f"- PASS: BIOS reset accepted (status {response.status_code})")
            return
        if response.status_code == 202:
            task_id = response.json().get("Id")
            print(f"- INFO: BIOS reset accepted as async redfish task {task_id}")
            self._wait_for_task(task_id)
            return
        raise GB200FactoryResetError(
            f"GB200 BIOS reset failed, status code {response.status_code}. "
            f"Details: {response.text}"
        )

    def factory_reset_bmc(
        self, reset_type: Literal["ResetAll", "PreserveNetworkAndUsers", "PreserveNetwork"] = "ResetAll"
    ) -> None:
        """Factory-reset the GB200 host BMC via redfish ``Manager.ResetToDefaults``.

        Mirrors the verified curl::

            POST .../Managers/BMC_0/Actions/Manager.ResetToDefaults
            {"ResetToDefaultsType": "ResetAll"}

        The BMC reboots as a result, so a dropped connection is tolerated
        (same rationale as ``admin_cli.factory_reset_bmc``); the caller must
        verify the BMC returns via ``network.wait_for_redfish_endpoint``.

        :raises GB200FactoryResetError: on an unexpected (non-transport)
          status code.
        """
        url = (
            f"https://{self.host_bmc_ip}/redfish/v1/Managers/BMC_0/Actions/"
            f"Manager.ResetToDefaults"
        )
        payload = {"ResetToDefaultsType": reset_type}
        print(f"Factory-resetting GB200 BMC via redfish.\nPayload: {payload}\nURL: {url}")
        try:
            response = requests.post(
                url,
                json=payload,
                headers={"Content-Type": "application/json"},
                auth=self._auth,
                verify=False,
                timeout=60,
            )
        except requests.exceptions.RequestException as e:
            # The BMC can drop the connection mid-reset even though the reset
            # itself succeeds. Caller verifies recovery via
            # wait_for_redfish_endpoint.
            print(
                f"GB200 BMC at {self.host_bmc_ip} dropped the connection while executing "
                f"ResetToDefaults ({e}). Caller must verify the BMC comes back via "
                f"wait_for_redfish_endpoint."
            )
            return
        if response.status_code in (200, 202, 204):
            print(f"- PASS: BMC ResetToDefaults accepted (status {response.status_code})")
            return
        raise GB200FactoryResetError(
            f"GB200 BMC factory reset failed, status code {response.status_code}. "
            f"Details: {response.text}"
        )

    def _wait_for_task(self, task_id: str) -> None:
        """Poll a redfish task to ``Completed`` (used for an async BIOS reset)."""
        if not task_id:
            raise GB200FactoryResetError("BIOS reset returned 202 but no task Id")
        url = f"https://{self.host_bmc_ip}/redfish/v1/TaskService/Tasks/{task_id}"
        for _ in range(self._TASK_POLL_ATTEMPTS):
            response = requests.get(url, auth=self._auth, verify=False, timeout=30)
            if response.status_code != 200:
                raise GB200FactoryResetError(
                    f"Failed to get redfish task {task_id} status. "
                    f"Status code: {response.status_code}"
                )
            state = response.json().get("TaskState")
            if state == "Completed":
                print(f"- PASS: BIOS reset task {task_id} completed")
                return
            print(f"BIOS reset task {task_id} not complete yet, state: {state}")
            time.sleep(self._TASK_POLL_INTERVAL)
        raise GB200FactoryResetError(
            f"BIOS reset task {task_id} did not complete within "
            f"{self._TASK_POLL_ATTEMPTS * self._TASK_POLL_INTERVAL}s"
        )
