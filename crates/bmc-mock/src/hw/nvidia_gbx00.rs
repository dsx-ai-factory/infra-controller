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

//! Common functions for GB200 and GB300.

use std::borrow::Cow;

use serde_json::json;

use crate::{RackPlacement, redfish};

const CBC_REVISION_ID: u32 = 2;

pub(crate) struct Topology {
    pub(crate) chassis_physical_slot_number: u32,
    pub(crate) compute_tray_index: u32,
    pub(crate) revision_id: u32,
    pub(crate) topology_id: u32,
}

impl Topology {
    pub(crate) fn from_rack_placement(placement: RackPlacement) -> Option<Self> {
        let compute_tray_index = u32::from(placement.compute_tray_index()?);
        Some(Self {
            chassis_physical_slot_number: placement.chassis_physical_slot_number()?,
            compute_tray_index,
            revision_id: CBC_REVISION_ID,
            topology_id: placement.topology_id(),
        })
    }
}

// CBC chassis definition.
pub(super) fn cbc_chassis(
    chassis_id: Cow<'static, str>,
    topology: Option<&Topology>,
) -> redfish::chassis::SingleChassisConfig {
    redfish::chassis::SingleChassisConfig {
        id: chassis_id,
        chassis_type: "Component".into(),
        manufacturer: Some("Nvidia".into()),
        part_number: Some("750-0567-002".into()),
        model: Some("18x1RU CBL Cartridge".into()),
        serial_number: Some("1821220000000".into()),
        pcie_devices: Some(vec![]),
        oem: topology.map(|topology| {
            json!({
                "Nvidia": {
                    "@odata.type": "#NvidiaChassis.v1_4_0.NvidiaCBCChassis",
                    "ChassisPhysicalSlotNumber": topology.chassis_physical_slot_number,
                    "ComputeTrayIndex": topology.compute_tray_index,
                    "RevisionId": topology.revision_id,
                    "TopologyId": topology.topology_id,
                }
            })
        }),
        ..redfish::chassis::SingleChassisConfig::defaults()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;
    use crate::{RackInfo, RackType};

    #[derive(Debug)]
    struct Input {
        rack_type: RackType,
        position: u8,
    }

    #[test]
    fn cbc_values_follow_rack_placement() {
        check_values(
            [
                Check {
                    scenario: "first lower compute tray",
                    input: Input {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        position: 11,
                    },
                    expect: Some((10, 0, 2, 128)),
                },
                Check {
                    scenario: "last lower compute tray",
                    input: Input {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        position: 18,
                    },
                    expect: Some((17, 7, 2, 128)),
                },
                Check {
                    scenario: "first upper compute tray",
                    input: Input {
                        rack_type: RackType::LenovoGb300Nvl72,
                        position: 28,
                    },
                    expect: Some((18, 8, 2, 128)),
                },
                Check {
                    scenario: "last upper compute tray",
                    input: Input {
                        rack_type: RackType::LenovoGb300Nvl72,
                        position: 37,
                    },
                    expect: Some((27, 17, 2, 128)),
                },
                Check {
                    scenario: "non-compute rack member",
                    input: Input {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        position: 19,
                    },
                    expect: None,
                },
            ],
            |input| {
                let placement = RackInfo {
                    rack_type: input.rack_type,
                }
                .placement(input.position);
                let topology = Topology::from_rack_placement(placement);
                let chassis = cbc_chassis("CBC_0".into(), topology.as_ref());
                let nvidia = chassis.oem?.get("Nvidia")?.clone();
                Some((
                    nvidia.get("ChassisPhysicalSlotNumber")?.as_u64()?,
                    nvidia.get("ComputeTrayIndex")?.as_u64()?,
                    nvidia.get("RevisionId")?.as_u64()?,
                    nvidia.get("TopologyId")?.as_u64()?,
                ))
            },
        );
    }
}
