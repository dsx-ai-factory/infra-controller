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

    /// Target number of completed or failed job records retained, including parents
    /// and children. Defaults to 10,000; records younger than
    /// `min_terminal_retention_secs` are retained even when this target is exceeded.
    /// A hard ceiling of four times this target overrides the retention age.
    pub max_terminal_jobs: usize,

    /// Minimum terminal record lifetime in seconds, measured from completion.
    /// Defaults to 3,600, unless the hard ceiling is reached. Zero permits eviction
    /// as soon as the count target is exceeded.
    pub min_terminal_retention_secs: u64,

    /// Jobs that fail instead of completing. Set by the host in Rust, not
    /// from configuration; nothing fails unless named here.
    #[serde(skip)]
    pub faults: FaultConfig,

    /// The object ids `ListFirmwareObjects` reports. An apply whose document
    /// names no `Id` is attributed to the first; an empty list is an empty
    /// catalog.
    pub firmware_object_ids: Vec<String>,
}

/// Which node-level jobs fail. A selected job still reports running before
/// it fails.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FaultConfig {
    /// Node ids, as the caller sends them, whose jobs fail.
    pub fail_jobs_for_node_ids: Vec<String>,
}

impl Default for RmsMockConfig {
    fn default() -> Self {
        Self {
            version_string: concat!("machine-a-tron-rms-mock/", env!("CARGO_PKG_VERSION"))
                .to_string(),
            max_terminal_jobs: 10_000,
            min_terminal_retention_secs: 3_600,
            faults: FaultConfig::default(),
            firmware_object_ids: vec!["rms-mock-fw-1.0.0".to_string()],
        }
    }
}
