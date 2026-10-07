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
use carbide_uuid::instance_type::InstanceTypeId;
use chrono::{DateTime, Utc};
use config_version::Versioned;

use crate::machine::Dpf;
use crate::machine_boot_interface::{BootInterfaceSelection, MachineBootInterfaceTarget};

/// Desired state for a machine, mutable through API calls that increment the
/// machine version.
///
/// Corresponds to `MachineConfig` in the forge proto, except for the internal
/// boot interface target and its selection metadata. Site Explorer initializes
/// that target from discovery and may add its Redfish id later when the MAC
/// still matches. Operator API calls may replace it.
#[derive(Debug, Clone, Default)]
pub struct MachineConfig {
    /// Override to enable or disable firmware auto-update.
    pub firmware_autoupdate: Option<bool>,

    /// The instance type this machine is associated with, if any.
    pub instance_type_id: Option<InstanceTypeId>,

    /// DPF configuration for this machine (operator-enabled).
    pub dpf: Dpf,

    /// The declared desired hardware SKU (set via the AssignSku API).
    /// Distinct from `MachineStatus::hw_sku`, which reflects the observed match.
    pub hw_sku: Option<String>,

    /// The host boot interface reserved for machine-controller convergence
    /// through Redfish.
    pub desired_boot_interface: Option<Versioned<MachineBootInterfaceTarget>>,

    /// Records why `desired_boot_interface` was selected; it is present exactly
    /// when the desired target is present.
    pub boot_interface_selection: Option<BootInterfaceSelection>,

    /// Maintenance reference token set when this machine is placed into maintenance mode.
    pub maintenance_reference: Option<String>,

    /// When maintenance mode began, if active.
    pub maintenance_start_time: Option<DateTime<Utc>>,
}
