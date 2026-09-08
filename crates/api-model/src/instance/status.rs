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

use std::collections::HashMap;

use carbide_uuid::machine::DpuMachineId;
use serde::{Deserialize, Serialize};

use crate::machine::ReprovisionRequest;

pub mod extension_service;
pub mod infiniband;
pub mod network;
pub mod nvlink;
pub mod spx;
pub mod tenant;

/// Instance status
///
/// This represents the actual status of an Instance
#[derive(Debug, Clone)]
pub struct InstanceStatus {
    /// Status that is related to the tenant of the instance.
    /// In case no tenant has been assigned to this instance, the field would be absent.
    pub tenant: Option<tenant::InstanceTenantStatus>,

    /// Status of the networking subsystem of an instance
    pub network: network::InstanceNetworkStatus,

    /// Status of the infiniband subsystem of an instance
    pub infiniband: infiniband::InstanceInfinibandStatus,

    /// Status of the extension services configured on an instance
    pub extension_services: extension_service::InstanceExtensionServicesStatus,

    /// Status of nvlink subsystem of an instance
    pub nvlink: nvlink::InstanceNvLinkStatus,

    /// Status of the SPX subsystem of an instance
    pub spx_status: spx::InstanceSpxStatus,

    /// Whether all configurations related to an instance are in-sync.
    /// This is a logical AND for the settings of all sub-configurations.
    /// At this time it equals `InstanceNetworkStatus::configs_synced`,
    /// but might in the future also include readiness for other subsystems.
    pub configs_synced: SyncState,

    /// Whether there is one reprovision request on the underlying Machine
    /// TODO: This might be multiple. and potentially it it should be
    /// `InstanceUpdateStatus` instead of `ReprovisionRequest`
    pub reprovision_request: Option<ReprovisionRequest>,
}

/// Whether user configurations have been applied
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum SyncState {
    /// All configuration changes that users requested have been applied
    Synced,
    // At least one configuration change to an active instance has not yet been processed
    Pending,
}

/// Contains all reports we have about the current instances state
///
/// We combine these with the desired config to derive instance state that we
/// signal to customers
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceStatusObservations {
    /// Observed status of the networking subsystem
    pub network: HashMap<DpuMachineId, network::InstanceNetworkStatusObservation>,

    /// Observed extension-service status, partitioned by service type. Each
    /// service type has one authoritative writer.
    pub extension_services:
        HashMap<DpuMachineId, extension_service::InstanceExtensionServiceStatusObservationByType>,

    /// Has the instance phoned home?
    pub phone_home_last_contact: Option<chrono::DateTime<chrono::Utc>>,
}
