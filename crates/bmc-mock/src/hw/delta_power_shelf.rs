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

//! Delta Energy Systems power shelf.
//!
//! Modeled on the real Delta scrape under
//! `libredfish/tests/mockups/delta_powershelf/`. Two traits distinguish it
//! from the Lite-On shelf and drive the site-explorer Delta code path:
//!
//! * There is **no `/redfish/v1/Systems` collection** — the service root does
//!   not advertise `Systems` and the collection endpoint 404s (see
//!   [`crate::HardwareType::DeltaPowerShelf`] wiring in
//!   `machine_info`/`mock_machine_router`). This reproduces the original
//!   ingestion failure that Delta support fixes.
//! * Per-PSU power state is carried under `Oem.deltaenergysystems.Power`
//!   rather than the standard `PowerState` field.

use std::borrow::Cow;

use mac_address::MacAddress;

use crate::redfish;

/// Chassis id reported by a Delta power shelf (matches the scrape).
const CHASSIS_ID: &str = "chassis";

/// Default per-PSU power states: the six-bay shelf the real scrape reports,
/// all outputting power.
pub(crate) const DEFAULT_PSU_POWER: &[bool] = &[true; 6];

pub(crate) struct DeltaPowerShelf<'a> {
    pub(crate) bmc_mac_address: MacAddress,
    pub(crate) product_serial_number: Cow<'a, str>,
    /// Commanded on/off state per PSU bay, reported under
    /// `Oem.deltaenergysystems.Power`. One entry per `PowerSupplyUnit`.
    pub(crate) psu_power: Cow<'a, [bool]>,
}

impl DeltaPowerShelf<'_> {
    pub(crate) fn event_service_config(&self) -> Option<crate::EventServiceConfig> {
        Some(crate::EventServiceConfig::default())
    }

    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        redfish::manager::Config {
            managers: vec![redfish::manager::SingleConfig {
                id: "SMC",
                eth_interfaces: Some(vec![
                    redfish::ethernet_interface::builder(
                        &redfish::ethernet_interface::manager_resource("SMC", "eth0"),
                    )
                    .mac_address(self.bmc_mac_address)
                    .interface_enabled(true)
                    .build(),
                ]),
                host_interfaces: None,
                serial_interfaces: None,
                firmware_version: Some("01.04.01.04"),
                oem: None,
            }],
        }
    }

    /// Delta power shelves expose no `ComputerSystem`; the collection is empty
    /// and (via the `exposes_computer_systems` gate) is not advertised or
    /// served. Site-explorer synthesizes a system from the chassis instead.
    pub(crate) fn system_config(&self) -> redfish::computer_system::Config {
        redfish::computer_system::Config { systems: vec![] }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        redfish::chassis::ChassisConfig {
            chassis: vec![redfish::chassis::SingleChassisConfig {
                id: CHASSIS_ID.into(),
                chassis_type: "RackMount".into(),
                manufacturer: Some("DELTA".into()),
                part_number: Some("ECD68000048".into()),
                model: Some("810".into()),
                serial_number: Some(self.product_serial_number.to_string().into()),
                sensors: None,
                power_supplies: Some(
                    // The scrape numbers PSU bays "PowerSupplyUnit 1"..;
                    // power is reported under Oem.deltaenergysystems.Power.
                    self.psu_power
                        .iter()
                        .enumerate()
                        .map(|(idx, &on)| {
                            redfish::power_supply::builder(&redfish::power_supply::resource(
                                CHASSIS_ID,
                                &format!("PowerSupplyUnit {}", idx + 1),
                            ))
                            .oem_delta_power_state(on)
                            .status(redfish::resource::Status::Ok)
                            .build()
                        })
                        .collect(),
                ),
                ..redfish::chassis::SingleChassisConfig::defaults()
            }],
        }
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: vec![],
            ..Default::default()
        }
    }
}
