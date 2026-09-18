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

use std::convert::identity;
use std::str::FromStr;

use carbide_network::{deserialize_input_mac_to_address, sanitized_mac};
use mac_address::MacAddress;
use model::site_explorer::{
    BlueFieldOperatingMode, BootOption as ModelBootOption, BootOrder as ModelBootOrder,
    ComputerSystem as ModelComputerSystem, ComputerSystemAttributes,
    EthernetInterface as ModelEthernetInterface, MachineSetupDiff, PCIeDevice,
    PowerState as ModelPowerState, SecureBootStatus, UefiDevicePath as ModelUefiDevicePath,
};
use nv_redfish::computer_system::boot_option::UefiDevicePath as BootOptionUefiDevicePath;
use nv_redfish::computer_system::{
    Bios, BootOption, ComputerSystem, SecureBoot, SecureBootCurrentBootType,
};
use nv_redfish::ethernet_interface::{EthernetInterface, UefiDevicePath as EthUefiDevicePath};
use nv_redfish::oem::nvidia::NvidiaComputerSystem;
use nv_redfish::pcie_device::PcieDevice;
use nv_redfish::resource::PowerState;
use nv_redfish::schema::computer_system::SerialConsoleProtocol;
use nv_redfish::{Bmc, Resource, ResourceProvidesStatus};
use regex::Regex;

use crate::{
    Config as ExploreConfig, Error, ErrorClass, ExploredChassisCollection, compare_boot_options, hw,
};

const UEFI_MAC_PATTERN_CAPTURE: &str = "mac";
lazy_static::lazy_static! {
    static ref UEFI_MAC_PATTERN: Regex = Regex::new(&format!(r"MAC\((?<{UEFI_MAC_PATTERN_CAPTURE}>[[:alnum:]]+)\,")).unwrap();
}

pub(crate) struct Config<'a, B: Bmc> {
    pub(crate) need_oem_nvidia_bluefield: bool,
    // Temporary workaround for BlueField DPU BMCs that intermittently return
    // HTTP 500 for the BIOS resource while the DPU is in NIC mode.
    pub(crate) ignore_500_on_bios_fetch: bool,
    // Temporary workaround for BlueField DPU BMCs that intermittently return
    // HTTP 404 for the OOB interface or the full EthernetInterfaces collection.
    // This is expected to be fixed in BMC firmware 24.10-39, which adds
    // internal retries.
    pub(crate) retry_404_on_eth_interfaces: bool,
    pub(crate) explore: &'a ExploreConfig<'a, B>,
}

pub(crate) struct ExploredComputerSystem<B: Bmc> {
    pub(crate) system: ComputerSystem<B>,
    pub(crate) bios: Option<Bios<B>>,
    pub(crate) boot_options: Vec<BootOption<B>>,
    ethernet_interfaces: Vec<EthernetInterface<B>>,
    oem_nvidia_bluefield: Option<NvidiaComputerSystem<B>>,
    secure_boot: Option<SecureBoot<B>>,
}

impl<B: Bmc> ExploredComputerSystem<B> {
    pub(crate) async fn explore(
        system: ComputerSystem<B>,
        config: &Config<'_, B>,
    ) -> Result<Self, Error<B>> {
        let boot_options = if let Some(collection) = system
            .boot_options()
            .await
            .map_err(Error::nv_redfish("boot options"))?
        {
            collection
                .members()
                .await
                .map_err(Error::nv_redfish("boot options members"))?
        } else {
            vec![]
        };

        let bios = Self::fetch_bios(&system, config).await?;

        let ethernet_interfaces = Self::fetch_eth_interfaces(&system, config)
            .await
            .map_err(Error::nv_redfish("system ethernet interfaces"))?;

        let oem_nvidia_bluefield = if config.need_oem_nvidia_bluefield {
            system
                .oem_nvidia()
                .await
                .map_err(Error::nv_redfish("NVIDIA system Bluefield OEM"))?
        } else {
            None
        };

        let secure_boot = system
            .secure_boot()
            .await
            .map_err(Error::nv_redfish("secure boot"))?;

        Ok(Self {
            system,
            bios,
            boot_options,
            ethernet_interfaces,
            oem_nvidia_bluefield,
            secure_boot,
        })
    }

    /// Chassis explicitly associated with this ComputerSystem by Redfish.
    pub(crate) fn linked_chassis_ids(&self) -> Vec<nv_redfish::core::ODataId> {
        self.system
            .raw()
            .links
            .as_ref()
            .and_then(|links| links.chassis.as_ref())
            .into_iter()
            .flatten()
            .map(|chassis| chassis.id().clone())
            .collect()
    }

    /// Whether the System EthernetInterfaces collection contains a usable MAC.
    pub(crate) fn has_usable_ethernet_mac_address(&self) -> bool {
        self.ethernet_interfaces.iter().any(|interface| {
            let mac_address = interface.mac_address();
            is_usable_ethernet_mac_address(
                interface.interface_enabled(),
                mac_address.as_ref().map(|mac_address| mac_address.as_str()),
            )
        })
    }

    async fn fetch_bios(
        system: &ComputerSystem<B>,
        config: &Config<'_, B>,
    ) -> Result<Option<Bios<B>>, Error<B>> {
        match system.bios().await {
            Ok(bios) => Ok(bios),
            Err(err) if config.ignore_500_on_bios_fetch => {
                if let nv_redfish::Error::Bmc(bmc_error) = &err
                    && (config.explore.error_classifier)(bmc_error)
                        == Some(ErrorClass::InternalServerError)
                {
                    // Ignore BlueField DPU BIOS HTTP 500 because it may fail in NIC mode.
                    tracing::warn!("ignoring HTTP 500 while fetching BlueField DPU BIOS");
                    Ok(None)
                } else {
                    Err(Error::nv_redfish("bios")(err))
                }
            }
            Err(err) => Err(Error::nv_redfish("bios")(err)),
        }
    }

    async fn fetch_eth_interfaces(
        system: &ComputerSystem<B>,
        config: &Config<'_, B>,
    ) -> Result<Vec<EthernetInterface<B>>, nv_redfish::Error<B>> {
        // Total tries: 3 (initial + 2 retries).
        let mut retries_remaining = 2;
        loop {
            let result = match system.ethernet_interfaces().await {
                Ok(Some(ifaces)) => ifaces.members().await,
                Ok(None) => Ok(vec![]),
                Err(err) => Err(err),
            };
            match result {
                Ok(v) => break Ok(v),
                Err(err) if config.retry_404_on_eth_interfaces && retries_remaining != 0 => {
                    if let nv_redfish::Error::Bmc(bmc_error) = &err
                        && (config.explore.error_classifier)(bmc_error)
                            == Some(ErrorClass::NotFound)
                    {
                        tracing::warn!(
                            retries_remaining,
                            "received 404 while fetching the system ethernet collection; retrying"
                        );
                        retries_remaining -= 1;
                        tokio::time::sleep(config.explore.retry_timeout).await;
                    } else {
                        break Err(err);
                    }
                }
                Err(err) => break Err(err),
            }
        }
    }
    pub(crate) fn to_model(
        &self,
        hw_type: Option<hw::HwType>,
        chassis: &ExploredChassisCollection<B>,
        pcie_devices: &[PcieDevice<B>],
    ) -> Result<ModelComputerSystem, Error<B>> {
        let hw_id = self.system.hardware_id();
        let is_dpu = hw_type == Some(hw::HwType::Bluefield);
        let ethernet_interfaces = self.ethernet_interfaces(hw_type)?;

        let mut base_mac = None;
        let mut nic_mode = None;
        let mut serial_number = hw_id.serial_number.map(|v| v.into_inner());
        if is_dpu {
            // This part processes dpu case and do two things such as
            // 1. update system serial_number in case it is empty using chassis serial_number
            // 2. format serial_number data using the same rules as in fetch_chassis()
            if serial_number.is_none() && !chassis.is_bluefield4() {
                serial_number = chassis.dpu_card1_serial_number()?;
            }

            if let Some(oem_bf) = &self.oem_nvidia_bluefield {
                base_mac = oem_bf.base_mac().and_then(|v| {
                    v.inner()
                        .parse()
                        .inspect_err(|err| {
                            tracing::warn!(
                                error = %err,
                                mac_address = %v,
                                "failed to parse BaseMAC"
                            );
                        })
                        .ok()
                });
                nic_mode = Self::bluefield_operating_mode(&self.system, self.bios.as_ref(), oem_bf);
            }
            let is_bf4_shape = chassis
                .members
                .iter()
                .any(|c| c.chassis.id().into_inner() == "BlueField_0");
            if base_mac.is_none() && is_bf4_shape {
                // BF4 temporary patch: some BMC firmware misses ComputerSystem
                // BaseMAC; patch from NDF0-derived base MAC (NDF0 - 0x10) if available.
                base_mac = chassis.dpu_bf4_ndf0_permanent_mac();
                if base_mac.is_none() {
                    tracing::warn!(
                        "BF4 NDF0 fallback did not provide PF0 base MAC (NIC inventory unavailable/uninitialized?)"
                    );
                }
            }
        }

        let boot_order = self.system.boot_order().map(|order| ModelBootOrder {
            boot_order: order
                .iter()
                .filter_map(|boot_ref| {
                    self.boot_options
                        .iter()
                        .find(|opt| opt.boot_reference() == *boot_ref)
                        .map(|opt| ModelBootOption {
                            id: opt.id().to_string(),
                            display_name: opt
                                .display_name()
                                .map(|v| v.to_string())
                                .unwrap_or("".into()),
                            uefi_device_path: opt.uefi_device_path().map(|v| v.to_string()),
                            boot_option_enabled: opt.enabled(),
                        })
                })
                .collect(),
        });

        let is_infinite_boot_enabled = hw_type
            .and_then(|hw_type| hw_type.infinite_boot_enabled_attr())
            .and_then(|attr| self.bios_attr_eq(&attr));

        let pcie_devices = pcie_devices
            .iter()
            .filter_map(|dev| pcie_device_to_model(hw_type, dev))
            .collect();

        let power_state = chassis
            .liteon_power_state()
            .map(|v| v.to_model())
            .unwrap_or_else(|| {
                self.system
                    .power_state()
                    .and_then(|v| match v {
                        PowerState::On => Some(ModelPowerState::On),
                        PowerState::Off => Some(ModelPowerState::Off),
                        PowerState::PoweringOn => Some(ModelPowerState::PoweringOn),
                        PowerState::PoweringOff => Some(ModelPowerState::PoweringOff),
                        PowerState::Paused => Some(ModelPowerState::Paused),
                        PowerState::Hibernating => Some(ModelPowerState::Hibernating),
                        PowerState::Sleeping => Some(ModelPowerState::Sleeping),
                        PowerState::UnsupportedValue => None,
                    })
                    .unwrap_or_default()
            });

        let bios_version = self
            .system
            .raw()
            .bios_version
            .clone()
            .flatten()
            .map(|version| version.trim().to_string())
            .filter(|version| !version.is_empty());

        let serial_console_ssh_port = self
            .system
            .raw()
            .serial_console
            .as_ref()
            .and_then(|serial_console| serial_console.ssh.as_ref())
            .map(enabled_serial_console_ssh_port)
            .transpose()
            .unwrap_or_else(|invalid_port| {
                tracing::warn!(
                    system_id = %self.system.id(),
                    serial_console_ssh_port = invalid_port,
                    "Ignoring invalid SSH serial-console port reported by Redfish",
                );
                None
            })
            .flatten();

        Ok(ModelComputerSystem {
            ethernet_interfaces,
            id: self.system.id().to_string(),
            manufacturer: hw_id.manufacturer.map(|v| v.to_string()),
            model: hw_id.model.map(|v| v.to_string()),
            serial_number: serial_number.map(|v| v.to_string()),
            attributes: ComputerSystemAttributes {
                nic_mode,
                is_infinite_boot_enabled,
            },
            pcie_devices,
            base_mac,
            power_state,
            sku: self.system.sku().map(|v| v.to_string()),
            boot_order,
            bios_version,
            serial_console_ssh_port,
        })
    }

    pub(crate) fn secure_boot_status(&self) -> Result<SecureBootStatus, Error<B>> {
        let secure_boot = self
            .secure_boot
            .as_ref()
            .ok_or_else(Error::bmc_not_provided("SecureBoot resource"))?;
        let enabled = secure_boot
            .secure_boot_enable()
            .ok_or_else(Error::bmc_not_provided(
                "SecureBootEnable in SecureBoot resource",
            ))?;
        let current_boot =
            secure_boot
                .secure_boot_current_boot()
                .ok_or_else(Error::bmc_not_provided(
                    "SecureBootCurrentBootType in SecureBoot resource",
                ))?;
        Ok(SecureBootStatus {
            is_enabled: enabled && current_boot == SecureBootCurrentBootType::Enabled,
        })
    }

    pub(crate) fn boot_order_first_option(&self) -> Option<&BootOption<B>> {
        self.system
            .boot_order()
            .as_ref()
            .and_then(|v| v.first())
            .and_then(|actual_ref| {
                self.boot_options
                    .iter()
                    .find(|opt| opt.boot_reference() == *actual_ref)
            })
    }

    pub(crate) fn check_boot_by_uefi_prefix(
        &self,
        boot_interface_mac: MacAddress,
    ) -> Option<MachineSetupDiff> {
        let expected = self.boot_option_by_uefi_prefix(boot_interface_mac);

        // Find actual option that is first in boot_order.
        let actual = self.boot_order_first_option();
        compare_boot_options(expected, actual)
    }

    pub(crate) fn check_boot_option_enabled_by_uefi_prefix(
        &self,
        boot_interface_mac: MacAddress,
    ) -> Option<MachineSetupDiff> {
        let option = self.boot_option_by_uefi_prefix(boot_interface_mac)?;
        (option.enabled() != Some(true)).then(|| MachineSetupDiff {
            key: "boot_option_enabled".to_string(),
            expected: "true".to_string(),
            actual: option
                .enabled()
                .map(|enabled| enabled.to_string())
                .unwrap_or_else(|| "Not provided".to_string()),
        })
    }

    fn boot_option_by_uefi_prefix(&self, boot_interface_mac: MacAddress) -> Option<&BootOption<B>> {
        self
            // Find UEFI device path of the ethernet interface
            // that has boot_interface_mac MAC address.
            .ethernet_interfaces
            .iter()
            .find(|eth| {
                eth.mac_address()
                    .map(|v| {
                        v.as_str()
                            .parse::<MacAddress>()
                            .is_ok_and(|v| v == boot_interface_mac)
                    })
                    .is_some_and(identity)
            })
            .and_then(|eth| eth.uefi_device_path())
            // Find boot option that starts with correponding
            // UEFI device path.
            .and_then(|eth_uefi_device_path| {
                self.boot_options.iter().find(|opt| {
                    opt.uefi_device_path().is_some_and(|path| {
                        is_uefi_tree_child(eth_uefi_device_path, path)
                            && path.inner().contains("/IPv4(")
                    })
                })
            })
    }

    fn ethernet_interfaces(
        &self,
        hw_type: Option<hw::HwType>,
    ) -> Result<Vec<ModelEthernetInterface>, Error<B>> {
        let is_bluefield = hw_type == Some(hw::HwType::Bluefield);
        let mut result = self
            .ethernet_interfaces
            .iter()
            .map(|iface| {
                let mac_address = iface
                    .mac_address()
                    .map(|addr| {
                        deserialize_input_mac_to_address(addr.as_str()).map_err(|e| {
                            Error::InvalidValue(format!("MAC address not valid: {addr} (err: {e})"))
                        })
                    })
                    .transpose()
                    .or_else(|err| {
                        if is_bluefield
                            || iface
                                .interface_enabled()
                                .is_some_and(|is_enabled| !is_enabled)
                        {
                            // Some BlueField firmware and disabled interfaces can populate
                            // MACAddress with junk. Keep the interface but omit its invalid MAC.
                            tracing::debug!(
                                interface_id = %iface.id(),
                                link_status = ?iface.link_status(),
                                error = %err,
                                "ignoring invalid system interface MAC address"
                            );
                            Ok(None)
                        } else {
                            Err(err)
                        }
                    })?;

                let uefi_device_path = iface
                    .uefi_device_path()
                    .map(|v| v.into_inner())
                    .map(ModelUefiDevicePath::from_str)
                    .transpose()
                    .map_err(|err| Error::InvalidValue(format!("UefiDevicePath: {err}")))?;

                Ok(ModelEthernetInterface {
                    description: iface.description().map(|d| d.to_string()),
                    id: Some(iface.id().to_string()),
                    interface_enabled: iface.interface_enabled(),
                    mac_address,
                    link_status: iface.link_status().map(|s| format!("{s:?}")),
                    uefi_device_path,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        if is_bluefield
            && !result.iter().any(|iface| {
                iface
                    .id
                    .as_ref()
                    .is_some_and(|v| v.to_lowercase().contains("oob"))
            })
        {
            // For Bluefield without OOB interface we craft ethernet
            // interface from boot options as workaround.
            if let Some(oob_iface) = self.oob_interface_from_boot_options()? {
                result.push(oob_iface);
            } else {
                tracing::warn!("Error getting OOB interface for the DPU");
            }
        }
        Ok(result)
    }

    fn oob_interface_from_boot_options(&self) -> Result<Option<ModelEthernetInterface>, Error<B>> {
        // Temporary workaround until oob mac would be possible to get via Redfish
        self.boot_options
            .iter()
            .find_map(|boot_opt| {
                // display_name: "NET-OOB-IPV4"
                if boot_opt
                    .display_name()
                    .is_some_and(|v| v.inner().contains("OOB"))
                {
                    boot_opt
                        .uefi_device_path()
                        .and_then(|path| UEFI_MAC_PATTERN.captures(path.inner()))
                } else {
                    None
                }
            })
            .and_then(|captures| captures.name(UEFI_MAC_PATTERN_CAPTURE))
            .map(|mac_capture| {
                sanitized_mac(mac_capture.as_str())
                    .map_err(|e| {
                        Error::InvalidValue(format!(
                            "MAC address not valid: {} (err: {e})",
                            mac_capture.as_str()
                        ))
                    })
                    .map(|mac_addr| ModelEthernetInterface {
                        description: Some("1G DPU OOB network interface".to_string()),
                        id: Some("oob_net0".to_string()),
                        interface_enabled: None,
                        mac_address: Some(mac_addr),
                        link_status: None,
                        uefi_device_path: None,
                    })
            })
            .transpose()
    }

    fn bluefield_operating_mode(
        system: &ComputerSystem<B>,
        bios: Option<&Bios<B>>,
        bf_ncs: &NvidiaComputerSystem<B>,
    ) -> Option<BlueFieldOperatingMode> {
        let hw_id = system.hardware_id();
        let manufacturer = hw_id.manufacturer.map(|v| v.into_inner());
        let model = hw_id.model.map(|v| v.into_inner());
        match manufacturer {
            None | Some("Nvidia") | Some("https://www.mellanox.com") => {
                match model {
                    None
                    | Some("BlueField-3 DPU")
                    | Some("Bluefield 3 DPU")
                    | Some("BlueField-3 SmartNIC Main Card")
                    | Some("Bluefield 3 SmartNIC Main Card") => {
                        use nv_redfish::oem::nvidia::computer_system::Mode;
                        bf_ncs.mode().and_then(|v| match v {
                            Mode::DpuMode => Some(BlueFieldOperatingMode::Dpu),
                            Mode::NicMode => Some(BlueFieldOperatingMode::Nic),
                            Mode::UnsupportedValue => None,
                        })
                    }
                    Some("Bluefield 2 SmartNIC Main Card") | Some("Bluefield SoC") => {
                        // Get from bios
                        bios.and_then(|bios| bios.attribute("NicMode"))
                            .and_then(|attr| {
                                attr.str_value().and_then(|v| match v {
                                    "NicMode" => Some(BlueFieldOperatingMode::Nic),
                                    "DpuMode" => Some(BlueFieldOperatingMode::Dpu),
                                    _ => None,
                                })
                            })
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    fn bios_attr_eq(&self, expected: &hw::BiosAttr) -> Option<bool> {
        self.bios
            .as_ref()
            .and_then(|bios| bios.attribute(expected.key))
            .map(|actual| match expected.value {
                hw::BiosAttrValue::Str(v) => actual.str_value() == Some(v),
                hw::BiosAttrValue::Bool(v) => actual.bool_value() == Some(v),
                hw::BiosAttrValue::Int(v) => actual.integer_value() == Some(v),
                hw::BiosAttrValue::AnyStr(v) => v.iter().any(|v| actual.str_value() == Some(v)),
            })
    }

    pub(crate) fn verify_bios_attr(&self, expected: &hw::BiosAttr<'_>) -> Option<MachineSetupDiff> {
        if let Some(actual) = self
            .bios
            .as_ref()
            .and_then(|bios| bios.attribute(expected.key))
            && !match expected.value {
                hw::BiosAttrValue::Bool(v) => actual.bool_value() == Some(v),
                hw::BiosAttrValue::Str(v) => actual.str_value() == Some(v),
                hw::BiosAttrValue::Int(v) => actual.integer_value() == Some(v),
                hw::BiosAttrValue::AnyStr(v) => v.iter().any(|v| actual.str_value() == Some(v)),
            }
        {
            Some(MachineSetupDiff {
                key: expected.key.to_string(),
                expected: expected.value.to_string(),
                actual: actual
                    .str_value()
                    .map(|v| v.to_string())
                    .or_else(|| actual.bool_value().map(|v| v.to_string()))
                    .or_else(|| actual.integer_value().map(|v| v.to_string()))
                    .unwrap_or_else(|| "unexpected type".to_string()),
            })
        } else {
            None
        }
    }
}

fn is_usable_ethernet_mac_address(
    interface_enabled: Option<bool>,
    mac_address: Option<&str>,
) -> bool {
    interface_enabled.is_none_or(identity)
        && mac_address
            .is_some_and(|mac_address| deserialize_input_mac_to_address(mac_address).is_ok())
}

fn is_uefi_tree_child(
    parent: EthUefiDevicePath<&str>,
    child: BootOptionUefiDevicePath<&str>,
) -> bool {
    let child_str = child.inner();
    let parent_str = parent.inner();
    // Here is exampl of child path:
    // "PciRoot(0x0)/Pci(0x10,0x0)/Pci(0x0,0x0)/MAC(5CFF35FE04BC,0x1)/IPv4(0.0.0.0,0x0,DHCP,0.0.0.0,0.0.0.0,0.0.0.0)/Uri()"
    // With root path:
    // "PciRoot(0x0)/Pci(0x10,0x0)/Pci(0x0,0x0)
    //
    // Also HPE can provide Acpi(0x00168E09,0x3)/Pci(0x10,0x0)/Pci(0x0,0x0)/...
    // instead of PciRoot(0x3)/Pci(0x10,0x0)/Pci(0x0,0x0)
    const HPE_ACPI_PREFIX: &str = "Acpi(0x00168E09,";
    const PCI_ROOT_PREFIX: &str = "PciRoot(";

    if child_str.starts_with(HPE_ACPI_PREFIX) && parent_str.starts_with(PCI_ROOT_PREFIX) {
        child_str[HPE_ACPI_PREFIX.len()..].starts_with(&parent_str[PCI_ROOT_PREFIX.len()..])
    } else {
        child_str.starts_with(parent_str)
    }
}

fn pcie_device_to_model<B: Bmc>(
    hw_type: Option<hw::HwType>,
    dev: &PcieDevice<B>,
) -> Option<PCIeDevice> {
    let hw_id = dev.hardware_id();
    let status = dev.status();
    hw_id.manufacturer?;
    if status.as_ref().is_some_and(|s| {
        s.state
            .is_some_and(|v| v != nv_redfish::resource::State::Enabled)
    }) {
        return None;
    }

    Some(PCIeDevice {
        description: dev.description().map(|v| v.to_string()),
        firmware_version: dev.firmware_version().map(|v| v.to_string()),
        id: Some(dev.id().to_string()),
        manufacturer: hw_id.manufacturer.map(|v| v.to_string()),
        // TODO: In old model it is dev.gpu_vendor, but it is not
        // standard. It can be taken from
        // .Oem.Supermicro.GPUDevice.GPUVendor for Supermicro but it
        // was never implemented.
        gpu_vendor: None,
        name: Some(dev.name().to_string()),
        part_number: hw_id.part_number.map(|v| v.to_string()),
        // Trim of serial_number is added because serial number of DPU
        // contains trailing spaces... Probably, it should be code
        // specific for DPU...
        serial_number: hw_id.serial_number.map(|v| {
            if hw_type == Some(hw::HwType::Hpe) {
                // TODO: This is how it is implemented in
                // libredfish. I'm quite sure that it should be same
                // way for all vendors but is unknown if it safe to
                // change
                v.inner().trim().to_string()
            } else {
                v.inner().to_string()
            }
        }),
        // TODO: Should not be converted to string....
        status: status.map(|status| model::site_explorer::SystemStatus {
            health: status
                .health
                .and_then(|v| match v {
                    nv_redfish::resource::Health::Ok => Some("OK"),
                    nv_redfish::resource::Health::Warning => Some("Warning"),
                    nv_redfish::resource::Health::Critical => Some("Critical"),
                    nv_redfish::resource::Health::UnsupportedValue => None,
                })
                .map(ToString::to_string),
            health_rollup: status
                .health_rollup
                .and_then(|v| match v {
                    nv_redfish::resource::Health::Ok => Some("OK"),
                    nv_redfish::resource::Health::Warning => Some("Warning"),
                    nv_redfish::resource::Health::Critical => Some("Critical"),
                    nv_redfish::resource::Health::UnsupportedValue => None,
                })
                .map(ToString::to_string),
            // Not enabled devices are filtered by code above.
            state: status
                .state
                .map(|_| "Enabled".to_string())
                .unwrap_or("".into()),
        }),
    })
}

fn enabled_serial_console_ssh_port(ssh: &SerialConsoleProtocol) -> Result<Option<u16>, i64> {
    ssh.service_enabled
        .filter(|enabled| *enabled)
        .and_then(|_| ssh.port.flatten())
        .map(|port| {
            let converted = u16::try_from(port).map_err(|_| port)?;
            (converted != 0).then_some(converted).ok_or(port)
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::{
        SerialConsoleProtocol, enabled_serial_console_ssh_port, is_usable_ethernet_mac_address,
    };

    #[test]
    fn extracts_only_enabled_valid_ssh_serial_console_ports() {
        let cases = [
            ("enabled", Some(true), Some(Some(2200)), Ok(Some(2200))),
            ("disabled", Some(false), Some(Some(2200)), Ok(None)),
            ("enabled state absent", None, Some(Some(2200)), Ok(None)),
            ("port absent", Some(true), None, Ok(None)),
            ("port null", Some(true), Some(None), Ok(None)),
            ("zero port", Some(true), Some(Some(0)), Err(())),
            ("negative port", Some(true), Some(Some(-1)), Err(())),
            (
                "maximum port",
                Some(true),
                Some(Some(i64::from(u16::MAX))),
                Ok(Some(u16::MAX)),
            ),
            (
                "port above u16 range",
                Some(true),
                Some(Some(i64::from(u16::MAX) + 1)),
                Err(()),
            ),
        ];

        for (name, service_enabled, port, expected) in cases {
            assert_eq!(
                enabled_serial_console_ssh_port(&SerialConsoleProtocol {
                    service_enabled,
                    port,
                    shared_with_manager_cli: None,
                    console_entry_command: None,
                    hot_key_sequence_display: None,
                })
                .map_err(drop),
                expected,
                "{name}",
            );
        }
    }

    #[test]
    fn usable_ethernet_mac_address_cases() {
        value_scenarios!(run = |(interface_enabled, mac_address)| {
            is_usable_ethernet_mac_address(interface_enabled, mac_address)
        };
            "enabled interface with a valid MAC" {
                (Some(true), Some("94:6d:ae:53:cb:9b")) => true,
            }
            "interface without an enabled state and with a valid MAC" {
                (None, Some("94:6d:ae:53:cb:9b")) => true,
            }
            "disabled interface with a placeholder MAC" {
                (Some(false), Some("00:00:00:00:00:00")) => false,
            }
            "enabled interface with an invalid MAC" {
                (Some(true), Some("not-a-mac")) => false,
            }
            "enabled interface without a MAC" {
                (Some(true), None) => false,
            }
        );
    }
}
