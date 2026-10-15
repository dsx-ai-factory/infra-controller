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
use carbide_uuid::rack::RackId;

/// MachineSearchConfig: Search parameters
#[derive(Default, Debug, Clone)]
pub struct MachineSearchConfig {
    pub include_dpus: bool,
    pub include_history: bool,
    pub include_predicted_host: bool,
    /// Only include machines in maintenance mode
    pub only_maintenance: bool,
    /// Only include quarantined machines
    pub only_quarantine: bool,
    pub exclude_hosts: bool,
    /// Returns machines only if they are assigned the given instance type
    pub instance_type_id: Option<InstanceTypeId>,

    /// Whether the query results will be later
    /// used for updates in the same transaction.
    ///
    /// Triggers one or more locking behaviors in the DB.
    ///
    /// This applies *only* to the immediate machines records
    /// and any joined tables.  The value is *not*
    /// propagated to any additional underlying queries.
    pub for_update: bool,
    /// Only include NVLink capable machines (GB200/GB300/VR etc), identified by GPU
    /// `name` or DMI `product_name` on entries with `platform_info` in discovered topology.
    pub mnnvl_only: bool,
    pub only_with_power_state: Option<String>,
    pub only_with_health_alert: Option<String>,
    /// Returns machines only if they are part of the given rack
    pub rack_id: Option<RackId>,
    /// Filter by the top-level controller state tag (e.g. "created", "ready")
    pub controller_state: Option<String>,
}
