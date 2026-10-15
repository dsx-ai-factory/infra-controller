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

"""Dell host factory-reset driver."""

import time

from lib import network
from lib.dell_factory_reset import DellFactoryResetMethods

from .base import ResetDriverError, ResetTarget


class DellHostResetDriver:
    """Perform the existing Dell BIOS, server reboot, and iDRAC reset sequence."""

    def reset_host(self, target: ResetTarget) -> None:
        print("Factory-resetting Dell machine")
        methods = DellFactoryResetMethods(
            target.bmc_ip,
            target.credentials.username,
            target.credentials.password,
        )

        print("Unlocking iDRAC")
        try:
            methods.unlock_idrac()
        except Exception as error:
            raise ResetDriverError(f"Error occurred during iDRAC unlock: {error}") from error

        print("Resetting BIOS settings to default on the Dell host")
        try:
            methods.reset_bios()
        except Exception as error:
            raise ResetDriverError(f"Error occurred during BIOS reset: {error}") from error

        try:
            methods.reboot_server()
        except Exception as error:
            raise ResetDriverError(f"Error occurred during server reboot: {error}") from error

        print("Dell BIOS/UEFI settings reset complete. Waiting for server to reboot...")
        time.sleep(300)
        try:
            network.wait_for_redfish_endpoint(hostname=target.bmc_ip)
        except Exception as error:
            raise ResetDriverError(
                f"Error while waiting for Dell host to recover: {error}"
            ) from error

        try:
            methods.disable_host_header_check()
        except Exception as error:
            raise ResetDriverError(
                f"Error while disabling host header check: {error}"
            ) from error

        print("Factory-resetting the iDRAC")
        try:
            methods.factory_reset_bmc(level="ResetAllWithRootDefaults")
        except Exception as error:
            raise ResetDriverError(
                f"Error occurred during iDRAC factory reset: {error}"
            ) from error

        print("Dell BMC factory-reset complete. Waiting for BMC to reboot...")
        time.sleep(120)
        try:
            network.wait_for_redfish_endpoint(hostname=target.bmc_ip, max_retries=40)
        except Exception as error:
            raise ResetDriverError(
                f"Error while waiting for iDRAC to recover: {error}"
            ) from error
