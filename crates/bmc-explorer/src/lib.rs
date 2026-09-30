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

mod chassis;
mod computer_system;
mod error;
pub mod hw;
mod inventories;
mod manager;
mod network_adapter;
#[cfg(any(test, feature = "test-support"))]
pub mod test_support;
use std::collections::HashMap;
use std::convert::identity;
use std::sync::Arc;
use std::time::Duration;

use chassis::ExploredChassisCollection;
use computer_system::ExploredComputerSystem;
pub use error::Error;
use inventories::ExploredInventories;
use itertools::Itertools;
use mac_address::MacAddress;
use manager::ExploredManager;
use model::site_explorer::{
    EndpointExplorationReport, EndpointType, InternalLockdownStatus, LockdownStatus,
    MachineSetupDiff, MachineSetupStatus,
};
use nv_redfish::assembly::Model as AssemblyModel;
use nv_redfish::computer_system::BootOption;
use nv_redfish::oem::ami::config_bmc::{
    LockdownBiosSettingsChangeState, LockdownBiosUpgradeDowngradeState,
    LockoutBiosVariableWriteMode, LockoutHostControlState,
};
use nv_redfish::oem::lenovo::computer_system::{FpMode, PortSwitchingTo};
use nv_redfish::oem::lenovo::manager::KcsState;
use nv_redfish::oem::lenovo::security_service::FwRollbackState;
use nv_redfish::oem::supermicro::Privilege as SupermicroPrivilege;
use nv_redfish::resource::{ResourceIdRef, ResourceNameRef};
pub use nv_redfish::service_root::Product;
use nv_redfish::service_root::Vendor;
use nv_redfish::{Bmc, Resource, ServiceRoot};

#[derive(PartialEq, Eq)]
pub enum ErrorClass {
    NotFound,
    InternalServerError,
}

pub type ErrorClassifier<'a, B> = &'a (dyn Fn(&<B as Bmc>::Error) -> Option<ErrorClass> + Sync);

fn is_bluefield_system_id(id: ResourceIdRef<'_>) -> bool {
    matches!(id.into_inner(), "Bluefield" | "BlueField_0")
}

pub struct Config<'a, B: Bmc> {
    pub boot_interface_mac: Option<MacAddress>,
    pub error_classifier: ErrorClassifier<'a, B>,
    pub retry_timeout: Duration,
}

pub fn is_bf4_product(product: Option<Product<&str>>) -> bool {
    // TODO: we should use part_number similar to BF3.
    product == Some(Product::new("B4240V")) || product == Some(Product::new("BlueField-4"))
}

/// Builds the chassis exploration config shared by [`nv_generate_exploration_report`]
/// and the [`detect_hw_type`] accessor, so detection cannot drift between them.
fn build_chassis_explore_config<B: Bmc>(root: &ServiceRoot<B>) -> chassis::Config {
    let is_nvidia_vendor = root.vendor() == Some(Vendor::new("Nvidia"))
        || root.vendor() == Some(Vendor::new("NVIDIA"));
    let need_bf4_network_device_fns = is_nvidia_vendor && is_bf4_product(root.product());

    chassis::Config {
        network_adapter: network_adapter::Config {
            // Dell exploration needs NDF data for host-DPU pairing. BF4 needs
            // NDF0 `PermanentMACAddress` to derive PF0 base MAC as (NDF0 - 0x10)
            // while some BMC firmware does not expose ComputerSystem BaseMAC.
            need_network_device_fns: root.vendor() == Some(Vendor::new("Dell"))
                || need_bf4_network_device_fns,
        },
        need_assembly_sn: |id| {
            // For GB200 and Vera Rubin hosts, use the Chassis_0 assembly serial
            // number to match Nautobot / expected-machine inventory serials.
            (*id.inner() == "Chassis_0").then_some(|model| {
                model.is_some_and(|model| {
                    hw::vera_rubin::chassis_assembly_serial_model(model.into_inner())
                }) || model == Some(AssemblyModel::new("GB200 NVL"))
            })
        },
        // BlueField-3 DPU (Tested on BF-25.10-9 firmware) has issue
        // with ERoT chassis. It stucks sometimes until next request
        // of BlueField_ERoT. Because carbide doesn't need
        // BlueField_ERoT we just skip it.
        lazy_fetch: (root.vendor() == Some(Vendor::new("Nvidia"))
            && root.product() == Some(Product::new("BlueField-3 DPU")))
        .then_some(|odata_id| odata_id.last_segment() != Some("Bluefield_ERoT")),
    }
}

pub async fn nv_generate_exploration_report<B: Bmc>(
    mut root: Arc<ServiceRoot<B>>,
    config: &Config<'_, B>,
) -> Result<EndpointExplorationReport, Error<B>> {
    let chassis_explore_config = build_chassis_explore_config(&root);
    let mut explored_chassis =
        ExploredChassisCollection::explore(&root, &chassis_explore_config).await?;
    let explored_inventories = ExploredInventories::explore(&root).await?;

    // Delta power shelves do not expose a `/redfish/v1/Systems` collection (and
    // report no vendor in the service root, so nv-redfish fabricates the path
    // and gets a 404). Detect them from the chassis and synthesize the report
    // from chassis + manager data instead of fetching a ComputerSystem.
    if explored_chassis.is_delta_powershelf() {
        return build_delta_powershelf_report(&root, explored_chassis, explored_inventories).await;
    }

    if explored_chassis.is_bluefield2() {
        root = root.as_ref().clone().restrict_expand().into();
    }

    let mut systems_iter = root
        .systems()
        .await
        .map_err(Error::nv_redfish("systems"))?
        .ok_or_else(Error::bmc_not_provided("systems"))?
        .members()
        .await
        .map_err(Error::nv_redfish("systems members"))?
        .into_iter();

    let first_system = systems_iter
        .next()
        .ok_or_else(Error::bmc_not_provided("at least one computer system"))?;
    let other_system_with_bios = systems_iter.find(|system| system.raw().bios.is_some());
    let system = other_system_with_bios.unwrap_or(first_system);

    let mut managers = root
        .managers()
        .await
        .map_err(Error::nv_redfish("managers"))?
        .ok_or_else(Error::bmc_not_provided("managers"))?
        .members()
        .await
        .map_err(Error::nv_redfish("managers members"))?;

    // Prefer the manager that controls the chosen system —  Fall back to
    // the first manager for BMCs.

    let manager = match managers.iter().position(|m| {
        m.raw()
            .links
            .as_ref()
            .and_then(|l| l.manager_for_servers.as_ref())
            .is_some_and(|servers| servers.iter().any(|s| s.id() == system.odata_id()))
    }) {
        Some(i) => managers.swap_remove(i),
        None => managers
            .into_iter()
            .next()
            .ok_or_else(Error::bmc_not_provided("at least one manager"))?,
    };

    let is_bluefield_system = is_bluefield_system_id(system.id());
    let system_explore_config = computer_system::Config {
        need_oem_nvidia_bluefield: is_bluefield_system,
        ignore_500_on_bios_fetch: is_bluefield_system,
        retry_404_on_eth_interfaces: is_bluefield_system,
        explore: config,
    };
    let explored_system = ExploredComputerSystem::explore(system, &system_explore_config).await?;

    let hw_type = hw_type(&root, &explored_system, &explored_chassis);
    let linked_chassis_ids = explored_system.linked_chassis_ids();
    let has_system_mac_address = explored_system.has_usable_ethernet_mac_address();
    if should_use_network_adapter_port_fallback(
        hw_type,
        has_system_mac_address,
        &linked_chassis_ids,
    ) || should_fetch_supplemental_network_adapter_ports(
        hw_type,
        has_system_mac_address,
        &linked_chassis_ids,
    ) {
        explored_chassis
            .fetch_network_adapter_ports(&linked_chassis_ids)
            .await;
    }
    let is_mgx_c2 = explored_chassis.is_mgx_c2();
    let manager_explore_config = hw_type
        .map(|hw_type| match hw_type {
            hw::HwType::Ami => manager::Config {
                need_host_interfaces: true,
                ..Default::default()
            },
            hw::HwType::Dell => manager::Config {
                need_oem_dell_attributes: true,
                ..Default::default()
            },
            hw::HwType::Lenovo => manager::Config {
                need_oem_lenovo_security_service: true,
                ..Default::default()
            },
            hw::HwType::LenovoAmi => manager::Config {
                need_oem_ami_config_bmc: true,
                ..Default::default()
            },
            hw::HwType::LenovoGb300 => manager::Config {
                need_host_interfaces: true,
                ..Default::default()
            },
            hw::HwType::Supermicro => manager::Config {
                need_host_interfaces: true,
                need_oem_supermicro_kcs_interface: !is_mgx_c2,
                need_oem_supermicro_sys_lockdown: true,
                ..Default::default()
            },
            _ => manager::Config::default(),
        })
        .unwrap_or_default();

    let explored_manager = ExploredManager::explore(manager, &manager_explore_config).await?;

    let pcie_devices = explored_chassis
        .pcie_devices(|chassis| match hw_type {
            Some(hw::HwType::Viking) => {
                let chassis_id = chassis.chassis.id().into_inner();
                chassis_id.starts_with("HGX_GPU_SXM") || chassis_id.starts_with("HGX_NVSwitch")
            }
            // When needed Chassis Id is equal to System Id.
            Some(
                hw::HwType::Ami
                | hw::HwType::Dell
                | hw::HwType::Hpe
                | hw::HwType::Lenovo
                | hw::HwType::Supermicro,
            ) => chassis.chassis.id().into_inner() == explored_system.system.id().into_inner(),
            // Provides only one Chassis.
            Some(hw::HwType::LenovoAmi) => true,
            Some(
                hw::HwType::LenovoGb300
                | hw::HwType::DgxGb300
                | hw::HwType::SupermicroGb300
                | hw::HwType::VeraRubin,
            ) => chassis.chassis.id().into_inner().starts_with("HGX_GPU_"),
            // No meaningful PCIeDevices.
            Some(
                hw::HwType::Bluefield
                | hw::HwType::Gb200
                | hw::HwType::LiteonPowerShelf
                | hw::HwType::DeltaPowerShelf
                | hw::HwType::NvSwitch,
            ) => false,
            None => false,
        })
        .await?;

    let lockdown_status = hw_type
        .map(|hw_type| {
            lockdown_status(
                &hw_type,
                &explored_system,
                &explored_manager,
                &explored_chassis,
            )
        })
        .transpose()?
        .and_then(identity);

    let secure_boot_status = explored_system
        .secure_boot_status()
        .inspect_err(|error| tracing::warn!(%error, "Failed to fetch forge secure boot status."))
        .ok();

    let machine_setup_status = hw_type
        .map(|hw_type| {
            machine_setup_status(
                &hw_type,
                &explored_manager,
                &explored_chassis,
                &explored_system,
                &lockdown_status,
                config.boot_interface_mac,
            )
        })
        .unwrap_or_else(|| MachineSetupStatus {
            is_done: false,
            diffs: vec![MachineSetupDiff {
                key: "platform type".into(),
                expected: "can detect".into(),
                actual: "cannot detect".into(),
            }],
            evaluated_boot_interface: None,
        });

    let system = explored_system.to_model(hw_type, &explored_chassis, &pcie_devices)?;
    let manager = explored_manager.to_model()?;
    let service = explored_inventories.to_model(hw_type);

    Ok(EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        last_exploration_error: None,
        last_exploration_latency: None,
        machine_id: None,
        managers: vec![manager],
        systems: vec![system],
        chassis: explored_chassis.to_model(),
        service,
        vendor: hw_type.and_then(|hw_type| hw_type.bmc_vendor()),
        versions: HashMap::default(),
        model: None,
        power_shelf_id: None,
        switch_id: None,
        machine_setup_status: Some(machine_setup_status),
        secure_boot_status,
        lockdown_status,
        physical_slot_number: None,
        compute_tray_index: None,
        topology_id: None,
        revision_id: None,
        remediation_error: None,
    })
}

/// `should_use_network_adapter_port_fallback` limits supplemental host MAC
/// discovery to platforms where we have verified the Redfish relationship.
///
/// Lenovo XCC can omit usable `EthernetInterfaces` while exposing host MAC
/// addresses through adapter `Ports` on the linked chassis. Keep this policy
/// narrow: a chassis `Port` is not necessarily a host or PXE interface.
fn should_use_network_adapter_port_fallback(
    hw_type: Option<hw::HwType>,
    has_system_mac_address: bool,
    linked_chassis_ids: &[nv_redfish::core::ODataId],
) -> bool {
    hw_type == Some(hw::HwType::Lenovo) && !has_system_mac_address && !linked_chassis_ids.is_empty()
}

/// Whether linked adapter Ports can supplement a Lenovo XCC's System inventory.
fn should_fetch_supplemental_network_adapter_ports(
    hw_type: Option<hw::HwType>,
    has_system_mac_address: bool,
    linked_chassis_ids: &[nv_redfish::core::ODataId],
) -> bool {
    hw_type == Some(hw::HwType::Lenovo) && has_system_mac_address && !linked_chassis_ids.is_empty()
}

/// Builds an exploration report for a Delta power shelf.
///
/// Delta BMCs do not serve `/redfish/v1/Systems`, so the standard flow (which
/// unconditionally fetches a `ComputerSystem`) fails with a 404. Here we skip
/// that fetch and synthesize a `ComputerSystem` from the chassis, matching the
/// behavior of the libredfish Delta power-shelf path.
async fn build_delta_powershelf_report<B: Bmc>(
    root: &ServiceRoot<B>,
    explored_chassis: ExploredChassisCollection<B>,
    explored_inventories: ExploredInventories<B>,
) -> Result<EndpointExplorationReport, Error<B>> {
    let hw_type = hw::HwType::DeltaPowerShelf;

    let manager = root
        .managers()
        .await
        .map_err(Error::nv_redfish("managers"))?
        .ok_or_else(Error::bmc_not_provided("managers"))?
        .members()
        .await
        .map_err(Error::nv_redfish("managers members"))?
        .into_iter()
        .next()
        .ok_or_else(Error::bmc_not_provided("at least one manager"))?;
    let explored_manager = ExploredManager::explore(manager, &manager::Config::default()).await?;

    let system = explored_chassis.synthesized_powershelf_system();

    Ok(EndpointExplorationReport {
        endpoint_type: EndpointType::Bmc,
        last_exploration_error: None,
        last_exploration_latency: None,
        machine_id: None,
        managers: vec![explored_manager.to_model()?],
        systems: vec![system],
        chassis: explored_chassis.to_model(),
        service: explored_inventories.to_model(Some(hw_type)),
        vendor: hw_type.bmc_vendor(),
        versions: HashMap::default(),
        model: None,
        power_shelf_id: None,
        switch_id: None,
        machine_setup_status: Some(MachineSetupStatus {
            is_done: true,
            diffs: vec![],
            evaluated_boot_interface: None,
        }),
        secure_boot_status: None,
        lockdown_status: None,
        physical_slot_number: None,
        compute_tray_index: None,
        topology_id: None,
        revision_id: None,
        remediation_error: None,
    })
}

pub(crate) fn hw_type<B: Bmc>(
    root: &nv_redfish::ServiceRoot<B>,
    explored_system: &ExploredComputerSystem<B>,
    explored_chassis: &ExploredChassisCollection<B>,
) -> Option<hw::HwType> {
    let system = &explored_system.system;
    let oem_id = root.oem_id().map(|v| v.into_inner());

    // GB300 is an NVIDIA HGX platform identity, recognized by the NVIDIA "NVIDIA GB300"
    // GPU chassis (`is_gb300()`) independent of the host BMC vendor. Resolve it before the
    // host-vendor match below so platform classification is not gated on the host ODM; the
    // ODM only selects the ODM-specific variant.
    if explored_chassis.is_gb300() {
        // Lenovo GB300: AMI host BMC + Lenovo host chassis.
        if explored_chassis.is_lenovo() {
            return Some(hw::HwType::LenovoGb300);
        }
        // DGX GB300: NVIDIA "GB BMC" host (same BMC family as GB200). Resolved here, ahead of
        // the GB200 arm below, since it shares GB200's ServiceRoot signature -- the GB300 GPU
        // chassis (`is_gb300()`) is what distinguishes it from a real GB200.
        if root.vendor() == Some(Vendor::new("NVIDIA"))
            && root.product() == Some(Product::new("GB BMC"))
        {
            return Some(hw::HwType::DgxGb300);
        }
        // SMC GB300: Supermicro OpenBMC host.
        if root.vendor() == Some(Vendor::new("Supermicro")) {
            return Some(hw::HwType::SupermicroGb300);
        }
    }

    root.vendor()
        .map(|v| v.into_inner())
        .or_else(|| (oem_id == Some("Supermicro")).then_some("Supermicro"))
        .and_then(|vendor_id| match vendor_id {
            "AMI" if system.id().into_inner() == "DGX" => Some(hw::HwType::Viking),
            "AMI" => Some(hw::HwType::Ami),
            "Dell" => Some(hw::HwType::Dell),
            "Lenovo" if oem_id == Some("Ami") => Some(hw::HwType::LenovoAmi),
            "Lenovo" if oem_id != Some("Ami") => Some(hw::HwType::Lenovo),
            "Supermicro" => Some(hw::HwType::Supermicro),
            "HPE" => Some(hw::HwType::Hpe),
            "Nvidia" if is_bluefield_system_id(system.id()) => Some(hw::HwType::Bluefield),
            "NVIDIA" if root.product() == Some(Product::new("VR NVL72")) => {
                Some(hw::HwType::VeraRubin)
            }
            "WIWYNN" | "NVIDIA"
                if root.product() == Some(Product::new("GB200 NVL"))
                    || root.product() == Some(Product::new("GB BMC")) =>
            {
                Some(hw::HwType::Gb200)
            }
            "NVIDIA" if root.product() == Some(Product::new("P3809")) => Some(hw::HwType::NvSwitch),
            _ => None,
        })
        .or_else(|| {
            explored_chassis
                .is_liteon_powershelf()
                .then_some(hw::HwType::LiteonPowerShelf)
        })
        .or_else(|| {
            explored_chassis
                .is_delta_powershelf()
                .then_some(hw::HwType::DeltaPowerShelf)
        })
}

fn lockdown_status<B: Bmc>(
    hw_type: &hw::HwType,
    explored_system: &ExploredComputerSystem<B>,
    explored_manager: &ExploredManager<B>,
    explored_chassis: &ExploredChassisCollection<B>,
) -> Result<Option<LockdownStatus>, Error<B>> {
    let bios = &explored_system.bios;
    let system = &explored_system.system;
    let manager = &explored_manager.manager;

    match hw_type {
        hw::HwType::Viking => {
            let bios = bios.as_ref().ok_or_else(Error::bmc_not_provided("bios"))?;
            let kcs_intreface = bios.attribute("KcsInterfaceDisable");
            let redfish_enable = bios.attribute("RedfishEnable");
            let kcs_intreface = kcs_intreface.as_ref().and_then(|attr| attr.str_value());
            let redfish_enable = redfish_enable.as_ref().and_then(|attr| attr.str_value());
            let message = [
                ("ipmi_kcs_disable", &kcs_intreface),
                ("redfish_enable", &redfish_enable),
            ]
            .into_iter()
            .filter_map(|(k, v)| v.map(|v| format!("{k}={v}")))
            .join(", ")
                + ".";
            let status = match (kcs_intreface, redfish_enable) {
                (None, None) => InternalLockdownStatus::Disabled,
                (Some("Deny All"), Some(_)) => InternalLockdownStatus::Enabled,
                (Some("Allow All"), Some("Enabled")) => InternalLockdownStatus::Disabled,
                (_, _) => InternalLockdownStatus::Partial,
            };
            Ok(Some(LockdownStatus { status, message }))
        }
        hw::HwType::Ami => {
            let bios = bios.as_ref().ok_or_else(Error::bmc_not_provided("bios"))?;
            let kcsacp = bios.attribute("KCSACP");
            let usb000 = bios.attribute("USB000");
            let hi_enabled = explored_manager
                .host_interfaces
                .as_ref()
                .ok_or_else(Error::bmc_not_provided("host interfaces"))?
                .iter()
                .any(|i| i.interface_enabled().is_none_or(identity));
            let kcsacp = kcsacp.as_ref().and_then(|v| v.str_value());
            let usb000 = usb000.as_ref().and_then(|v| v.str_value());
            let message =
                format!("kcsacp: {kcsacp:?}; usb000: {usb000:?}; host_interfaces: {hi_enabled}");
            match (kcsacp, usb000, hi_enabled) {
                (Some("Deny All"), Some("Disabled"), false) => Ok(InternalLockdownStatus::Enabled),
                (Some("Allow All"), Some("Enabled"), true) => Ok(InternalLockdownStatus::Disabled),
                _ => Ok(InternalLockdownStatus::Partial),
            }
            .map(|status| Some(LockdownStatus { status, message }))
        }

        // LenovoGB300 (Grace-based AMI host BMC) has neither the KCS BIOS
        // attribute nor the OEM ConfigBMC endpoint. Lockdown is read from the
        // USB support attribute (attribute-id prefixed enum, e.g.
        // "USB000Disabled") together with the host interface state.
        hw::HwType::LenovoGb300 => {
            let bios = bios.as_ref().ok_or_else(Error::bmc_not_provided("bios"))?;
            let usb000 = bios.attribute("USB000");
            let usb000 = usb000.as_ref().and_then(|v| v.str_value());
            let hi_enabled = explored_manager
                .host_interfaces
                .as_ref()
                .ok_or_else(Error::bmc_not_provided("host interfaces"))?
                .iter()
                .any(|i| i.interface_enabled().is_none_or(identity));
            let message = format!("usb_support={usb000:?}; host_interface={hi_enabled}");
            match (usb000, hi_enabled) {
                (Some("USB000Disabled"), false) => Ok(InternalLockdownStatus::Enabled),
                (Some("USB000Enabled"), true) => Ok(InternalLockdownStatus::Disabled),
                _ => Ok(InternalLockdownStatus::Partial),
            }
            .map(|status| Some(LockdownStatus { status, message }))
        }

        hw::HwType::Dell => {
            let attributes = explored_manager
                .oem_dell_attributes
                .as_ref()
                .ok_or_else(Error::bmc_not_provided("Dell OEM Attributes"))?;
            let system_lockdown = attributes.attribute("Lockdown.1.SystemLockdown");
            let racadm = attributes.attribute("Racadm.1.Enable");
            let system_lockdown = system_lockdown
                .as_ref()
                .and_then(|v| v.str_value())
                .ok_or_else(Error::bmc_not_provided(
                    "Dell OEM Attributes: SystemLockdown",
                ))?;
            let racadm = racadm
                .as_ref()
                .and_then(|v| v.str_value())
                .ok_or_else(Error::bmc_not_provided("Dell OEM Attributes: Racadm"))?;
            let message = format!("BMC: system_lockdown={system_lockdown}, racadm={racadm}.");
            match (system_lockdown, racadm) {
                ("Enabled", "Disabled") => Ok(InternalLockdownStatus::Enabled),
                ("Disabled", "Enabled") => Ok(InternalLockdownStatus::Disabled),
                (_, _) => Ok(InternalLockdownStatus::Partial),
            }
            .map(|status| Some(LockdownStatus { status, message }))
        }

        hw::HwType::LenovoAmi => {
            let config_bmc = explored_manager
                .oem_ami_config_bmc
                .as_ref()
                .ok_or_else(Error::bmc_not_provided("AMI Manager ConfigBMC"))?;
            let attrs = config_bmc.raw();
            let descr = [
                (
                    "LockoutHostControl",
                    attrs
                        .lockout_host_control
                        .map(|v| v == LockoutHostControlState::Enable),
                ),
                (
                    "LockoutBiosVariableWriteMode",
                    attrs
                        .lockout_bios_variable_write_mode
                        .map(|v| v == LockoutBiosVariableWriteMode::Enable),
                ),
                (
                    "LockdownBiosSettingsChange",
                    attrs
                        .lockdown_bios_settings_change
                        .map(|v| v == LockdownBiosSettingsChangeState::Enable),
                ),
                (
                    "LockdownBiosUpgradeDowngrade",
                    attrs
                        .lockdown_bios_upgrade_downgrade
                        .map(|v| v == LockdownBiosUpgradeDowngradeState::Enable),
                ),
            ];
            let all_enabled = descr.iter().filter_map(|v| v.1).all(identity);
            let all_disabled = descr.iter().filter_map(|v| v.1).all(|v| !v);
            let status = if all_enabled {
                InternalLockdownStatus::Enabled
            } else if all_disabled {
                InternalLockdownStatus::Disabled
            } else {
                InternalLockdownStatus::Partial
            };
            let message = descr
                .iter()
                .filter_map(|(name, enabled)| {
                    enabled.map(|enabled| {
                        format!("{name}={}", if enabled { "Enable" } else { "Disable" })
                    })
                })
                .join(", ");

            Ok(Some(LockdownStatus { status, message }))
        }

        hw::HwType::Lenovo => {
            let oem_lenovo_manager = manager
                .oem_lenovo()
                .map_err(Error::nv_redfish("Lenovo manager OEM"))?
                .ok_or_else(Error::bmc_not_provided("Lenovo manager OEM"))?;
            let kcs_enabled = oem_lenovo_manager
                .kcs_enabled()
                .ok_or(Error::BmcNotProvided("Lenovo manager: KCS state"))?;
            let firmware_rollback = explored_manager
                .oem_lenovo_security_service
                .as_ref()
                .ok_or_else(Error::bmc_not_provided("Lenovo security service"))?
                .fw_rollback()
                .ok_or(Error::BmcNotProvided(
                    "Lenovo security service: firmware rollback status",
                ))?;
            let eth_usb = explored_manager
                .eth_interfaces
                .iter()
                .find(|iface| *iface.id().inner() == "ToHost")
                .and_then(|iface| iface.interface_enabled())
                .ok_or(Error::BmcNotProvided(
                    "Lenovo manager ethernet interfaces: enabled property",
                ))?;

            let oem_lenovo_system = system
                .oem_lenovo()
                .map_err(Error::nv_redfish("Lenovo computer system"))?
                .ok_or_else(Error::bmc_not_provided("Lenovo computer system"))?;

            let fp_mode = oem_lenovo_system
                .front_panel_mode()
                .ok_or(Error::BmcNotProvided(
                    "Lenovo computer system: front panel mode",
                ))?;
            let port_switching_to =
                oem_lenovo_system
                    .port_switching_to()
                    .ok_or(Error::BmcNotProvided(
                        "Lenovo computer system: port switching to",
                    ))?;

            let message = format!(
                "kcs={}, firmware_rollback={firmware_rollback:?}, ethernet_over_usb={eth_usb:?}, front_panel_usb={fp_mode:?}/{port_switching_to:?}",
                kcs_enabled == KcsState::Enabled
            );

            match (
                kcs_enabled,
                firmware_rollback,
                eth_usb,
                fp_mode,
                port_switching_to,
            ) {
                (KcsState::Disabled, FwRollbackState::Disabled, false, FpMode::Server, _) => {
                    Ok(InternalLockdownStatus::Enabled)
                }
                (
                    KcsState::Enabled,
                    FwRollbackState::Enabled,
                    true,
                    FpMode::Shared,
                    PortSwitchingTo::Server,
                ) => Ok(InternalLockdownStatus::Disabled),
                (_, _, _, _, _) => Ok(InternalLockdownStatus::Partial),
            }
            .map(|status| Some(LockdownStatus { status, message }))
        }

        hw::HwType::Supermicro => {
            let hi_enabled = explored_manager
                .host_interfaces
                .as_ref()
                .ok_or_else(Error::bmc_not_provided("host interfaces"))?
                .iter()
                .any(|i| i.interface_enabled().is_none_or(identity));
            let kcs_privilege = explored_manager
                .oem_supermicro_kcs_interface
                .as_ref()
                .and_then(|iface| iface.privilege());
            let is_syslockdown = explored_manager
                .oem_supermicro_sys_lockdown
                .as_ref()
                .and_then(|lck| lck.sys_lockdown_enabled())
                .ok_or_else(Error::bmc_not_provided("Supermicro lockdown status"))?;
            let ipmi_host_interface_enabled = system
                .raw()
                .ipmi_host_interface
                .as_ref()
                .and_then(|interface| interface.service_enabled);
            let message = format!(
                "SysLockdownEnabled={is_syslockdown}, kcs_privilege={kcs_privilege:#?}, \
                 host_interface_enabled={hi_enabled}, \
                 ipmi_host_interface_enabled={ipmi_host_interface_enabled:?}"
            );

            let model = system.hardware_id().model.map(|v| v.into_inner());
            let is_ars_121l_dnr = model == Some("ARS-121L-DNR");
            let (inband_locked, inband_unlocked) = if explored_chassis.is_mgx_c2() {
                (
                    ipmi_host_interface_enabled.is_none_or(|enabled| !enabled),
                    ipmi_host_interface_enabled.is_none_or(identity),
                )
            } else {
                (
                    kcs_privilege == Some(SupermicroPrivilege::Callback),
                    kcs_privilege.is_none()
                        || kcs_privilege == Some(SupermicroPrivilege::Administrator),
                )
            };
            // ARS-121L-DNR must keep HostInterface enabled to PXE boot.
            let host_interface_locked = hi_enabled == is_ars_121l_dnr;

            let status = if is_syslockdown && inband_locked && host_interface_locked {
                InternalLockdownStatus::Enabled
            } else if !is_syslockdown && inband_unlocked && hi_enabled {
                InternalLockdownStatus::Disabled
            } else {
                InternalLockdownStatus::Partial
            };
            Ok(Some(LockdownStatus { status, message }))
        }

        hw::HwType::Hpe => {
            let bios = bios.as_ref().ok_or_else(Error::bmc_not_provided("bios"))?;
            let usb_boot = bios.attribute("UsbBoot");
            let usb_boot = usb_boot.as_ref().and_then(|v| v.str_value());
            let virtual_nic_enabled = manager
                .oem_hpe()
                .map_err(Error::nv_redfish("HPE manager OEM"))?
                .and_then(|oem| oem.virtual_nic_enabled())
                .ok_or_else(Error::bmc_not_provided("HPE manager virtual NIC state"))?;
            let message = format!(
                "usb_boot={}, virtual_nic_enabled={}",
                usb_boot.unwrap_or("Unknown"),
                virtual_nic_enabled
            );
            // TODO: (not nv-redfish todo): kcs_enabled not implemented...
            let status = match (usb_boot, virtual_nic_enabled) {
                (Some("Disabled"), false) => InternalLockdownStatus::Enabled,
                (Some("Enabled"), true) => InternalLockdownStatus::Disabled,
                (_, _) => InternalLockdownStatus::Partial,
            };
            Ok(Some(LockdownStatus { message, status }))
        }

        _ => Ok(None),
    }
}

fn machine_setup_status<B: Bmc>(
    hw_type: &hw::HwType,
    explored_manager: &ExploredManager<B>,
    chassis: &ExploredChassisCollection<B>,
    explored_system: &ExploredComputerSystem<B>,
    lockdown_status: &Option<LockdownStatus>,
    boot_interface_mac: Option<MacAddress>,
) -> MachineSetupStatus {
    let mut diffs = Vec::new();

    if let Some(lockdown_status) = lockdown_status
        && lockdown_status.status != InternalLockdownStatus::Enabled
    {
        diffs.push(MachineSetupDiff {
            key: "lockdown".to_string(),
            expected: "Enabled".to_string(),
            actual: format!("{:?}", lockdown_status.status),
        });
    }
    match hw_type {
        hw::HwType::LiteonPowerShelf => (),
        hw::HwType::DeltaPowerShelf => (),
        hw::HwType::NvSwitch => (),
        hw::HwType::Viking => {
            diffs.extend(
                hw::viking::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );
            if let Some(mac) = boot_interface_mac
                && let Some(diff) = explored_system.check_boot_by_uefi_prefix(mac)
            {
                diffs.push(diff)
            }
        }

        hw::HwType::Ami | hw::HwType::LenovoAmi => {
            diffs.extend(
                hw::lenovo_ami::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );
            if let Some(mac) = boot_interface_mac
                && let Some(diff) = explored_system.check_boot_by_uefi_prefix(mac)
            {
                diffs.push(diff)
            }
        }

        hw::HwType::Hpe => {
            diffs.extend(
                hw::hpe::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );
            if let Some(mac) = boot_interface_mac
                && let Some(diff) = explored_system.check_boot_by_uefi_prefix(mac)
            {
                diffs.push(diff)
            }
        }

        hw::HwType::Bluefield => {
            // Check BIOS configuration:
            diffs.extend(
                hw::bluefield::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );
        }

        hw::HwType::Dell => {
            // Bios attributes:
            diffs.extend(
                hw::dell::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );

            // Dell BMC Attrbiutes:
            if let Some(oem_dell_attributes) = &explored_manager.oem_dell_attributes {
                // Manager attributes:
                for (key, expected) in [
                    ("WebServer.1.HostHeaderCheck", "Disabled"),
                    ("IPMILan.1.Enable", "Enabled"),
                    ("OS-BMC.1.AdminState", "Disabled"),
                ] {
                    if let Some(actual) = oem_dell_attributes.attribute(key)
                        && actual.str_value() != Some(expected)
                    {
                        diffs.push(MachineSetupDiff {
                            key: key.to_string(),
                            expected: expected.to_string(),
                            actual: actual.str_value().unwrap_or("unexpected type").to_string(),
                        })
                    }
                }
            }

            // Boot order:
            if let Some(mac) = boot_interface_mac {
                // 1. Find network adapter that have specified MAC address inside
                //    it's functions.
                // 2. Make sure that adapter id is set to HttpDev1Interface bios attribute.
                // 3. Make sure that it is referenced via related_item by first boot option
                //    in boot order.
                let expected = if let Some((_adapter, function)) = chassis
                    .members
                    .iter()
                    .find_map(|c| c.network_adapters.find_by_mac(mac))
                {
                    if let Some(actual) = explored_system
                        .bios
                        .as_ref()
                        .and_then(|bios| bios.attribute("HttpDev1Interface"))
                        && actual.str_value() != Some(function.id().into_inner())
                    {
                        diffs.push(MachineSetupDiff {
                            key: "HttpDev1Interface".to_string(),
                            expected: function.id().into_inner().to_string(),
                            actual: actual.str_value().unwrap_or("unexpected type").to_string(),
                        })
                    }
                    explored_system.boot_options.iter().find(|option| {
                        option
                            .raw()
                            .related_item
                            .iter()
                            .flatten()
                            .any(|v| &v.odata_id == function.odata_id())
                    })
                } else {
                    None
                };

                let actual = explored_system.boot_order_first_option();
                if let Some(diff) = compare_boot_options(expected, actual) {
                    diffs.push(diff);
                }
            }
        }

        hw::HwType::Lenovo => {
            // Check BIOS configuration:
            diffs.extend(
                hw::lenovo::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );

            // Boot order:
            let expected_name = ResourceNameRef::new("Network");
            if let Some(actual_opt) = explored_system.boot_order_first_option()
                && actual_opt.name() != expected_name
            {
                diffs.push(MachineSetupDiff {
                    key: "boot_first_type".to_string(),
                    expected: expected_name.to_string(),
                    actual: actual_opt.name().to_string(),
                });
            }
        }

        hw::HwType::LenovoGb300 => {
            // Check BIOS configuration:
            diffs.extend(
                hw::lenovo_gb300::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );
            if let Some(mac) = boot_interface_mac {
                if let Some(diff) = explored_system.check_boot_by_uefi_prefix(mac) {
                    diffs.push(diff);
                }
                if let Some(diff) = explored_system.check_boot_option_enabled_by_uefi_prefix(mac) {
                    diffs.push(diff);
                }
            }
        }

        hw::HwType::Supermicro => {
            // BIOS.
            let bios_raw = explored_system.bios.as_ref().map(|bios| bios.raw());
            let attrs_json = bios_raw
                .as_ref()
                .and_then(|raw| raw.attributes.as_ref())
                .map(|attributes| &attributes.dynamic_properties);
            // Transform all BIOS keys by pattern:
            // {"X_a": 1, "X_b": 2, "Y_c": 3, "Z_z_d": 4}
            // => {"X": ["X_a", "X_b"], "Y": ["Y_c"], "Z_z": ["Z_z_d"]}
            // It is needed to handle suffixes of Supermicro:
            // Attribute names examples:
            //  "IPv4HTTPSupport_009F", "DeviceSelect_0034", "DeviceSelect_003D", "SR_IOVSupport_002B"
            let actual_attrs_keys = attrs_json
                .iter()
                .flat_map(|m| {
                    m.keys().map(|k| {
                        if let Some((prefix, _)) = k.rsplit_once("_") {
                            (prefix, k.as_str())
                        } else {
                            (k.as_str(), k.as_str())
                        }
                    })
                })
                .fold(HashMap::<_, Vec<_>>::new(), |mut acc, (k, v)| {
                    acc.entry(k).or_default().push(v);
                    acc
                });

            // Go through prefixes to veryfy and check that each
            // attribute with the prefix has expected value.
            diffs.extend(
                hw::supermicro::EXPECTED_BIOS_ATTRS_PREFIXES
                    .iter()
                    .flat_map(|expected| {
                        actual_attrs_keys
                            .get(expected.key)
                            .into_iter()
                            .flat_map(|actual_keys| {
                                actual_keys.iter().filter_map(|actual_key| {
                                    explored_system.verify_bios_attr(&hw::BiosAttr {
                                        key: actual_key,
                                        value: expected.value,
                                    })
                                })
                            })
                    }),
            );

            // Boot order:
            if let Some(mac) = boot_interface_mac {
                let mac_str = mac.to_string();
                const MELLANOX_UEFI_HTTP_IPV4: &str = "UEFI HTTP IPv4 Mellanox Network Adapter";
                const NVIDIA_UEFI_HTTP_IPV4: &str = "UEFI HTTP IPv4 Nvidia Network Adapter";
                let expected = explored_system.boot_options.iter().find(|boot_opt| {
                    boot_opt.display_name().is_some_and(|v| {
                        (v.inner().contains(MELLANOX_UEFI_HTTP_IPV4)
                            || v.inner().contains(NVIDIA_UEFI_HTTP_IPV4))
                            && v.inner().contains(&mac_str)
                    })
                });
                let actual = explored_system.boot_order_first_option();
                if let Some(diff) = compare_boot_options(expected, actual) {
                    diffs.push(diff);
                }
            }
        }

        hw::HwType::Gb200 => {
            if explored_system
                .secure_boot_status()
                .is_ok_and(|s| s.is_enabled)
            {
                diffs.push(MachineSetupDiff {
                    key: "SecureBoot".to_string(),
                    expected: "false".to_string(),
                    actual: "true".to_string(),
                })
            }
            // BIOS configuration:
            diffs.extend(
                hw::gb200::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );
            // Boot order
            if let Some(mac) = boot_interface_mac {
                // Looking for UEFI Device path:
                // VenHw(REDACTED)/MemoryMapped(REDACTED)/PciRoot(0x6)/Pci(0x0,0x0)/Pci(0x0,0x0)/Pci(0x0,0x0)/Pci(0x0,0x0)/MAC(020304050607,0x1)/IPv4(0.0.0.0)/Uri()
                let actual = explored_system.boot_order_first_option();
                let mac_str = format!("/MAC({},", mac.to_string().replace(":", ""));
                let expected = explored_system.boot_options.iter().find(|option| {
                    option.uefi_device_path().is_some_and(|path| {
                        path.inner().contains(&mac_str)
                            && path.inner().contains("/IPv4(")
                            && path.inner().ends_with("/Uri()")
                    })
                });
                if let Some(diff) = compare_boot_options(expected, actual) {
                    diffs.push(diff)
                }
            }
        }

        hw::HwType::VeraRubin => {
            if explored_system
                .secure_boot_status()
                .is_ok_and(|s| s.is_enabled)
            {
                diffs.push(MachineSetupDiff {
                    key: "SecureBoot".to_string(),
                    expected: "false".to_string(),
                    actual: "true".to_string(),
                })
            }
            diffs.extend(
                hw::vera_rubin::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );
            if let Some(mac) = boot_interface_mac {
                // Looking for UEFI Device path:
                // VenHw(...)/.../MAC(020304050607,0x1)/IPv4(0.0.0.0)/Uri()
                let actual = explored_system.boot_order_first_option();
                let mac_str = format!("/MAC({},", mac.to_string().replace(":", ""));
                let expected = explored_system.boot_options.iter().find(|option| {
                    option.uefi_device_path().is_some_and(|path| {
                        path.inner().contains(&mac_str)
                            && path.inner().contains("/IPv4(")
                            && path.inner().ends_with("/Uri()")
                    })
                });
                if let Some(diff) = compare_boot_options(expected, actual) {
                    diffs.push(diff)
                }
            }
        }

        hw::HwType::DgxGb300 => {
            // DGX GB300 on the NVIDIA "GB BMC" uses the platform-level setup expectations:
            // secure boot off and boot order by MAC.
            // TODO(dgx-gb300): add EXPECTED_BIOS_ATTRS once the tray BIOS is characterized.
            if explored_system
                .secure_boot_status()
                .is_ok_and(|s| s.is_enabled)
            {
                diffs.push(MachineSetupDiff {
                    key: "SecureBoot".to_string(),
                    expected: "false".to_string(),
                    actual: "true".to_string(),
                })
            }
            if let Some(mac) = boot_interface_mac {
                let actual = explored_system.boot_order_first_option();
                let mac_str = format!("/MAC({},", mac.to_string().replace(":", ""));
                let expected = explored_system.boot_options.iter().find(|option| {
                    option.uefi_device_path().is_some_and(|path| {
                        path.inner().contains(&mac_str)
                            && path.inner().contains("/IPv4(")
                            && path.inner().ends_with("/Uri()")
                    })
                });
                if let Some(diff) = compare_boot_options(expected, actual) {
                    diffs.push(diff)
                }
            }
        }

        hw::HwType::SupermicroGb300 => {
            // Supermicro GB300 uses the GBx00 OpenBMC flow, but its firmware does not expose
            // SecureBootEnable or EmbeddedUefiShell. Verify only the controls present in the
            // real tray: TPM support and the DPU-facing PCIe option ROMs.
            diffs.extend(
                hw::supermicro_gb300::EXPECTED_BIOS_ATTRS
                    .iter()
                    .flat_map(|expected| explored_system.verify_bios_attr(expected)),
            );
            if let Some(mac) = boot_interface_mac {
                let actual = explored_system.boot_order_first_option();
                let mac_str = format!("/MAC({},", mac.to_string().replace(":", ""));
                let expected = explored_system.boot_options.iter().find(|option| {
                    option.uefi_device_path().is_some_and(|path| {
                        path.inner().contains(&mac_str)
                            && path.inner().contains("/IPv4(")
                            && path.inner().ends_with("/Uri()")
                    })
                });
                if let Some(diff) = compare_boot_options(expected, actual) {
                    diffs.push(diff)
                }
            }
        }
    }

    MachineSetupStatus {
        is_done: diffs.is_empty(),
        diffs,
        evaluated_boot_interface: None,
    }
}

fn compare_boot_options<B: Bmc>(
    expected: Option<&BootOption<B>>,
    actual: Option<&BootOption<B>>,
) -> Option<MachineSetupDiff> {
    if expected.is_none() || actual.map(|v| v.id()) != expected.map(|v| v.id()) {
        Some(MachineSetupDiff {
            key: "boot_first".to_string(),
            expected: expected
                .map(|v| {
                    v.display_name()
                        .map(|v| v.into_inner())
                        .unwrap_or(v.id().into_inner())
                })
                .unwrap_or("Not found")
                .to_string(),
            actual: actual
                .map(|v| {
                    v.display_name()
                        .map(|v| v.into_inner())
                        .unwrap_or(v.id().into_inner())
                })
                .unwrap_or("Not found")
                .to_string(),
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use nv_redfish::core::ODataId;

    use super::hw::HwType;
    use super::{
        Product, is_bf4_product, should_fetch_supplemental_network_adapter_ports,
        should_use_network_adapter_port_fallback,
    };

    #[test]
    fn is_bf4_product_matches_bf4_service_root_products() {
        assert!(is_bf4_product(Some(Product::new("B4240V"))));
        assert!(is_bf4_product(Some(Product::new("BlueField-4"))));
        assert!(!is_bf4_product(Some(Product::new("BlueField-3 DPU"))));
        assert!(!is_bf4_product(None));
    }

    #[test]
    fn lenovo_network_adapter_port_fallback_is_narrow() {
        value_scenarios!(run = |(hw_type, has_system_mac_address, has_linked_chassis)| {
            let linked_chassis_ids = has_linked_chassis
                .then(|| ODataId::from("/redfish/v1/Chassis/Self".to_string()))
                .into_iter()
                .collect::<Vec<_>>();

            should_use_network_adapter_port_fallback(
                hw_type,
                has_system_mac_address,
                &linked_chassis_ids,
            )
        };
            "Lenovo XCC without a System MAC and with a chassis link" {
                (Some(HwType::Lenovo), false, true) => true,
            }
            "non-Lenovo host" {
                (Some(HwType::Ami), false, true) => false,
            }
            "Lenovo AMI host" {
                (Some(HwType::LenovoAmi), false, true) => false,
            }
            "Lenovo XCC with a System MAC" {
                (Some(HwType::Lenovo), true, true) => false,
            }
            "Lenovo XCC without a linked chassis" {
                (Some(HwType::Lenovo), false, false) => false,
            }
        );
    }

    #[test]
    fn lenovo_network_adapter_port_fetch_supplements_partial_inventory() {
        value_scenarios!(run = |(hw_type, has_system_mac_address, has_linked_chassis)| {
            let linked_chassis_ids = has_linked_chassis
                .then(|| ODataId::from("/redfish/v1/Chassis/Self".to_string()))
                .into_iter()
                .collect::<Vec<_>>();

            should_fetch_supplemental_network_adapter_ports(
                hw_type,
                has_system_mac_address,
                &linked_chassis_ids,
            )
        };
            "Lenovo XCC without a System MAC" {
                (Some(HwType::Lenovo), false, true) => false,
            }
            "Lenovo XCC with a System MAC supplements its inventory" {
                (Some(HwType::Lenovo), true, true) => true,
            }
            "non-Lenovo host" {
                (Some(HwType::Ami), true, true) => false,
            }
            "Lenovo AMI host" {
                (Some(HwType::LenovoAmi), true, true) => false,
            }
            "Lenovo XCC without a linked chassis" {
                (Some(HwType::Lenovo), true, false) => false,
            }
        );
    }
}
