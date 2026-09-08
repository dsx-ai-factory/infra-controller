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

use crate::{BootOptionKind, Callbacks, LogService, LogServices, hw, redfish};

pub(crate) struct DellPowerEdgeR750<'a> {
    pub(crate) bmc_mac_address: MacAddress,
    pub(crate) product_serial_number: Cow<'a, str>,
    pub(crate) nics: Vec<(hw::nic::SlotNumber, hw::nic::Nic<'a>)>,
    pub(crate) embedded_nic: EmbeddedNic,
}

pub(crate) struct EmbeddedNic {
    pub(crate) port_1: MacAddress,
    pub(crate) port_2: MacAddress,
}

struct DellEventLog {
    entries: Vec<String>,
}

impl LogService for DellEventLog {
    fn id(&self) -> &str {
        "EventLog"
    }

    fn entries(&self, collection: &redfish::Collection<'_>) -> Vec<serde_json::Value> {
        self.entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| {
                redfish::log_service::event_entry(collection, &idx.to_string())
                    .message(entry)
                    // Severity + Created are not required by the Redfish spec but
                    // libredfish expects them (mirrors the BlueField event log).
                    .severity("OK")
                    .created("2026-02-12T02:06:58+00:00")
                    .build()
            })
            .collect()
    }
}

struct DellLogServices {
    event_log: DellEventLog,
}

impl LogServices for DellLogServices {
    fn services(&self) -> Vec<&(dyn LogService + '_)> {
        vec![&self.event_log as &dyn LogService]
    }
}

impl DellPowerEdgeR750<'_> {
    fn sensor_layout() -> redfish::sensor::Layout {
        redfish::sensor::Layout {
            temperature: 10,
            fan: 10,
            power: 20,
            current: 10,
            voltage: 0,
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
                firmware_version: Some("6.00.30.00"),
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

        let eth_interfaces = [
            (1, &self.embedded_nic.port_1),
            (2, &self.embedded_nic.port_2),
        ]
        .into_iter()
        .map(|(port, mac)| {
            let eth_id = format!("NIC.Embedded.{port}-1-1");
            let resource = redfish::ethernet_interface::system_resource(system_id, &eth_id);
            redfish::ethernet_interface::builder(&resource)
                .description(&format!("Embedded NIC 1 Port {port} Partition 1"))
                .mac_address(*mac)
                .interface_enabled(true)
                .build()
        })
        .chain(self.nics.iter().map(|(slot_number, nic)| {
            let eth_id = format!("NIC.Slot.{slot_number}-1");
            let resource = redfish::ethernet_interface::system_resource(system_id, &eth_id);
            redfish::ethernet_interface::builder(&resource)
                .description(&format!("NIC in Slot {slot_number} Port 1"))
                .mac_address(nic.mac_address)
                .interface_enabled(true)
                .build()
        }))
        .collect();

        let boot_opt_builder = |id: &str, kind| {
            redfish::boot_option::builder(&redfish::boot_option::resource(system_id, id), kind)
                .boot_option_reference(id)
        };
        let boot_options = self
            .nics
            .iter()
            .map(|(slot_number, _)| {
                (
                    format!("HTTP Device 1: NIC in Slot {slot_number} Port 1"),
                    BootOptionKind::Network,
                )
            })
            .chain(std::iter::once((
                "PCIe SSD in Slot 2 in Bay 1: EFI Fixed Disk Boot Device 1".to_string(),
                BootOptionKind::Disk,
            )))
            .enumerate()
            .map(|(index, (display_name, kind))| {
                boot_opt_builder(&format!("Boot{index:04X}"), kind)
                    .display_name(&display_name)
                    .build()
            })
            .collect::<Vec<_>>();

        redfish::computer_system::Config {
            systems: vec![redfish::computer_system::SingleSystemConfig {
                id: Cow::Borrowed(system_id),
                manufacturer: Some("Dell Inc.".into()),
                model: Some("PowerEdge R750".into()),
                bios_version: None,
                eth_interfaces: Some(eth_interfaces),
                serial_number,
                boot_order_mode: redfish::computer_system::BootOrderMode::OrderedCollection,
                callbacks,
                chassis: vec!["System.Embedded.1".into()],
                boot_options: Some(boot_options),
                bios_mode: redfish::computer_system::BiosMode::DellOem,
                oem: redfish::computer_system::Oem::Generic,
                log_services: Some(Arc::new(DellLogServices {
                    event_log: DellEventLog {
                        // Always report a completed reboot so the controller's
                        // restart verification (which reads the host BMC event
                        // log) can confirm reboots for this host.
                        entries: vec!["Server reset.".to_string()],
                    },
                })),
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
                        "HttpDev1Interface": "NIC.Slot.5-1",
                    })).build()),
            }],
        }
    }

    pub(crate) fn chassis_config(&self) -> redfish::chassis::ChassisConfig {
        let chassis_id = "System.Embedded.1";
        let net_adapter_builder = |id: &str| {
            redfish::network_adapter::builder(&redfish::network_adapter::chassis_resource(
                chassis_id, id,
            ))
        };
        let network_adapters = std::iter::once(
            net_adapter_builder("NIC.Embedded.1")
                .manufacturer("Broadcom Inc. and subsidiaries")
                .build(),
        )
        .chain(self.nics.iter().map(|(slot, nic)| {
            let network_adapter_id = format!("NIC.Slot.{slot}");
            let function_id = format!("NIC.Slot.{slot}-1");
            let func_resource = &redfish::network_device_function::chassis_resource(
                chassis_id,
                &network_adapter_id,
                &function_id,
            );
            let function = redfish::network_device_function::builder(func_resource)
                .ethernet(json!({"MACAddress": &nic.mac_address}))
                .oem(redfish::oem::dell::network_device_function::dell_nic_info(
                    &function_id,
                    *slot,
                    nic.serial_number
                        .as_ref()
                        .unwrap_or(&Cow::Borrowed("unknown")),
                ))
                .build();
            redfish::network_adapter::builder_from_nic(
                &redfish::network_adapter::chassis_resource(chassis_id, &network_adapter_id),
                nic,
            )
            .network_device_functions(
                &redfish::network_device_function::chassis_collection(
                    chassis_id,
                    &network_adapter_id,
                ),
                vec![function],
            )
            .status(redfish::resource::Status::Ok)
            .build()
        }))
        .collect();

        let pcie_devices = self
            .nics
            .iter()
            .map(|(slot, nic)| {
                let pcie_device_id = format!("mat_{}", slot);
                redfish::pcie_device::builder_from_nic(
                    &redfish::pcie_device::chassis_resource(chassis_id, &pcie_device_id),
                    nic,
                )
                .status(redfish::resource::Status::Ok)
                .build()
            })
            .collect();

        redfish::chassis::ChassisConfig {
            chassis: vec![redfish::chassis::SingleChassisConfig {
                id: Cow::Borrowed(chassis_id),
                chassis_type: "RackMount".into(),
                manufacturer: Some("Dell Inc.".into()),
                part_number: Some("01J4WFA05".into()),
                model: Some("PowerEdge R750".into()),
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
            firmware_inventory: [
                // BMC (iDRAC) version matches manager_config firmware_version.
                ("HostBMC_0", "6.00.30.00"),
                // Representative BIOS version for Dell PowerEdge R750.
                ("HostBIOS_0", "2.22.2"),
            ]
            .iter()
            .map(|(id, version)| {
                redfish::software_inventory::builder(
                    &redfish::software_inventory::firmware_inventory_resource(id),
                )
                .version(version)
                .build()
            })
            .collect(),
            advertise_multipart_push_uri: false,
            host_bmc_inventory_id: Some("HostBMC_0".to_string()),
            host_uefi_inventory_id: Some("HostBIOS_0".to_string()),
            ..Default::default()
        }
    }
}
