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
use std::sync::Arc;

use mac_address::MacAddress;
use serde_json::json;

use crate::{BootOptionKind, Callbacks, hw, redfish};

pub(crate) struct DellPowerEdgeR760Bf4<'a> {
    pub(crate) bmc_mac_address: MacAddress,
    pub(crate) product_serial_number: Cow<'a, str>,
    pub(crate) bf4: hw::nic::Nic<'a>,
}

const BF4_SLOT: hw::nic::SlotNumber = 2;

impl DellPowerEdgeR760Bf4<'_> {
    fn sensor_layout() -> redfish::sensor::Layout {
        // R760 BF4 System.Embedded.1 dump contains 38 sensors total. The
        // generic mock layout currently models Temperature, Fan, Power,
        // Current, and Voltage. Missing R760 BF4 ReadingType counts:
        // Percent=4, Frequency=2.
        redfish::sensor::Layout {
            temperature: 5,
            fan: 12,
            power: 5,
            current: 2,
            voltage: 8,
        }
    }

    pub(crate) fn manager_config(&self) -> redfish::manager::Config {
        redfish::manager::Config {
            managers: vec![redfish::manager::SingleConfig {
                id: "iDRAC.Embedded.1",
                eth_interfaces: Some(vec![
                    redfish::ethernet_interface::builder(
                        &redfish::ethernet_interface::manager_resource("iDRAC.Embedded.1", "NIC.1"),
                    )
                    .mac_address(self.bmc_mac_address)
                    .interface_enabled(true)
                    .build(),
                ]),
                host_interfaces: Some(vec![
                    redfish::host_interface::builder(&redfish::host_interface::manager_resource(
                        "iDRAC.Embedded.1",
                        "Host.1",
                    ))
                    .interface_enabled(false)
                    .build(),
                ]),
                serial_interfaces: None,
                firmware_version: Some("7.10.50.00"),
                oem: Some(redfish::manager::Oem::Dell),
            }],
        }
    }

    pub(crate) fn system_config(
        &self,
        callbacks: Arc<dyn Callbacks>,
    ) -> redfish::computer_system::Config {
        let callbacks = Some(callbacks);
        let serial_number = Some(self.product_serial_number.to_string().into());
        let system_id = "System.Embedded.1";

        let eth_id = format!("NIC.Slot.{BF4_SLOT}-1");
        let eth_interfaces = vec![
            redfish::ethernet_interface::builder(&redfish::ethernet_interface::system_resource(
                system_id, &eth_id,
            ))
            .description(&format!("NIC in Slot {BF4_SLOT} Port 1"))
            .mac_address(self.bf4.mac_address)
            .interface_enabled(true)
            .build(),
        ];

        let boot_opt_builder = |id: &str, kind| {
            redfish::boot_option::builder(&redfish::boot_option::resource(system_id, id), kind)
                .boot_option_reference(id)
        };
        let boot_options = vec![
            boot_opt_builder("Boot0000", BootOptionKind::Network)
                .display_name(&format!("HTTP Device 1: NIC in Slot {BF4_SLOT} Port 1"))
                .build(),
            boot_opt_builder("Boot0001", BootOptionKind::Disk)
                .display_name("Unavailable: Ubuntu")
                .uefi_device_path("HD(1,GPT,2D26D4C6-A7B5-4F71-93D4-7F6DF36BC8A8,0x800,0x219800)/\\EFI\\ubuntu\\shimx64.efi")
                .build(),
        ];

        redfish::computer_system::Config {
            systems: vec![redfish::computer_system::SingleSystemConfig {
                id: Cow::Borrowed(system_id),
                manufacturer: Some("Dell Inc.".into()),
                model: Some("PowerEdge R760".into()),
                bios_version: None,
                eth_interfaces: Some(eth_interfaces),
                serial_number,
                boot_order_mode: redfish::computer_system::BootOrderMode::OrderedCollection,
                callbacks,
                chassis: vec!["System.Embedded.1".into()],
                boot_options: Some(boot_options),
                bios_mode: redfish::computer_system::BiosMode::DellOem,
                oem: redfish::computer_system::Oem::Generic,
                log_services: None,
                // Today carbide need for any Dell to have storage
                // collection. It tries to find BOSS controller
                // there. So we provide empty collection to avoid 404
                // failure.
                storage: Some(vec![]),
                processors: None,
                memory: None,
                serial_console: None,
                secure_boot_available: true,
                base_bios: Some(redfish::bios::builder(&redfish::bios::resource(system_id))
                    .attributes(json!({
                        "BootSeqRetry": "Disabled",
                        "SetBootOrderEn": "NIC.HttpDevice.1-1,Disk.Bay.2:Enclosure.Internal.0-1",
                        "InBandManageabilityInterface": "Enabled",
                        "UefiVariableAccess": "Standard",
                        "SerialComm": "OnConRedir",
                        "SerialPortAddress": "Com1",
                        "FailSafeBaud": "115200",
                        "ConTermType": "Vt100Vt220",
                        "RedirAfterBoot": "Enabled",
                        "SriovGlobalEnable": "Enabled",
                        "TpmSecurity": "On",
                        "Tpm2Algorithm": "SHA256",
                        "Tpm2Hierarchy": "Enabled",
                        "HttpDev1EnDis": "Enabled",
                        "PxeDev1EnDis": "Disabled",
                        "HttpDev1Interface": eth_id,
                        "SystemBiosVersion": "2.2.7",
                        "SystemManufacturer": "Dell Inc.",
                        "SystemModelName": "PowerEdge R760",
                    })).build()),
            }],
        }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        let chassis_id = "System.Embedded.1";
        let network_adapter_id = format!("NIC.Slot.{BF4_SLOT}");
        let function_id = format!("NIC.Slot.{BF4_SLOT}-1");
        let function = redfish::network_device_function::builder(
            &redfish::network_device_function::chassis_resource(
                chassis_id,
                &network_adapter_id,
                &function_id,
            ),
        )
        .ethernet(json!({"MACAddress": &self.bf4.mac_address}))
        .oem(redfish::oem::dell::network_device_function::dell_nic_info(
            &function_id,
            BF4_SLOT,
            self.bf4
                .serial_number
                .as_ref()
                .unwrap_or(&Cow::Borrowed("unknown")),
        ))
        .build();
        let network_adapters = vec![
            redfish::network_adapter::builder_from_nic(
                &redfish::network_adapter::chassis_resource(chassis_id, &network_adapter_id),
                &self.bf4,
            )
            .network_device_functions(
                &redfish::network_device_function::chassis_collection(
                    chassis_id,
                    &network_adapter_id,
                ),
                vec![function],
            )
            .status(redfish::resource::Status::Ok)
            .build(),
        ];

        let pcie_device_id = format!("mat_{BF4_SLOT}");
        let pcie_devices = vec![
            redfish::pcie_device::builder_from_nic(
                &redfish::pcie_device::chassis_resource(chassis_id, &pcie_device_id),
                &self.bf4,
            )
            .status(redfish::resource::Status::Ok)
            .build(),
        ];

        redfish::chassis::ChassisConfig {
            chassis: vec![redfish::chassis::SingleChassisConfig {
                id: Cow::Borrowed(chassis_id),
                chassis_type: "RackMount".into(),
                manufacturer: Some("Dell Inc.".into()),
                part_number: Some("0C9W19A01".into()),
                model: Some("PowerEdge R760".into()),
                serial_number: Some(self.product_serial_number.to_string().into()),
                network_adapters: Some(network_adapters),
                pcie_devices: Some(pcie_devices),
                sensors: Some(redfish::sensor::generate_chassis_sensors(
                    chassis_id,
                    Self::sensor_layout(),
                )),
                ..redfish::chassis::SingleChassisConfig::defaults()
            }],
        }
    }

    pub(crate) fn update_service_config(&self) -> redfish::update_service::UpdateServiceConfig {
        redfish::update_service::UpdateServiceConfig {
            firmware_inventory: vec![],
            advertise_multipart_push_uri: false,
            ..Default::default()
        }
    }
}
