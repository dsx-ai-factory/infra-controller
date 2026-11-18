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
use std::net::IpAddr;

use carbide_uuid::domain::DomainId;
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use carbide_uuid::network::NetworkSegmentId;
use ipnetwork::IpNetwork;
use mac_address::MacAddress;
use sqlx::FromRow;

///
/// A machine dhcp response is a representation of some booting interface by Mac Address or DUID
/// (not implemented) that returns the network information for that interface on that node, and
/// contains everything necessary to return a DHCP response.
///
/// A DhcpRecord is populated by a database view (named machine_dhcp_records).
#[derive(Debug, FromRow, carbide_macros::DbTable)]
#[db_table(name = "machine_dhcp_records")]
pub struct DhcpRecord {
    pub machine_id: Option<MachineId>,
    pub segment_id: NetworkSegmentId,
    pub machine_interface_id: MachineInterfaceId,
    pub subdomain_id: Option<DomainId>,

    pub fqdn: String,

    pub mac_address: MacAddress,
    pub address: IpAddr,
    pub mtu: i32,

    pub prefix: IpNetwork,
    pub gateway: Option<IpAddr>,

    pub last_invalidation_time: chrono::DateTime<chrono::Utc>,
}
