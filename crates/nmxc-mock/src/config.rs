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

/// Real NMX-C boots with every GPU in one factory partition under this id,
/// which NICo recognises and deletes before it provisions anything.
const DEFAULT_PARTITION_ID: u32 = 32766;

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct NmxcMockConfig {
    /// Reported as the controller version in `Hello`.
    pub version_string: String,
    /// Seed every new domain with a factory partition holding all of its
    /// GPUs, as a real controller does. NICo must delete it before it can
    /// provision partitions of its own; turn this off to skip that step.
    pub boot_with_default_partition: bool,
    pub default_partition_id: u32,
    /// NICo also treats any partition whose name contains `Default` as the
    /// factory one, so keep that substring if the name is changed.
    pub default_partition_name: String,
}

impl Default for NmxcMockConfig {
    fn default() -> Self {
        Self {
            version_string: concat!("machine-a-tron-nmxc-mock/", env!("CARGO_PKG_VERSION")).into(),
            boot_with_default_partition: true,
            default_partition_id: DEFAULT_PARTITION_ID,
            default_partition_name: "Default".into(),
        }
    }
}
