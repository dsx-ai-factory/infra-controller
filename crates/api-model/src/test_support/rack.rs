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

//! Rack model fixtures.

use crate::rack_type::{
    RackCapabilitiesSet, RackCapabilityCompute, RackCapabilityPowerShelf, RackCapabilitySwitch,
    RackHardwareTopology, RackProductFamily, RackProfile, RackProfileConfig,
};

/// Rack profile ID used by RMS-backed test fixtures.
pub const TEST_RMS_RACK_PROFILE_ID: &str = "NVL72";

/// Returns one RMS-ready GB200 NVL72 profile with 18 compute trays, 9 switches,
/// and 8 power shelves.
pub fn rms_rack_profiles() -> RackProfileConfig {
    RackProfileConfig {
        rack_profiles: [(
            TEST_RMS_RACK_PROFILE_ID.to_string(),
            RackProfile {
                product_family: Some(RackProductFamily::Gb200),
                rack_hardware_topology: Some(RackHardwareTopology::Gb200Nvl72r1C2g4Topology),
                rack_capabilities: RackCapabilitiesSet {
                    compute: RackCapabilityCompute {
                        count: 18,
                        vendor: Some("NVIDIA".to_string()),
                        ..Default::default()
                    },
                    switch: RackCapabilitySwitch {
                        count: 9,
                        vendor: Some("NVIDIA".to_string()),
                        ..Default::default()
                    },
                    power_shelf: RackCapabilityPowerShelf {
                        count: 8,
                        vendor: Some("LiteOn".to_string()),
                        ..Default::default()
                    },
                },
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
    }
}
