// SPDX-FileCopyrightText: Copyright (c) 2025-2026 MIRANTIS, INC. & AFFILIATES. All rights reserved.
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

/// One LLDP neighbor that a running scout or DPU agent reported on one of its local interfaces.
#[derive(Clone, Debug, PartialEq, Eq, FromRow)]
pub struct LldpNeighbor {
    pub local_mac_address: MacAddress,
    pub local_port: String,
    pub chassis_id_type: String,
    pub chassis_id_value: String,
    pub remote_port_type: String,
    pub remote_port_value: String,
    pub system_name: String,
    pub system_description: String,
    pub management_addresses: Vec<String>,
    pub med_serial: Option<String>,
    pub med_manufacturer: Option<String>,
    pub med_model: Option<String>,
}
