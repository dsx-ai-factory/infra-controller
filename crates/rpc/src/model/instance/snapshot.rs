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
use config_version::Versioned;
use model::health::HealthReportSources;
use model::instance::snapshot::InstanceSnapshot;
use model::instance::status::InstanceStatus;
use model::machine::infiniband::MachineInfinibandStatusObservation;
use model::machine::nvlink::MachineNvLinkStatusObservation;
use model::machine::spx::MachineSpxStatusObservation;
use model::machine::{ManagedHostState, ReprovisionRequest};

use crate::errors::RpcDataConversionError;
use crate::model::instance::status::instance_status_from_config_and_observation;

/// Derives the tenant and site-admin facing [`InstanceStatus`] from the
/// snapshot information about the instance
#[allow(clippy::too_many_arguments)]
pub fn instance_snapshot_derive_status(
    snapshot: &InstanceSnapshot,
    dpu_id_to_device_map: HashMap<String, Vec<DpuMachineId>>,
    primary_dpu_machine_id: Option<DpuMachineId>,
    managed_host_state: ManagedHostState,
    reprovision_request: Option<ReprovisionRequest>,
    ib_status: Option<&MachineInfinibandStatusObservation>,
    nvlink_status: Option<&MachineNvLinkStatusObservation>,
    spx_status: Option<&MachineSpxStatusObservation>,
    host_health: &HealthReportSources,
) -> Result<InstanceStatus, RpcDataConversionError> {
    instance_status_from_config_and_observation(
        dpu_id_to_device_map,
        primary_dpu_machine_id,
        Versioned::new(&snapshot.config, snapshot.config_version),
        Versioned::new(&snapshot.config.network, snapshot.network_config_version),
        Versioned::new(&snapshot.config.infiniband, snapshot.ib_config_version),
        Versioned::new(
            &snapshot.config.extension_services,
            snapshot.extension_services_config_version,
        ),
        Versioned::new(&snapshot.config.nvlink, snapshot.nvlink_config_version),
        Versioned::new(&snapshot.config.spxconfig, snapshot.spx_config_version),
        &snapshot.observations,
        managed_host_state,
        snapshot.deleted.is_some(),
        reprovision_request,
        ib_status,
        nvlink_status,
        spx_status,
        snapshot.update_network_config_request.is_some(),
        host_health,
    )
}
