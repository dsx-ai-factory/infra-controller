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

"""GB200 host factory-reset driver."""

import time

from lib import network
from lib.gb200_factory_reset import GB200FactoryResetMethods

from .base import ResetDriverError, ResetTarget


class GB200HostResetDriver:
    """Perform the existing GB200 BIOS and host-BMC reset sequence."""

    def reset_host(self, target: ResetTarget) -> None:
        print("Factory-resetting GB200 compute tray")
        methods = GB200FactoryResetMethods(
            target.bmc_ip,
            target.credentials.username,
            target.credentials.password,
        )

        print("Resetting BIOS settings to default on the GB200 host")
        try:
            methods.reset_bios()
        except Exception as error:
            raise ResetDriverError(
                f"Error occurred during GB200 BIOS reset: {error}",
                set_maintenance=True,
            ) from error

        try:
            network.wait_for_redfish_endpoint(hostname=target.bmc_ip)
        except Exception as error:
            raise ResetDriverError(
                f"Error while waiting for GB200 host to recover: {error}",
                set_maintenance=True,
            ) from error

        print("Factory-resetting the GB200 BMC")
        try:
            methods.factory_reset_bmc(reset_type="ResetAll")
        except Exception as error:
            raise ResetDriverError(
                f"Error occurred during GB200 BMC factory reset: {error}",
                set_maintenance=True,
            ) from error

        print("GB200 BMC factory-reset complete. Waiting for BMC to reboot...")
        time.sleep(120)
        try:
            network.wait_for_redfish_endpoint(hostname=target.bmc_ip, max_retries=40)
        except Exception as error:
            raise ResetDriverError(
                f"Error while waiting for GB200 BMC to recover: {error}",
                set_maintenance=True,
            ) from error
