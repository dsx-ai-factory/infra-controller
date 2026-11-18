/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Compatibility reads for CLI connections to independently versioned Core servers.

use rpc::Machine;

/// Borrowed connection fields used by CLI commands.
pub(crate) struct Endpoint<'a> {
    pub(crate) ip: Option<&'a str>,
    pub(crate) mac: Option<&'a str>,
}

/// Prefer the split endpoint, including unknown values, and otherwise read legacy BMC info.
#[allow(deprecated)] // The CLI can connect to older Core versions.
pub(crate) fn endpoint(machine: &Machine) -> Option<Endpoint<'_>> {
    match machine.bmc.as_ref() {
        Some(endpoint) => Some(Endpoint {
            ip: endpoint.ip.as_deref(),
            mac: endpoint.mac.as_deref(),
        }),
        None => machine.bmc_info.as_ref().map(|info| Endpoint {
            ip: info.ip.as_deref(),
            mac: info.mac.as_deref(),
        }),
    }
}

/// Prefer split BMC status independently of endpoint availability; absent status uses legacy info.
#[allow(deprecated)] // The CLI can connect to older Core versions.
pub(crate) fn firmware_version(machine: &Machine) -> Option<&str> {
    match machine
        .status
        .as_ref()
        .and_then(|status| status.bmc_status.as_ref())
    {
        Some(status) => status.firmware_version.as_deref(),
        None => machine
            .bmc_info
            .as_ref()
            .and_then(|info| info.firmware_version.as_deref()),
    }
}
