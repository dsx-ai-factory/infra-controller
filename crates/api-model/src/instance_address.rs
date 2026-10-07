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
use carbide_uuid::instance::InstanceId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use ipnetwork::IpNetwork;
use sqlx::FromRow;

#[derive(Debug, FromRow, Clone)]
pub struct InstanceAddress {
    pub instance_id: InstanceId,
    pub segment_id: NetworkSegmentId,
    pub vpc_id: VpcId,
    // pub id: Uuid,          // unused
    pub address: std::net::IpAddr,
    /// The address's network in CIDR form, e.g. `10.3.2.0/30`.
    pub prefix: IpNetwork,
    /// The forward-DNS name in the host-naming strategy's IP-derived form,
    /// stored so the `dns_records_instance` view serves it without
    /// re-deriving in SQL. Nullable in the table.
    pub hostname: Option<String>,
}
