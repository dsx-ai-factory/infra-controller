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

use serde::{Deserialize, Serialize};

// Represents metadata associated with a DNS domain.
///
/// This struct holds additional configuration information for a DNS domain,
/// such as which IP addresses or networks are allowed to perform AXFR (zone transfer) requests.
///
/// # Fields
///
/// * `allow_axfr_from` - A list of IP addresses or CIDR ranges as strings that are permitted to perform AXFR (zone transfer) requests.
///   This can be used to restrict zone transfers to trusted servers.
///
/// A list of IP addresses or CIDR ranges allowed to perform AXFR (zone transfer) requests.
///
/// This provides control over which external servers are permitted to retrieve
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING-KEBAB-CASE")]
pub struct DomainMetadata {
    pub allow_axfr_from: Vec<String>,
}

impl DomainMetadata {
    pub fn update_allow_axfr_from(&mut self, axfr_list: Vec<String>) {
        self.allow_axfr_from = axfr_list
    }

    pub fn allow_axfr_from(&self) -> &Vec<String> {
        &self.allow_axfr_from
    }
}
