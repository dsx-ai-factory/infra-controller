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

//! Which ComputerSystem and Manager NICo acts on when a BMC lists several.

use nv_redfish::Bmc;
use nv_redfish::computer_system::ComputerSystem;
use nv_redfish::manager::Manager;

/// Index of the BMC's primary ComputerSystem. BMCs that list a host beside
/// baseboards report a BIOS only on the host, so the first system after the
/// first that reports one wins, otherwise the first.
pub fn primary_system_index<B: Bmc>(systems: &[ComputerSystem<B>]) -> Option<usize> {
    if systems.is_empty() {
        return None;
    }
    Some(
        systems
            .iter()
            .enumerate()
            .skip(1)
            .find(|(_, system)| system.raw().bios.is_some())
            .map_or(0, |(index, _)| index),
    )
}

/// Index of the Manager for `system`: the one whose `ManagerForServers` links
/// it, otherwise the first, for BMCs that don't populate the link.
pub fn manager_index_for<B: Bmc>(
    managers: &[Manager<B>],
    system: Option<&ComputerSystem<B>>,
) -> Option<usize> {
    if managers.is_empty() {
        return None;
    }
    let linked = system.and_then(|system| {
        managers.iter().position(|manager| {
            manager
                .raw()
                .links
                .as_ref()
                .and_then(|links| links.manager_for_servers.as_ref())
                .is_some_and(|servers| {
                    servers
                        .iter()
                        .any(|server| *server.id() == system.raw().odata_id)
                })
        })
    });
    Some(linked.unwrap_or(0))
}
