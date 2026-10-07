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

use carbide_uuid::rack::RackId;
use mac_address::MacAddress;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ExpectedSwitchJson {
    pub(crate) bmc_mac_address: MacAddress,
    pub(crate) bmc_username: String,
    pub(crate) bmc_password: String,
    pub(crate) switch_serial_number: String,
    #[serde(default)]
    pub(crate) nvos_mac_addresses: Vec<MacAddress>,
    pub(crate) nvos_username: Option<String>,
    pub(crate) nvos_password: Option<String>,
    #[serde(default)]
    pub(crate) metadata: Option<rpc::forge::Metadata>,
    pub(crate) rack_id: Option<RackId>,
    pub(crate) bmc_ip_address: Option<IpAddr>,
    #[serde(default)]
    pub(crate) nvos_ip_address: Option<IpAddr>,
    #[serde(default)]
    pub(crate) bmc_retain_credentials: Option<bool>,
}
