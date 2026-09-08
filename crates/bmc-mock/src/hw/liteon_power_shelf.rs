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

use std::borrow::Cow;

use mac_address::MacAddress;

use crate::{hw, redfish};

pub(crate) struct LiteOnPowerShelf<'a> {
    pub(crate) bmc_mac_address: MacAddress,
    pub(crate) product_serial_number: Cow<'a, str>,
}

impl LiteOnPowerShelf<'_> {
    fn sensor_layout() -> redfish::sensor::Layout {
        redfish::sensor::Layout {
            temperature: 20,
            fan: 6,
            power: 12,
            current: 12,
            voltage: 0,
        }
    }

    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        redfish::manager::Config {
            managers: vec![redfish::manager::SingleConfig {
                id: "bmc",
                eth_interfaces: Some(vec![
                    redfish::ethernet_interface::builder(
                        &redfish::ethernet_interface::manager_resource("bmc", "can0"),
                    )
                    .mac_address(MacAddress::new([0, 0, 0, 0, 0, 0]))
                    .interface_enabled(true)
                    .build(),
                    redfish::ethernet_interface::builder(
                        &redfish::ethernet_interface::manager_resource("bmc", "eth0"),
                    )
                    .mac_address(self.bmc_mac_address)
                    .interface_enabled(true)
                    .build(),
                ]),
                host_interfaces: None,
                serial_interfaces: None,
                firmware_version: Some("r1.3.9"),
                oem: None,
            }],
        }
    }

    pub(crate) fn system_config(&self) -> redfish::computer_system::Config {
        let system_id = "system";

        redfish::computer_system::Config {
            systems: vec![redfish::computer_system::SingleSystemConfig {
                id: Cow::Borrowed(system_id),
                manufacturer: None,
                model: None,
                bios_version: None,
                eth_interfaces: None,
                serial_number: None,
                boot_order_mode: redfish::computer_system::BootOrderMode::Generic,
                callbacks: None,
                chassis: vec!["powershelf".into()],
                boot_options: None,
                bios_mode: redfish::computer_system::BiosMode::Generic,
                oem: redfish::computer_system::Oem::Generic,
                log_services: None,
                storage: None,
                processors: None,
                memory: None,
                base_bios: Some(
                    redfish::bios::builder(&redfish::bios::resource(system_id)).build(),
                ),
                serial_console: Some(hw::openbmc::enabled_serial_console()),
                secure_boot_available: false,
            }],
        }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        let chassis_id = "powershelf";

        redfish::chassis::ChassisConfig {
            chassis: vec![redfish::chassis::SingleChassisConfig {
                id: chassis_id.into(),
                chassis_type: "Shelf".into(),
                manufacturer: Some("LITE-ON TECHNOLOGY CORP.".into()),
                part_number: Some("PF-1333-7R".into()),
                model: Some("PF-1333-7R".into()),
                serial_number: Some(self.product_serial_number.to_string().into()),
                sensors: Some(redfish::sensor::generate_chassis_sensors(
                    chassis_id,
                    Self::sensor_layout(),
                )),
                power_supplies: Some(
                    (0..=5)
                        .map(|idx| {
                            redfish::power_supply::builder(&redfish::power_supply::resource(
                                chassis_id,
                                &idx.to_string(),
                            ))
                            .oem_liteon_power_state(true)
                            // libredfish requires status to be
                            // here...
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
