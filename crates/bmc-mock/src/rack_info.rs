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

use std::collections::BTreeSet;

use crate::hw::lenovo_gb300_nvl72_rack::LenovoGB300Nvl72Rack;
use crate::hw::rack::{RackElevation, RackPlacement};
use crate::hw::wiwynn_gb200_nvl72_rack::WiwynnGB200Nvl72Rack;
use crate::{HardwareType, RackType};

const RACK_POSITION_MIN: u8 = 1;
const RACK_POSITION_MAX: u8 = 48;
const COMPUTE_TRAY_COUNT: usize = 18;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RackInfo {
    pub rack_type: RackType,
}

impl RackInfo {
    pub fn rack_elevation(&self) -> eyre::Result<RackElevation> {
        let elevation = match self.rack_type {
            RackType::WiwynnGb200Nvl72 => self.wiwynn_gb200_nvl72_rack().rack_elevation(),
            RackType::LenovoGb300Nvl72 => self.lenovo_gb300_nvl72_rack().rack_elevation(),
        };
        self.validate_elevation(&elevation)?;
        Ok(elevation)
    }

    pub fn placement(&self, position: u8) -> RackPlacement {
        match self.rack_type {
            RackType::WiwynnGb200Nvl72 => self.wiwynn_gb200_nvl72_rack().placement(position),
            RackType::LenovoGb300Nvl72 => self.lenovo_gb300_nvl72_rack().placement(position),
        }
    }

    fn wiwynn_gb200_nvl72_rack(&self) -> WiwynnGB200Nvl72Rack {
        WiwynnGB200Nvl72Rack
    }

    fn lenovo_gb300_nvl72_rack(&self) -> LenovoGB300Nvl72Rack {
        LenovoGB300Nvl72Rack
    }

    fn compute_tray_hardware_type(&self) -> HardwareType {
        match self.rack_type {
            RackType::WiwynnGb200Nvl72 => HardwareType::WiwynnGB200Nvl,
            RackType::LenovoGb300Nvl72 => HardwareType::LenovoGB300Nvl,
        }
    }

    fn validate_elevation(&self, elevation: &RackElevation) -> eyre::Result<()> {
        let mut occupied_positions = BTreeSet::new();
        for unit in &elevation.units {
            eyre::ensure!(
                (RACK_POSITION_MIN..=RACK_POSITION_MAX).contains(&unit.position),
                "{} rack design position {} must be between {} and {}",
                self.rack_type,
                unit.position,
                RACK_POSITION_MIN,
                RACK_POSITION_MAX
            );
            eyre::ensure!(
                occupied_positions.insert(unit.position),
                "{} rack design position {} is occupied more than once",
                self.rack_type,
                unit.position
            );
        }

        let expected_compute_type = self.compute_tray_hardware_type();
        let mut cbc_index_counts = [0_u8; COMPUTE_TRAY_COUNT];
        for unit in &elevation.units {
            if is_gbx00_compute_tray(unit.hardware_type)
                && unit.hardware_type != expected_compute_type
            {
                eyre::bail!(
                    "{} rack design position {} uses {}, expected {}",
                    self.rack_type,
                    unit.position,
                    unit.hardware_type,
                    expected_compute_type
                );
            }
            if unit.hardware_type != expected_compute_type {
                continue;
            }

            let placement = self.placement(unit.position);
            let compute_tray_index = placement.compute_tray_index().ok_or_else(|| {
                eyre::eyre!(
                    "{} compute tray at rack position {} cannot be mapped to a CBC index; compute trays must occupy positions 11 through 18 or 28 through 37",
                    self.rack_type,
                    unit.position
                )
            })?;
            cbc_index_counts[usize::from(compute_tray_index)] += 1;
        }

        for (compute_tray_index, machine_count) in cbc_index_counts.into_iter().enumerate() {
            eyre::ensure!(
                machine_count == 1,
                "{} rack design CBC compute tray index {} must resolve to exactly one machine, found {}",
                self.rack_type,
                compute_tray_index,
                machine_count
            );
        }

        Ok(())
    }
}

fn is_gbx00_compute_tray(hardware_type: HardwareType) -> bool {
    matches!(
        hardware_type,
        HardwareType::WiwynnGB200Nvl
            | HardwareType::LenovoGB300Nvl
            | HardwareType::NvidiaDgxGb300
            | HardwareType::SupermicroGb300Nvl
    )
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases};

    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum Mutation {
        None,
        OutOfRangePosition,
        DuplicatePosition,
        InvalidComputePosition,
        MissingComputeTray,
        WrongComputeHardware,
    }

    #[derive(Debug)]
    struct ValidationInput {
        rack_type: RackType,
        mutation: Mutation,
    }

    fn unvalidated_elevation(rack_info: RackInfo) -> RackElevation {
        match rack_info.rack_type {
            RackType::WiwynnGb200Nvl72 => rack_info.wiwynn_gb200_nvl72_rack().rack_elevation(),
            RackType::LenovoGb300Nvl72 => rack_info.lenovo_gb300_nvl72_rack().rack_elevation(),
        }
    }

    #[test]
    fn rack_placement_validation() {
        check_cases(
            [
                Case {
                    scenario: "valid WIWYNN GB200 rack",
                    input: ValidationInput {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        mutation: Mutation::None,
                    },
                    expect: Yields(()),
                },
                Case {
                    scenario: "valid Lenovo GB300 rack",
                    input: ValidationInput {
                        rack_type: RackType::LenovoGb300Nvl72,
                        mutation: Mutation::None,
                    },
                    expect: Yields(()),
                },
                Case {
                    scenario: "rack position outside the 48-position design",
                    input: ValidationInput {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        mutation: Mutation::OutOfRangePosition,
                    },
                    expect: FailsWith(
                        "WIWYNN GB200 NVL72 rack design position 0 must be between 1 and 48"
                            .to_string(),
                    ),
                },
                Case {
                    scenario: "duplicate occupied position",
                    input: ValidationInput {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        mutation: Mutation::DuplicatePosition,
                    },
                    expect: FailsWith(
                        "WIWYNN GB200 NVL72 rack design position 6 is occupied more than once"
                            .to_string(),
                    ),
                },
                Case {
                    scenario: "compute tray outside CBC ranges",
                    input: ValidationInput {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        mutation: Mutation::InvalidComputePosition,
                    },
                    expect: FailsWith(
                        "WIWYNN GB200 NVL72 compute tray at rack position 10 cannot be mapped to a CBC index; compute trays must occupy positions 11 through 18 or 28 through 37"
                            .to_string(),
                    ),
                },
                Case {
                    scenario: "CBC index without a machine",
                    input: ValidationInput {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        mutation: Mutation::MissingComputeTray,
                    },
                    expect: FailsWith(
                        "WIWYNN GB200 NVL72 rack design CBC compute tray index 17 must resolve to exactly one machine, found 0"
                            .to_string(),
                    ),
                },
                Case {
                    scenario: "compute hardware from the wrong design",
                    input: ValidationInput {
                        rack_type: RackType::WiwynnGb200Nvl72,
                        mutation: Mutation::WrongComputeHardware,
                    },
                    expect: FailsWith(
                        "WIWYNN GB200 NVL72 rack design position 11 uses Lenovo GB300 NVL, expected WIWYNN GB200 NVL"
                            .to_string(),
                    ),
                },
            ],
            |input| {
                let rack_info = RackInfo {
                    rack_type: input.rack_type,
                };
                let mut elevation = unvalidated_elevation(rack_info);
                match input.mutation {
                    Mutation::None => {}
                    Mutation::OutOfRangePosition => elevation.units[0].position = 0,
                    Mutation::DuplicatePosition => {
                        elevation.units[1].position = elevation.units[0].position;
                    }
                    Mutation::InvalidComputePosition => {
                        elevation
                            .units
                            .iter_mut()
                            .find(|unit| unit.position == 11)
                            .unwrap()
                            .position = 10;
                    }
                    Mutation::MissingComputeTray => {
                        elevation
                            .units
                            .iter_mut()
                            .find(|unit| unit.position == 37)
                            .unwrap()
                            .hardware_type = HardwareType::NvidiaSwitchNd5200Ld;
                    }
                    Mutation::WrongComputeHardware => {
                        elevation
                            .units
                            .iter_mut()
                            .find(|unit| unit.position == 11)
                            .unwrap()
                            .hardware_type = HardwareType::LenovoGB300Nvl;
                    }
                }

                rack_info
                    .validate_elevation(&elevation)
                    .map_err(|error| error.to_string())
            },
        );
    }
}
