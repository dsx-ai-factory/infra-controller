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
use chrono::{DateTime, Utc};

use crate::bmc_info::BmcInfo;
use crate::hardware_info::{HardwareInfo, MachineInventory, MachineNvLinkInfo};
use crate::instance::status::extension_service::InstanceExtensionServiceStatusObservationByType;
use crate::machine::infiniband::MachineInfinibandStatusObservation;
use crate::machine::nvlink::MachineNvLinkStatusObservation;
use crate::machine::spx::MachineSpxStatusObservation;
use crate::machine::{FailureDetails, MachineInterfaceSnapshot, MachineLastRebootRequested};
use crate::machine_boot_interface::BootInterfaceStatusObservation;
use crate::power_manager::PowerOptions;
use crate::sku::SkuStatus;

/// System-observed state for a machine.
///
/// Corresponds to `MachineStatus` in the protobuf.
#[derive(Debug, Clone)]
pub struct MachineStatus {
    pub interfaces: Vec<MachineInterfaceSnapshot>,
    /// Latest persisted convergence status for the desired boot interface.
    pub boot_interface_status_observation: Option<BootInterfaceStatusObservation>,
    pub hardware_info: Option<HardwareInfo>,
    pub bmc_info: BmcInfo,
    pub last_reboot_time: Option<DateTime<Utc>>,
    pub last_cleanup_time: Option<DateTime<Utc>>,
    pub last_discovery_time: Option<DateTime<Utc>>,
    pub last_scout_contact_time: Option<DateTime<Utc>>,
    pub last_scout_observed_version: Option<String>,
    pub failure_details: FailureDetails,
    pub inventory: Option<MachineInventory>,
    pub last_reboot_requested: Option<MachineLastRebootRequested>,
    pub hw_sku: Option<SkuStatus>,
    pub hw_sku_device_type: Option<String>,
    pub update_complete: bool,
    pub nvlink_info: Option<MachineNvLinkInfo>,
    pub infiniband_status_observation: Option<MachineInfinibandStatusObservation>,
    pub nvlink_status_observation: Option<MachineNvLinkStatusObservation>,
    pub spx_status_observation: Option<MachineSpxStatusObservation>,
    pub extension_service_status_observations: InstanceExtensionServiceStatusObservationByType,
    pub slot_number: Option<i32>,
    pub tray_index: Option<i32>,
    /// Power management state for this machine (hosts only; absent for DPUs).
    pub power_options: Option<PowerOptions>,
}
