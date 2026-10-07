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
use carbide_uuid::machine::MachineId;
use mac_address::MacAddress;
use sqlx::FromRow;
use uuid::Uuid;

use crate::machine_boot_interface::MachineBootInterface;
use crate::network_segment::NetworkSegmentType;

#[derive(Debug, Clone, FromRow)]
pub struct PredictedMachineInterface {
    pub id: Uuid,
    pub machine_id: MachineId,
    pub mac_address: MacAddress,
    pub expected_network_segment_type: NetworkSegmentType,
    /// The last known vendor-named Redfish `EthernetInterface.Id` for this
    /// MAC, handed to the `machine_interfaces` row at DHCP promotion so
    /// the host's boot target is a full pair from its first owned interface.
    pub boot_interface_id: Option<String>,
    /// Whether promotion should make this the machine's primary interface.
    ///
    /// For hosts this carries the declared `ExpectedInterface.primary` intent. For DPUs the
    /// trusted OOB prediction is always primary because it is the DPU OS data interface.
    pub primary_interface: bool,
}

impl PredictedMachineInterface {
    /// The predicted [`MachineBootInterface`]: this NIC's MAC plus its last
    /// recorded Redfish interface id -- the same pair the promoted
    /// `machine_interfaces` row holds once the NIC's first lease lands.
    /// `None` until the id has been captured from an exploration report.
    pub fn boot_interface(&self) -> Option<MachineBootInterface> {
        MachineBootInterface::for_mac(self.mac_address, self.boot_interface_id.clone())
    }
}

#[derive(Debug, Clone)]
pub struct NewPredictedMachineInterface<'a> {
    pub machine_id: &'a MachineId,
    pub mac_address: MacAddress,
    pub expected_network_segment_type: NetworkSegmentType,
    pub boot_interface_id: Option<String>,
    /// See [`PredictedMachineInterface::primary_interface`].
    pub primary_interface: bool,
}
