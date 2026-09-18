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
use std::fmt;

use carbide_network::BaseMac;
use itertools::Itertools;
use mac_address::MacAddress;
use model::site_explorer::{
    Chassis, ComputerSystem as ModelComputerSystem, PowerState as ModelPowerState,
};
use nv_redfish::assembly::Model as AssemblyModel;
use nv_redfish::chassis::{Chassis as NvChassis, PowerSupply as NvPowerSupply};
use nv_redfish::core::ODataId;
use nv_redfish::hardware_id::{Manufacturer, Model};
use nv_redfish::pcie_device::PcieDevice;
use nv_redfish::resource::ResourceIdRef;
use nv_redfish::{Bmc, Resource, ServiceRoot};

use crate::network_adapter::ExploredNetworkAdapterCollection;
use crate::{Error, network_adapter};

type AssemblyModelFilterFn = fn(Option<AssemblyModel<&str>>) -> bool;
const BF4_NDF0_TO_BASE_MAC_OFFSET: u64 = 0x10;
pub(crate) struct Config {
    pub(crate) network_adapter: network_adapter::Config,
    pub(crate) need_assembly_sn: fn(ResourceIdRef) -> Option<AssemblyModelFilterFn>,
    pub(crate) lazy_fetch: Option<fn(&ODataId) -> bool>,
}

pub(crate) struct ExploredChassisCollection<B: Bmc> {
    pub(super) members: Vec<ExploredChassis<B>>,
}

impl<B: Bmc> ExploredChassisCollection<B> {
    pub(crate) async fn explore(root: &ServiceRoot<B>, config: &Config) -> Result<Self, Error<B>> {
        let mut members = Vec::new();
        for m in Self::fetch_members(root, config).await? {
            members.push(ExploredChassis::explore(m, config).await?);
        }
        Ok(Self { members })
    }

    async fn fetch_members(
        root: &ServiceRoot<B>,
        config: &Config,
    ) -> Result<Vec<NvChassis<B>>, Error<B>> {
        if let Some(filter) = config.lazy_fetch {
            let links = root
                .chassis_links()
                .await
                .map_err(Error::nv_redfish("chassis collection"))?
                .ok_or_else(Error::bmc_not_provided("chassis collection"))?;
            let mut result = Vec::with_capacity(links.len());
            for l in links {
                if filter(l.odata_id()) {
                    result.push(
                        l.upgrade()
                            .await
                            .map_err(Error::nv_redfish("chassis collection member"))?,
                    )
                }
            }
            Ok(result)
        } else {
            root.chassis()
                .await
                .map_err(Error::nv_redfish("chassis collection"))?
                .ok_or_else(Error::bmc_not_provided("chassis collection"))?
                .members()
                .await
                .map_err(Error::nv_redfish("chassis collection members"))
        }
    }

    pub(crate) fn to_model(&self) -> Vec<Chassis> {
        self.members.iter().map(|v| v.to_model()).collect()
    }

    /// `fetch_network_adapter_ports` reads supplemental adapter `Port`
    /// inventory only from chassis explicitly linked to the selected
    /// `ComputerSystem`.
    ///
    /// A BMC can expose its own chassis and other enclosures beside the host.
    /// Following `Links.Chassis` keeps their adapter MACs out of host inventory.
    pub(crate) async fn fetch_network_adapter_ports(&mut self, linked_chassis_ids: &[ODataId]) {
        for chassis in &mut self.members {
            if linked_chassis_ids
                .iter()
                .any(|chassis_id| chassis_id == chassis.chassis.odata_id())
            {
                chassis.network_adapters.fetch_ports().await;
            }
        }
    }

    pub(crate) fn is_liteon_powershelf(&self) -> bool {
        self.members.iter().any(|m| {
            m.chassis.id().into_inner() == "powershelf"
                || (m.chassis.id().into_inner() == "chassis"
                    && m.chassis
                        .hardware_id()
                        .manufacturer
                        .as_ref()
                        .is_some_and(|mfg| mfg.as_ref().to_lowercase().contains("lite-on")))
        })
    }

    pub(crate) fn liteon_power_state(&self) -> Option<LiteOnSuppliesState<'_>> {
        self.members.iter().find_map(|m| {
            m.oem_liteon_power_supplies
                .as_ref()
                .map(|v| LiteOnSuppliesState(v))
        })
    }

    /// Detects a Delta power shelf. Delta BMCs expose neither a `Vendor` in the
    /// service root nor a `/redfish/v1/Systems` collection, so classification
    /// relies on a Delta manufacturer on the power-shelf chassis. The
    /// manufacturer gate is what distinguishes Delta from the Lite-On power
    /// shelf, which can share the generic "powershelf" chassis id.
    pub(crate) fn is_delta_powershelf(&self) -> bool {
        self.delta_powershelf_chassis().is_some()
    }

    fn delta_powershelf_chassis(&self) -> Option<&ExploredChassis<B>> {
        self.members.iter().find(|m| {
            is_delta_powershelf_chassis(
                m.chassis.id().into_inner(),
                m.chassis
                    .hardware_id()
                    .manufacturer
                    .as_ref()
                    .map(|mfg| **mfg),
            )
        })
    }

    /// Aggregate power state across all Delta PSUs found on the chassis members.
    /// Delta reports commanded PSU on/off state under
    /// `Oem.deltaenergysystems.Power`.
    fn delta_power_state(&self) -> ModelPowerState {
        let supplies: Vec<&DeltaPowerSupply> = self
            .members
            .iter()
            .filter_map(|m| m.oem_delta_power_supplies.as_ref())
            .flatten()
            .collect();

        let state = powershelf_power_state(supplies.iter().map(|ps| ps.power_state));
        if state == ModelPowerState::Unknown && !supplies.is_empty() {
            let detail = supplies
                .iter()
                .map(|ps| format!("{}:{:?}", ps.id, ps.power_state))
                .join(", ");
            tracing::warn!(
                power_supply_states = %detail,
                "Delta power shelf power state is unknown"
            );
        }
        state
    }

    /// Synthesizes a [`ModelComputerSystem`] for a power shelf that does not
    /// expose a Redfish `ComputerSystem`. Identity is taken from the primary
    /// power-shelf chassis member (mirrors the libredfish Delta path).
    pub(crate) fn synthesized_powershelf_system(&self) -> ModelComputerSystem {
        let member = self
            .delta_powershelf_chassis()
            .or_else(|| self.members.first());

        let (id, manufacturer, model, serial_number) = member
            .map(|m| {
                let hw_id = m.chassis.hardware_id();
                (
                    m.chassis.id().to_string(),
                    hw_id.manufacturer.map(|v| v.to_string()),
                    hw_id.model.map(|v| v.to_string()),
                    hw_id
                        .serial_number
                        .map(|v| v.to_string().trim().to_string()),
                )
            })
            .unwrap_or_default();

        ModelComputerSystem {
            id,
            manufacturer,
            model,
            serial_number,
            power_state: self.delta_power_state(),
            ..Default::default()
        }
    }

    pub(crate) fn is_gb300(&self) -> bool {
        self.members.iter().any(|m| {
            m.chassis.hardware_id().manufacturer == Some(Manufacturer::new("NVIDIA"))
                && m.chassis.hardware_id().model == Some(Model::new("NVIDIA GB300"))
        })
    }

    pub(crate) fn is_mgx_c2(&self) -> bool {
        self.members.iter().any(|m| {
            let hardware_id = m.chassis.hardware_id();
            is_mgx_c2_processor_module(
                hardware_id.manufacturer.map(|v| v.into_inner()),
                hardware_id.model.map(|v| v.into_inner()),
                hardware_id.part_number.map(|v| v.into_inner()),
            )
        })
    }

    pub(crate) fn is_lenovo(&self) -> bool {
        self.members
            .iter()
            .any(|m| m.chassis.hardware_id().manufacturer == Some(Manufacturer::new("Lenovo")))
    }

    pub(crate) fn is_bluefield2(&self) -> bool {
        self.members
            .iter()
            .find(|c| c.chassis.id().into_inner() == "Card1")
            .is_some_and(|c| {
                let hw_id = c.chassis.hardware_id();
                hw_id.manufacturer == Some(Manufacturer::new("Nvidia"))
                    && hw_id.model == Some(Model::new("Bluefield 2 SmartNIC Main Card"))
            })
    }

    pub(crate) fn is_bluefield4(&self) -> bool {
        self.members.iter().any(|c| {
            c.chassis.hardware_id().model == Some(Model::new("B4240"))
                || c.chassis.hardware_id().model == Some(Model::new("B4240V"))
        })
    }

    pub(crate) fn dpu_card1_serial_number(&self) -> Result<Option<&str>, Error<B>> {
        let maybe_sn = self
            .members
            .iter()
            .find(|c| c.chassis.id().into_inner() == "Card1")
            .ok_or_else(Error::bmc_not_provided("chassis with id Card1"))?
            .chassis
            .hardware_id()
            .serial_number
            .map(|v| v.into_inner());
        Ok(maybe_sn)
    }

    // BF4 temporary PF0 fallback source:
    // read NDF0 PermanentMACAddress from known BF4 Redfish topology paths and
    // derive PF0 base MAC as (NDF0 - 0x10).
    // Remove callers once BF4 BMC exposes PF0 base MAC in ComputerSystem BaseMAC.
    pub(crate) fn dpu_bf4_ndf0_permanent_mac(&self) -> Option<BaseMac> {
        const BF4_NDF0_PATHS: [(&str, &str, &str); 2] = [
            // /redfish/v1/Chassis/BlueField_0/NetworkAdapters/BlueField_NIC_0/NetworkDeviceFunctions/0
            ("BlueField_0", "BlueField_NIC_0", "0"),
            // /redfish/v1/Chassis/Card1/NetworkAdapters/Bluefield_NIC/NetworkDeviceFunctions/0
            ("Card1", "Bluefield_NIC", "0"),
        ];
        for (chassis_id, adapter_id, function_id) in BF4_NDF0_PATHS {
            let mac = self
                .members
                .iter()
                .find(|c| c.chassis.id().into_inner() == chassis_id)
                .and_then(|c| {
                    c.network_adapters
                        .members()
                        .iter()
                        .find(|a| a.adapter.id().into_inner() == adapter_id)
                })
                .and_then(|adapter| adapter.functions.as_ref())
                .and_then(|functions| {
                    functions
                        .iter()
                        .find(|f| f.id().into_inner() == function_id)
                        .and_then(|f| f.ethernet_permanent_mac_address())
                });

            if let Some(mac) = mac
                && let Ok(parsed) = mac.as_str().parse::<MacAddress>()
            {
                let derived = mac_to_u64(parsed).checked_sub(BF4_NDF0_TO_BASE_MAC_OFFSET)?;
                return Some(u64_to_mac(derived).into());
            }
        }

        None
    }

    pub(crate) async fn pcie_devices(
        &self,
        chassis_filter: impl Fn(&ExploredChassis<B>) -> bool,
    ) -> Result<Vec<PcieDevice<B>>, Error<B>> {
        let mut pcie_devices = Vec::new();
        for c in &self.members {
            if chassis_filter(c)
                && let Some(collection) = c
                    .chassis
                    .pcie_devices()
                    .await
                    .map_err(Error::nv_redfish("chassis pcie devices"))?
            {
                let mut chassis_pcie_devices = collection
                    .members()
                    .await
                    .map_err(Error::nv_redfish("chassis pcie devices members"))?;
                pcie_devices.append(&mut chassis_pcie_devices);
            }
        }
        Ok(pcie_devices)
    }
}

fn mac_to_u64(mac: MacAddress) -> u64 {
    mac.bytes()
        .iter()
        .fold(0u64, |acc, &byte| (acc << 8) | u64::from(byte))
}

fn u64_to_mac(value: u64) -> MacAddress {
    let b = value.to_be_bytes();
    MacAddress::new([b[2], b[3], b[4], b[5], b[6], b[7]])
}

pub(crate) struct ExploredChassis<B: Bmc> {
    pub(crate) chassis: NvChassis<B>,
    pub(crate) network_adapters: ExploredNetworkAdapterCollection<B>,
    assembly_sn: Option<String>,
    oem_liteon_power_supplies: Option<Vec<LiteOnPowerSupply>>,
    oem_delta_power_supplies: Option<Vec<DeltaPowerSupply>>,
}

impl<B: Bmc> ExploredChassis<B> {
    async fn explore(chassis: NvChassis<B>, config: &Config) -> Result<Self, Error<B>> {
        let network_adapters =
            ExploredNetworkAdapterCollection::explore(&chassis, &config.network_adapter).await?;
        let assembly_sn = if let Some(model_check_fn) = (config.need_assembly_sn)(chassis.id()) {
            match chassis.assembly().await {
                Ok(Some(assembly)) => {
                    let assembly_data = assembly
                        .assemblies()
                        .await
                        .map_err(Error::nv_redfish("chassis assemblies"))?;
                    assembly_data
                        .iter()
                        .find(|asm| model_check_fn(asm.hardware_id().model))
                        .and_then(|asm| asm.hardware_id().serial_number)
                        .map(|v| v.to_string())
                }
                Ok(None) => None,
                Err(err) => {
                    return Err(Error::NvRedfish {
                        context: "chassis assembly",
                        err,
                    });
                }
            }
        } else {
            None
        };
        // Here we rely on the fact that
        // Chassis::oem_liteon_power_supply_links returns None
        // immediately if chassis is not LiteOn.
        let oem_liteon_power_supplies = if let Some(ps_links) = chassis
            .oem_liteon_power_supply_links()
            .await
            .map_err(Error::nv_redfish("LiteOn power supply links"))?
        {
            let mut power_supplies = Vec::new();
            for l in ps_links {
                let ps = l
                    .fetch()
                    .await
                    .map_err(Error::nv_redfish("LiteOn power supply"))?;
                power_supplies.push(LiteOnPowerSupply {
                    id: ps.base.id.clone(),
                    serial_number: ps.serial_number.clone().and_then(std::convert::identity),
                    power_state: ps.power_state,
                });
            }
            Some(power_supplies)
        } else {
            None
        };

        // Delta power shelves carry the commanded PSU on/off state as an OEM
        // extension (`Oem.deltaenergysystems.Power`) on the standard
        // PowerSubsystem power supplies. Only fetch it for Delta chassis to
        // avoid extra requests on unrelated hardware.
        let oem_delta_power_supplies = if chassis
            .hardware_id()
            .manufacturer
            .as_ref()
            .is_some_and(|mfg| mfg.as_ref().to_lowercase().contains("delta"))
        {
            let supplies = chassis
                .power_supplies()
                .await
                .map_err(Error::nv_redfish("Delta power supplies"))?;
            let power_supplies = supplies
                .iter()
                .map(|ps| DeltaPowerSupply {
                    id: ps.id().to_string(),
                    power_state: delta_psu_power_on(ps),
                })
                .collect();
            Some(power_supplies)
        } else {
            None
        };

        Ok(Self {
            chassis,
            network_adapters,
            assembly_sn,
            oem_liteon_power_supplies,
            oem_delta_power_supplies,
        })
    }

    fn to_model(&self) -> Chassis {
        let network_adapters = self.network_adapters.to_model();
        let chassis_id = self.chassis.id();
        let hw_id = self.chassis.hardware_id();
        let serial_number = self
            .assembly_sn
            .clone()
            .or(hw_id.serial_number.map(|v| v.to_string()))
            .map(|s| s.trim().to_string());

        let nvidia_oem = self.chassis.oem_nvidia_cbc().ok().and_then(identity);
        Chassis {
            id: chassis_id.to_string(),
            manufacturer: hw_id.manufacturer.map(|v| v.to_string()),
            model: hw_id.model.map(|v| v.to_string()),
            part_number: hw_id.part_number.map(|v| v.to_string()),
            serial_number,
            network_adapters,
            physical_slot_number: nvidia_oem
                .as_ref()
                .and_then(|x| x.chassis_physical_slot_number())
                .map(|v| v.into_inner() as i32),
            compute_tray_index: nvidia_oem
                .as_ref()
                .and_then(|x| x.compute_tray_index())
                .map(|v| v.into_inner() as i32),
            topology_id: nvidia_oem
                .as_ref()
                .and_then(|x| x.topology_id())
                .map(|v| v.into_inner() as i32),
            revision_id: nvidia_oem
                .as_ref()
                .and_then(|x| x.revision_id())
                .map(|v| v.into_inner() as i32),
        }
    }
}

struct LiteOnPowerSupply {
    id: String,
    serial_number: Option<String>,
    power_state: Option<bool>,
}

struct DeltaPowerSupply {
    id: String,
    power_state: Option<bool>,
}

/// Reads a Delta PSU's commanded on/off state from its OEM extension.
///
/// Delta power shelves report this as `Oem.deltaenergysystems.Power` (a bool)
/// on the standard `PowerSupply` resource, exposed via nv-redfish's typed
/// [`oem_delta`](NvPowerSupply::oem_delta) accessor. Returns `None` when the
/// Delta extension is absent, its `Power` flag is unset, or the OEM payload
/// fails to parse.
fn delta_psu_power_on<B: Bmc>(ps: &NvPowerSupply<B>) -> Option<bool> {
    match ps.oem_delta() {
        Ok(oem) => oem.and_then(|d| d.power()),
        Err(e) => {
            tracing::warn!(error = ?e, "Failed to parse Delta OEM power supply data");
            None
        }
    }
}

/// Delta power-shelf identity gate: a `chassis` id or an id containing `power`
/// before `shelf`, compared case-insensitively, whose manufacturer identifies
/// as Delta. The manufacturer distinguishes Delta from Lite-On shelves that can
/// share the generic `powershelf` chassis id. Split out so the gate can be
/// exercised in unit tests without a live BMC.
fn is_delta_powershelf_chassis(chassis_id: &str, manufacturer: Option<&str>) -> bool {
    let chassis_id = chassis_id.to_ascii_lowercase();
    let is_powershelf_id = chassis_id == "chassis"
        || chassis_id
            .split_once("power")
            .is_some_and(|(_, suffix)| suffix.contains("shelf"));

    is_powershelf_id && manufacturer.is_some_and(|mfg| mfg.to_lowercase().contains("delta"))
}

fn is_mgx_c2_processor_module(
    manufacturer: Option<&str>,
    model: Option<&str>,
    part_number: Option<&str>,
) -> bool {
    manufacturer.is_some_and(|v| v.eq_ignore_ascii_case("NVIDIA"))
        && (model.is_some_and(|v| v.starts_with("PG535"))
            || part_number.is_some_and(|v| v.contains("2G535")))
}

/// Aggregates per-PSU commanded on/off states into a single power-shelf state.
///
/// All PSUs reporting `Some(true)` means the shelf is on; all `Some(false)`
/// means off. An empty set, a mix, or any `None` yields `Unknown`.
fn powershelf_power_state(states: impl Iterator<Item = Option<bool>>) -> ModelPowerState {
    let states: Vec<Option<bool>> = states.collect();
    if states.is_empty() {
        return ModelPowerState::Unknown;
    }
    if states.iter().all(|v| *v == Some(true)) {
        ModelPowerState::On
    } else if states.iter().all(|v| *v == Some(false)) {
        ModelPowerState::Off
    } else {
        ModelPowerState::Unknown
    }
}

pub(crate) struct LiteOnSuppliesState<'a>(&'a [LiteOnPowerSupply]);

impl fmt::Display for LiteOnSuppliesState<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0
            .iter()
            .map(|s| format!("{}:{:?}:{:?}", s.id, s.serial_number, s.power_state))
            .join(", ")
            .fmt(f)
    }
}

impl LiteOnSuppliesState<'_> {
    pub(crate) fn to_model(&self) -> ModelPowerState {
        if self.0.is_empty() {
            return ModelPowerState::Unknown;
        }

        let on = self.0.iter().all(|v| v.power_state == Some(true));
        let off = self.0.iter().all(|v| v.power_state == Some(false));
        if on {
            ModelPowerState::On
        } else if off {
            ModelPowerState::Off
        } else {
            tracing::warn!(
                power_supply_states = %self,
                "powershelf power state is unknown"
            );
            ModelPowerState::Unknown
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ModelPowerState, is_delta_powershelf_chassis, is_mgx_c2_processor_module,
        powershelf_power_state,
    };

    #[test]
    fn identifies_mgx_c2_processor_modules() {
        let cases = [
            ("PG535 model", Some("NVIDIA"), Some("PG535-B02"), None, true),
            (
                "2G535 part number",
                Some("Nvidia"),
                None,
                Some("699-2G535-0200-310"),
                true,
            ),
            (
                "unrelated NVIDIA module",
                Some("NVIDIA"),
                Some("B4240"),
                Some("900-9D3B4-00CC-EA0"),
                false,
            ),
            (
                "non-NVIDIA PG535",
                Some("Supermicro"),
                Some("PG535-B02"),
                Some("699-2G535-0200-310"),
                false,
            ),
        ];

        for (case, manufacturer, model, part_number, expected) in cases {
            assert_eq!(
                is_mgx_c2_processor_module(manufacturer, model, part_number),
                expected,
                "{case}",
            );
        }
    }

    // is_delta_powershelf_chassis gates Delta detection on both a recognized
    // power-shelf chassis id and a Delta manufacturer.
    #[test]
    fn is_delta_powershelf_chassis_gates_on_id_and_manufacturer() {
        let cases: [(&str, Option<&str>, bool); 15] = [
            // Legacy and power-before-shelf ids are case-insensitive.
            ("chassis", Some("DELTA"), true),
            ("CHASSIS", Some("DELTA"), true),
            ("powershelf", Some("Delta"), true),
            ("PowerShelf_0", Some("DELTA"), true),
            ("delta-power-module-shelf_0", Some("DELTA"), true),
            // Case-insensitive, substring match on the manufacturer.
            ("chassis", Some("delta electronics"), true),
            ("powershelf", Some("Delta Energy Systems"), true),
            // Both words are required, with "power" before "shelf".
            ("ShelfPower_0", Some("DELTA"), false),
            ("PowerUnit_0", Some("DELTA"), false),
            ("Shelf_0", Some("DELTA"), false),
            // Unrelated chassis ids are ignored.
            ("Card1", Some("DELTA"), false),
            ("Baseboard", Some("delta"), false),
            // Power-shelf chassis id but a different (or missing) manufacturer.
            ("powershelf", Some("Lite-On"), false),
            ("PowerShelf_0", Some("NVIDIA"), false),
            ("PowerShelf_0", None, false),
        ];
        for (id, mfg, expected) in cases {
            assert_eq!(
                is_delta_powershelf_chassis(id, mfg),
                expected,
                "id={id:?} manufacturer={mfg:?}"
            );
        }
    }

    // powershelf_power_state collapses per-PSU flags: all-on => On, all-off =>
    // Off, and empty / mixed / unknown => Unknown.
    #[test]
    fn powershelf_power_state_aggregates_psu_flags() {
        let cases: [(&[Option<bool>], ModelPowerState); 6] = [
            (&[], ModelPowerState::Unknown),
            (&[Some(true), Some(true)], ModelPowerState::On),
            (&[Some(false), Some(false)], ModelPowerState::Off),
            (&[Some(true), Some(false)], ModelPowerState::Unknown),
            (&[Some(true), None], ModelPowerState::Unknown),
            (&[None], ModelPowerState::Unknown),
        ];
        for (states, expected) in cases {
            assert_eq!(
                powershelf_power_state(states.iter().copied()),
                expected,
                "states: {states:?}"
            );
        }
    }
}
