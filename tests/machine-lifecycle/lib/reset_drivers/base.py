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

"""Shared reset-driver contracts."""

from dataclasses import dataclass, field
from typing import Protocol


class ResetCredentials(Protocol):
    """The credential shape required by current direct-BMC reset drivers."""

    username: str
    password: str


@dataclass(frozen=True)
class ResetTarget:
    """A machine endpoint and the identity required by its current reset path."""

    machine_id: str
    bmc_ip: str
    credentials: ResetCredentials = field(repr=False)
    label: str


class ResetDriverError(Exception):
    """An expected reset failure and its existing maintenance-mode policy."""

    def __init__(self, message: str, *, set_maintenance: bool = False):
        super().__init__(message)
        self.set_maintenance = set_maintenance


class HostResetDriver(Protocol):
    """Reset operations required for one supported host platform."""

    def reset_host(self, target: ResetTarget) -> None:
        """Reset the host BIOS and BMC, returning only after required waits."""


class DpuResetDriver(Protocol):
    """Reset operations required for one supported DPU platform."""

    def reset_dpu(self, target: ResetTarget) -> None:
        """Reset the DPU BIOS and BMC, returning only after required waits."""
