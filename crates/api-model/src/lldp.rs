// SPDX-FileCopyrightText: Copyright (c) 2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use mac_address::MacAddress;
use sqlx::FromRow;

/// One LLDP neighbor that a running scout or DPU agent reported on one of its
/// local interfaces. The `local_*` fields describe the reporting machine's side
/// of the link; the rest describe the neighbor, as lldpd decoded its LLDP frame.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct LldpNeighbor {
    /// MAC address of the reporting machine's own interface that received the LLDP frame.
    pub local_mac_address: MacAddress,
    /// Name of that interface on the reporting machine, as lldpd reports it
    /// (e.g. `p0`, `oob_net0`, `pf0hpf`, `enp1s0np0`).
    pub local_port: String,
    /// Subtype of the neighbor's Chassis ID TLV, as lldpd names it (e.g. `mac`, `local`).
    pub chassis_id_type: String,
    /// The neighbor's chassis ID, interpreted according to `chassis_id_type`
    /// (e.g. the switch's base MAC).
    pub chassis_id_value: String,
    /// Subtype of the neighbor's Port ID TLV, as lldpd names it (e.g. `ifname`, `mac`).
    pub remote_port_type: String,
    /// The neighbor's port ID, interpreted according to `remote_port_type`
    /// (e.g. `swp1`, `Gi0/3`, or a MAC).
    pub remote_port_value: String,
    /// The neighbor's System Name TLV, usually its hostname.
    pub system_name: String,
    /// The neighbor's System Description TLV, usually its OS and hardware.
    /// May span several lines.
    pub system_description: String,
    /// The neighbor's Management Address TLVs, as text. lldpd does not
    /// guarantee these are IP addresses.
    pub management_addresses: Vec<String>,
    /// Serial number from the neighbor's LLDP-MED inventory, or `None` if it did not send one.
    pub med_serial: Option<String>,
    /// Manufacturer from the neighbor's LLDP-MED inventory, or `None` if it did not send one.
    pub med_manufacturer: Option<String>,
    /// Model name from the neighbor's LLDP-MED inventory, or `None` if it did not send one.
    pub med_model: Option<String>,
}
