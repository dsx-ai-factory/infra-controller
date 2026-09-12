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

/// Mock configuration. Every field has a default so that a host can mount
/// the mock without any configuration at all.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct RmsMockConfig {
    /// Reported by `GetVersion`. `librms` issues `GetVersion` as its
    /// connection liveness probe, so this is the first call any client makes.
    pub version_string: String,
}

impl Default for RmsMockConfig {
    fn default() -> Self {
        Self {
            version_string: concat!("machine-a-tron-rms-mock/", env!("CARGO_PKG_VERSION"))
                .to_string(),
        }
    }
}
