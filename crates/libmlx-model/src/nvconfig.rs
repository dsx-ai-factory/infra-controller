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

//! Fixed mlxconfig profiles selected from trusted platform identity.

// NVIDIA's public CPU as Root Complex recipe, "CPU as RC of NVMe, DPU as
// PCIe switch":
// <https://docs.nvidia.com/networking/display/mftv4341lts/mlxconfig-%E2%80%93-changing-device-configuration-tool>
// The generic recipe uses PCI_BUS00_SPEED=4, while the validated GB200/B3240
// platform uses 5. Applying this profile requires a cold host power cycle.
/// Number of PF scalable functions required by the GB200 B3240 V1 profile.
pub const GB200_B3240_V1_PF_TOTAL_SF: u32 = 128;

const GB200_B3240_V1_PARAMETERS: [&str; 28] = [
    "PF_NUM_PF_MSIX_VALID=1",
    "PER_PF_NUM_SF=1",
    "NUM_PF_MSIX_VALID=0",
    "PF_BAR2_ENABLE=0",
    "LINK_TYPE_P1=2",
    "LINK_TYPE_P2=2",
    "OFF_BOARD_SERIALIZER=1",
    "PCI_BUS00_HIERARCHY_TYPE=1",
    "PCI_BUS00_SPEED=5",
    "PCI_BUS00_WIDTH=5",
    "PCI_BUS10_HIERARCHY_TYPE=1",
    "PCI_BUS10_SPEED=4",
    "PCI_BUS10_WIDTH=3",
    "PCI_BUS12_HIERARCHY_TYPE=1",
    "PCI_BUS12_SPEED=4",
    "PCI_BUS12_WIDTH=3",
    "PCI_BUS14_HIERARCHY_TYPE=1",
    "PCI_BUS14_SPEED=4",
    "PCI_BUS14_WIDTH=3",
    "PCI_BUS16_HIERARCHY_TYPE=1",
    "PCI_BUS16_SPEED=4",
    "PCI_BUS16_WIDTH=3",
    "PF_TOTAL_SF=128",
    "PF_SF_BAR_SIZE=10",
    "PF_NUM_PF_MSIX=228",
    "LAG_RESOURCE_ALLOCATION=1",
    "PCI_SWITCH0_UPSTREAM_PORT_BUS=0",
    "PCI_SWITCH0_UPSTREAM_PORT_PEX=0",
];

const GB200_B3240_PART_NUMBERS: [&str; 5] = [
    "900-9D3B6-00CN-AB0",
    "900-9D3B6-00CN-PA0",
    "900-9D3B6-00SN-AB0",
    "900-9D3B6-00CN-PN0",
    "900-9D3B6-00CN-P_Ax",
];

/// `DpuNvConfigProfile` identifies a fixed version of platform mlxconfig values.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DpuNvConfigProfile {
    /// Version 1 of the validated GB200/B3240 CPU as Root Complex profile.
    Gb200B3240V1,
}

impl DpuNvConfigProfile {
    /// Returns the GB200 profile for an exact supported B3240 part number.
    ///
    /// Accepted identities are `900-9D3B6-00CN-AB0`, `900-9D3B6-00SN-AB0`,
    /// `900-9D3B6-00CN-PA0`, `900-9D3B6-00CN-PN0`, and
    /// `900-9D3B6-00CN-P_Ax`. Surrounding whitespace and ASCII letter case are
    /// ignored. The caller must separately verify that the DPU belongs to a
    /// GB200 rack. Broader B3240 product prefixes are not accepted.
    pub fn for_gb200_b3240_part_number(part_number: &str) -> Option<Self> {
        let part_number = part_number.trim();

        GB200_B3240_PART_NUMBERS
            .iter()
            .any(|supported| supported.eq_ignore_ascii_case(part_number))
            .then_some(Self::Gb200B3240V1)
    }

    /// Returns the trusted `KEY=VALUE` assignments for this profile.
    pub const fn parameters(self) -> &'static [&'static str] {
        match self {
            Self::Gb200B3240V1 => &GB200_B3240_V1_PARAMETERS,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn gb200_b3240_profile_requires_an_exact_supported_part_number() {
        value_scenarios!(
            run = DpuNvConfigProfile::for_gb200_b3240_part_number;
            "supported identities" {
                "900-9D3B6-00CN-AB0" => Some(DpuNvConfigProfile::Gb200B3240V1),
                "900-9D3B6-00SN-AB0" => Some(DpuNvConfigProfile::Gb200B3240V1),
                "900-9D3B6-00CN-PA0" => Some(DpuNvConfigProfile::Gb200B3240V1),
                "900-9D3B6-00CN-PN0" => Some(DpuNvConfigProfile::Gb200B3240V1),
                "900-9D3B6-00CN-P_Ax" => Some(DpuNvConfigProfile::Gb200B3240V1),
                " 900-9d3b6-00cn-pa0\n" => Some(DpuNvConfigProfile::Gb200B3240V1),
            }

            "unsupported part numbers" {
                "900-9D3B6" => None,
                "900-9D3B6-00CV-AA0" => None,
                "900-9D3B6-00CN-PA0-extra" => None,
                "" => None,
            }
        );
    }

    #[test]
    fn gb200_b3240_v1_preserves_parameter_count_and_documented_values() {
        // Keep the reviewed platform recipe visible so accidental changes fail
        // without depending on hardware-backed testing.
        let parameters = DpuNvConfigProfile::Gb200B3240V1.parameters();

        assert_eq!(
            parameters,
            &[
                "PF_NUM_PF_MSIX_VALID=1",
                "PER_PF_NUM_SF=1",
                "NUM_PF_MSIX_VALID=0",
                "PF_BAR2_ENABLE=0",
                "LINK_TYPE_P1=2",
                "LINK_TYPE_P2=2",
                "OFF_BOARD_SERIALIZER=1",
                "PCI_BUS00_HIERARCHY_TYPE=1",
                "PCI_BUS00_SPEED=5",
                "PCI_BUS00_WIDTH=5",
                "PCI_BUS10_HIERARCHY_TYPE=1",
                "PCI_BUS10_SPEED=4",
                "PCI_BUS10_WIDTH=3",
                "PCI_BUS12_HIERARCHY_TYPE=1",
                "PCI_BUS12_SPEED=4",
                "PCI_BUS12_WIDTH=3",
                "PCI_BUS14_HIERARCHY_TYPE=1",
                "PCI_BUS14_SPEED=4",
                "PCI_BUS14_WIDTH=3",
                "PCI_BUS16_HIERARCHY_TYPE=1",
                "PCI_BUS16_SPEED=4",
                "PCI_BUS16_WIDTH=3",
                "PF_TOTAL_SF=128",
                "PF_SF_BAR_SIZE=10",
                "PF_NUM_PF_MSIX=228",
                "LAG_RESOURCE_ALLOCATION=1",
                "PCI_SWITCH0_UPSTREAM_PORT_BUS=0",
                "PCI_SWITCH0_UPSTREAM_PORT_PEX=0",
            ],
        );
    }
}
