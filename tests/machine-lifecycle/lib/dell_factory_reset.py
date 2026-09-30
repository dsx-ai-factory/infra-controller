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

import json
import time
from typing import Literal

import requests

REDFISH_TIMEOUT_SECONDS = 60
GRACEFUL_SHUTDOWN_SECONDS = 300


class DellFactoryResetError(Exception):
    pass


class DellFactoryResetMethods:
    def __init__(self, host_bmc_ip, host_bmc_username, host_bmc_password):
        self.host_bmc_ip = host_bmc_ip
        self.host_bmc_username = host_bmc_username
        self.host_bmc_password = host_bmc_password

    def reset_bios(self):
        url = (
            "https://%s/redfish/v1/Systems/System.Embedded.1/Bios/Actions/Bios.ResetBios"
            % self.host_bmc_ip
        )
        payload = {}
        headers = {"content-type": "application/json"}
        response = requests.post(
            url,
            json=payload,
            headers=headers,
            verify=False,
            timeout=REDFISH_TIMEOUT_SECONDS,
            auth=(self.host_bmc_username, self.host_bmc_password),
        )
        if response.status_code == 200:
            print(
                f"\n- PASS: status code {response.status_code} returned for POST command to reset "
                f"BIOS to default settings"
            )
        else:
            print("\n- FAIL, Command failed, status code is %s" % response.status_code)
            detail_message = str(response.__dict__)
            if "reset the iDRAC" in detail_message:
                print("WARNING: Failed to reset the BIOS. Continuing with factory-reset...")
            else:
                print(detail_message)
                raise DellFactoryResetError(
                    f"Dell BIOS reset failed, status code {response.status_code}. "
                    f"Details: {detail_message}"
                )

    def _get_server_status(self):
        for _ in range(10):
            try:
                response = requests.get(
                    "https://%s/redfish/v1/Systems/System.Embedded.1" % self.host_bmc_ip,
                    verify=False,
                    timeout=REDFISH_TIMEOUT_SECONDS,
                    auth=(self.host_bmc_username, self.host_bmc_password),
                )
                return response.json()
            except json.decoder.JSONDecodeError:
                print(
                    "Error: Failed to decode JSON response from Redfish API. Retrying in "
                    "5 seconds..."
                )
                time.sleep(5)
        raise DellFactoryResetError(
            f"Could not read server status from {self.host_bmc_ip}: "
            "no valid Redfish response after 10 attempts"
        )

    def reboot_server(self):
        data = self._get_server_status()
        print("\n- INFO, Current server power state is: %s" % data["PowerState"])
        if data["PowerState"] == "On":
            url = (
                "https://%s/redfish/v1/Systems/System.Embedded.1/Actions/ComputerSystem.Reset"
                % self.host_bmc_ip
            )
            payload = {"ResetType": "GracefulShutdown"}
            headers = {"content-type": "application/json"}
            response = requests.post(
                url,
                json=payload,
                headers=headers,
                verify=False,
                timeout=REDFISH_TIMEOUT_SECONDS,
                auth=(self.host_bmc_username, self.host_bmc_password),
            )
            if response.status_code == 204:
                print(
                    f"- PASS, POST command passed to gracefully power OFF server, status code "
                    f"returned is {response.status_code}"
                )
                print(
                    "- INFO, script will now verify the server was able to perform a graceful "
                    "shutdown. If the server was unable to perform a graceful shutdown, forced "
                    "shutdown will be invoked in 5 minutes"
                )
                time.sleep(15)
                deadline = time.monotonic() + GRACEFUL_SHUTDOWN_SECONDS
            else:
                print(
                    f"\n- FAIL, Command failed to gracefully power OFF server, status code is: "
                    f"{response.status_code}\n"
                )
                print("Extended Info Message: {0}".format(response.json()))
                raise DellFactoryResetError(
                    f"Dell graceful power OFF failed, status code {response.status_code}. "
                    f"Extended Info: {response.json()}"
                )
            while True:
                response = requests.get(
                    "https://%s/redfish/v1/Systems/System.Embedded.1" % self.host_bmc_ip,
                    verify=False,
                    timeout=REDFISH_TIMEOUT_SECONDS,
                    auth=(self.host_bmc_username, self.host_bmc_password),
                )
                data = response.json()
                if data["PowerState"] == "Off":
                    print(
                        "- PASS, GET command passed to verify graceful shutdown was successful and "
                        "server is in OFF state"
                    )
                    break
                elif time.monotonic() >= deadline:
                    print(
                        "- INFO, unable to perform graceful shutdown, server will now perform "
                        "forced shutdown"
                    )
                    payload = {"ResetType": "ForceOff"}
                    headers = {"content-type": "application/json"}
                    response = requests.post(
                        url,
                        json=payload,
                        headers=headers,
                        verify=False,
                        timeout=REDFISH_TIMEOUT_SECONDS,
                        auth=(self.host_bmc_username, self.host_bmc_password),
                    )
                    if response.status_code == 204:
                        print(
                            f"- PASS, POST command passed to perform forced shutdown, status code "
                            f"return is {response.status_code}"
                        )
                        time.sleep(15)
                        response = requests.get(
                            "https://%s/redfish/v1/Systems/System.Embedded.1" % self.host_bmc_ip,
                            verify=False,
                            timeout=REDFISH_TIMEOUT_SECONDS,
                            auth=(self.host_bmc_username, self.host_bmc_password),
                        )
                        data = response.json()
                        if data["PowerState"] == "Off":
                            print(
                                "- PASS, GET command passed to verify forced shutdown was "
                                "successful and server is in OFF state"
                            )
                            break
                        else:
                            print(
                                "- FAIL, server not in OFF state, current power status is %s"
                                % data["PowerState"]
                            )
                            raise DellFactoryResetError(
                                f"Dell forced shutdown did not result in OFF state. "
                                f"Current state: {data['PowerState']}"
                            )
                    else:
                        raise DellFactoryResetError(
                            f"Dell forced shutdown failed, status code {response.status_code}"
                        )
                else:
                    time.sleep(15)
            payload = {"ResetType": "On"}
            headers = {"content-type": "application/json"}
            response = requests.post(
                url,
                json=payload,
                headers=headers,
                verify=False,
                timeout=REDFISH_TIMEOUT_SECONDS,
                auth=(self.host_bmc_username, self.host_bmc_password),
            )
            if response.status_code == 204:
                print(
                    "- PASS, Command passed to power ON server, status code return is %s"
                    % response.status_code
                )
            else:
                print(
                    "\n- FAIL, Command failed to power ON server, status code is: %s\n"
                    % response.status_code
                )
                print("Extended Info Message: {0}".format(response.json()))
                raise DellFactoryResetError(
                    f"Dell power ON failed from ON path, status code {response.status_code}. "
                    f"Extended Info: {response.json()}"
                )
        elif data["PowerState"] == "Off":
            url = (
                f"https://{self.host_bmc_ip}/redfish/v1/Systems/System.Embedded.1/Actions/"
                f"ComputerSystem.Reset"
            )
            payload = {"ResetType": "On"}
            headers = {"content-type": "application/json"}
            response = requests.post(
                url,
                json=payload,
                headers=headers,
                verify=False,
                timeout=REDFISH_TIMEOUT_SECONDS,
                auth=(self.host_bmc_username, self.host_bmc_password),
            )
            if response.status_code == 204:
                print(
                    "- PASS, Command passed to power ON server, code return is %s"
                    % response.status_code
                )
            else:
                print(
                    "\n- FAIL, Command failed to power ON server, status code is: %s\n"
                    % response.status_code
                )
                print("Extended Info Message: {0}".format(response.json()))
                raise DellFactoryResetError(
                    f"Dell power ON failed from OFF path, status code {response.status_code}. "
                    f"Extended Info: {response.json()}"
                )
        else:
            print(
                "- FAIL, unable to get current server power state to perform either reboot or "
                "power on"
            )
            raise DellFactoryResetError(
                "Dell server power state unknown; cannot reboot or power on"
            )

    def unlock_idrac(self):
        print("Unlocking iDRAC")
        url = f"https://{self.host_bmc_ip}/redfish/v1/Managers/iDRAC.Embedded.1/Attributes"
        payload = {"Attributes": {"Lockdown.1.SystemLockdown": "Disabled"}}
        headers = {"Content-Type": "application/json"}
        try:
            response = requests.patch(
                url,
                headers=headers,
                json=payload,
                auth=(self.host_bmc_username, self.host_bmc_password),
                verify=False,
                timeout=REDFISH_TIMEOUT_SECONDS,
            )
            if response.status_code != 200:
                print(f"Unlocking iDRAC failed. Status code: {response.status_code}")
                print(response.text)
        except Exception as e:
            print(f"Error unlocking iDRAC: {e}")
            raise DellFactoryResetError(f"Error unlocking iDRAC: {e}")

    def disable_host_header_check(self):
        print("Disabling host header check")
        url = f"https://{self.host_bmc_ip}/redfish/v1/Managers/iDRAC.Embedded.1/Attributes"
        payload = {"Attributes": {"WebServer.1.HostHeaderCheck": "Disabled"}}
        headers = {"Content-Type": "application/json"}
        response = requests.patch(
            url,
            headers=headers,
            json=payload,
            auth=(self.host_bmc_username, self.host_bmc_password),
            verify=False,
            timeout=REDFISH_TIMEOUT_SECONDS,
        )
        if response.status_code != 200:
            print(f"Disabling host header check failed. Status code: {response.status_code}")
            print(response.text)
            raise DellFactoryResetError(
                f"Failed to disable host header check, status code {response.status_code}. "
                f"Details: {response.text}"
            )
        print("Disabling host header check successful.")

    def factory_reset_bmc(
        self, level: Literal["Default", "ResetAllWithRootDefaults", "All"] = "Default"
    ):
        url = (
            f"https://{self.host_bmc_ip}/redfish/v1/Managers/iDRAC.Embedded.1/Actions/"
            f"Oem/DellManager.ResetToDefaults"
        )
        payload = {"ResetType": level}
        headers = {"content-type": "application/json"}
        response = requests.post(
            url,
            json=payload,
            headers=headers,
            verify=False,
            timeout=REDFISH_TIMEOUT_SECONDS,
            auth=(self.host_bmc_username, self.host_bmc_password),
        )
        if response.status_code == 200:
            print(
                f"\n- PASS, status code {response.status_code} returned for POST command to reset "
                f"iDRAC to {level} setting\n"
            )
        else:
            data = response.json()
            print(f"\n- FAIL, status code {response.status_code} returned, error is: \n{data}")
            raise DellFactoryResetError(
                f"Dell iDRAC factory reset failed with status code {response.status_code}. "
                f"Error: {data}"
            )
        time.sleep(15)
        print("- INFO, iDRAC will now reset and be back online within a few minutes.")

    def change_bmc_password(self, password: str):
        print("Changing iDRAC root password")
        url = f"https://{self.host_bmc_ip}/redfish/v1/Managers/iDRAC.Embedded.1/Accounts/2"
        payload = {"Password": password}
        headers = {"Content-Type": "application/json"}
        try:
            response = requests.patch(
                url,
                headers=headers,
                json=payload,
                auth=(self.host_bmc_username, self.host_bmc_password),
                verify=False,
                timeout=REDFISH_TIMEOUT_SECONDS,
            )
            if response.status_code == 200:
                print("- PASS, iDRAC root password changed successfully")
                return True
            else:
                print(f"- FAIL, Failed to change password. Status code: {response.status_code}")
                print(response.text)
                return False
        except Exception as e:
            print(f"- ERROR, Exception occurred while changing password: {e}")
            return False
