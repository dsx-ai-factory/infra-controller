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

#[cfg(any(test, feature = "test-support"))]
use std::collections::HashMap;

use model::machine::HostHealthConfig;
use serde::{Deserialize, Serialize};

pub mod bom_validation;
pub mod controller;
pub mod firmware_global;
pub mod machine_validation;
pub mod power_manager;

pub use bom_validation::BomValidationConfig;
pub use controller::MachineStateControllerConfig;
pub use firmware_global::FirmwareGlobal;
pub use machine_validation::MachineValidationConfig;
pub use power_manager::PowerManagerOptions;

pub struct MachineStateHandlerSiteConfig {
    pub pxe_public_base_url: String,
    pub firmware_global: FirmwareGlobal,
    pub machine_state_controller: MachineStateControllerConfig,
    pub host_health: HostHealthConfig,
    /// Rack profiles used to refine DPF deployment selection from the host's rack identity.
    pub rack_profiles: model::rack_type::RackProfileConfig,

    pub selected_profile: libredfish::BiosProfileType,
    pub bios_profiles: libredfish::BiosProfileVendor,
    pub oem_manager_profiles: libredfish::BiosProfileVendor,

    pub ewethers_enabled: bool,
    /// Site-wide enable for the Astra (East-West CX NIC) path. When `false`,
    /// host ingestion skips enabling Astra on declared CX9 NICs.
    pub astra_enabled: bool,
    pub dpf_enabled: bool,
    /// Site-wide enable for releasing the DPF maintenance hold so a changed
    /// DPUService rolls out. When `false`, a host that DPF has parked in the
    /// NodeEffect phase keeps its hold and its pending marker.
    pub dpu_service_sync_enabled: bool,
    pub spdm_enabled: bool,

    /// Site-wide kill-switch for the passive BMC credential rotation guard. When
    /// `false`, a Ready host never enters `RotatingBmc` on its own; the operator
    /// force-converge escape hatch still works regardless.
    pub bmc_rotation_enabled: bool,

    /// Site-wide kill-switch for the passive UEFI credential rotation guard,
    /// covering both host and DPU UEFI. When `false`, a Ready host never enters
    /// `RotatingHostUefi` (nor drives a DPU into `RotatingDpuUefi`) on its own;
    /// the per-machine operator force-converge escape hatch still works
    /// regardless, for the host or an individual DPU.
    pub uefi_rotation_enabled: bool,

    /// Site-wide kill-switch for the passive SuperNIC lockdown IKM rekey guard.
    /// When `false` (the default), a Ready host never enters
    /// `RotatingNicLockdown` on its own even if a card lags the staged site-wide
    /// target; the per-host operator force-converge escape hatch still works
    /// regardless.
    pub nic_lockdown_ikm_rotation_enabled: bool,

    /// Site-wide opt-in for factory-resetting the host BMC during tenant
    /// release. When `false` (the default), tenant release skips the BMC
    /// factory-reset sub-flow and proceeds directly to `PowerCycle`.
    pub bmc_factory_reset_on_instance_termination_enabled: bool,

    pub dpu_enable_secure_boot: bool,
    pub restart_ovs_on_use_admin_network_change: bool,
}

impl MachineStateHandlerSiteConfig {
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_default() -> Self {
        Self {
            pxe_public_base_url: "http://carbide-pxe.forge:8080".to_string(),
            firmware_global: FirmwareGlobal::test_default(),
            machine_state_controller: MachineStateControllerConfig::test_default(),
            host_health: HostHealthConfig::default(),
            rack_profiles: model::rack_type::RackProfileConfig::default(),
            selected_profile: libredfish::BiosProfileType::Performance,
            bios_profiles: HashMap::new(),
            oem_manager_profiles: HashMap::new(),
            ewethers_enabled: true,
            astra_enabled: false,
            dpf_enabled: false,
            dpu_service_sync_enabled: true,
            spdm_enabled: false,
            bmc_rotation_enabled: false,
            uefi_rotation_enabled: false,
            nic_lockdown_ikm_rotation_enabled: false,
            bmc_factory_reset_on_instance_termination_enabled: false,
            dpu_enable_secure_boot: true,
            restart_ovs_on_use_admin_network_change: false,
        }
    }
}

/// A UTC time window defined by a start and end timestamp.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TimePeriod {
    /// Start of the time window (UTC).
    pub start: chrono::DateTime<chrono::Utc>,
    /// End of the time window (UTC).
    pub end: chrono::DateTime<chrono::Utc>,
}
