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

use model::dhcp_record::DhcpRecord;

use crate as rpc;

impl From<DhcpRecord> for rpc::forge::DhcpRecord {
    fn from(record: DhcpRecord) -> Self {
        Self {
            machine_id: record.machine_id,
            machine_interface_id: Some(record.machine_interface_id),
            segment_id: Some(record.segment_id),
            subdomain_id: record.subdomain_id,
            fqdn: record.fqdn,
            mac_address: record.mac_address.to_string(),
            address: record.address.to_string(),
            mtu: record.mtu,
            prefix: record.prefix.to_string(),
            gateway: record.gateway.map(|gw| gw.to_string()),
            booturl: None, // TODO(ajf): extend database, synthesize URL
            last_invalidation_time: Some(record.last_invalidation_time.into()),
            ntp_servers: vec![],
        }
    }
}
