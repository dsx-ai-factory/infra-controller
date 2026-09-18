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

use std::sync::Arc;

use carbide_credential_rotation::RotationGate;
use carbide_health_metrics::PerObjectMetricsRegistry;
use carbide_ipmi::IPMITool;
use carbide_redfish::libredfish::{BmcCredentialOps, RedfishClientPool};
use carbide_secrets::credentials::CredentialManager;
use carbide_uuid::machine::MachineIdSubtypeTrait;
use component_manager::component_manager::ComponentManager;
use db::db_read::PgPoolReader;
use libredfish::Redfish;
use model::machine::Machine;
use sqlx::PgPool;
use state_controller::state_handler::{StateHandlerContextObjects, StateHandlerError};

use crate::config::MachineStateHandlerSiteConfig;
use crate::metrics::MachineMetrics;
use crate::per_object::MachinePerObjectInfo;

pub struct MachineStateHandlerContextObjects {}

impl StateHandlerContextObjects for MachineStateHandlerContextObjects {
    type Services = MachineStateHandlerServices;
    type ObjectMetrics = MachineMetrics;
}

#[derive(Clone)]
pub struct MachineStateHandlerServices {
    pub db_pool: PgPool,
    /// Postgres database pool that can be passed directly to read-only db functions without a
    /// transaction
    pub db_reader: PgPoolReader,
    /// API for interaction with Libredfish
    pub redfish_client_pool: Arc<dyn RedfishClientPool>,
    /// Credential-lifecycle operations (password set/rotate/clear,
    /// candidate validation). A sealed trait implemented only by the direct
    /// pool, so handing these to a wrapper pool is a compile error (a
    /// wrong-pool guard, not a wire-path guarantee -- see
    /// [`BmcCredentialOps`]).
    pub bmc_credential_ops: Arc<dyn BmcCredentialOps>,
    /// An implementation of the IPMITool that understands how to reboot a machine
    pub ipmi_tool: Arc<dyn IPMITool>,
    /// Configuration used by MachineStateHandler.
    pub site_config: Arc<MachineStateHandlerSiteConfig>,
    /// Optional Component Manager backend for rack-scale maintenance operations.
    pub component_manager: Option<Arc<ComponentManager>>,
    pub credential_manager: Arc<dyn CredentialManager>,
    /// Short-TTL cache of the site-wide BMC rotation aggregate, shared across
    /// this replica's per-object ticks so the steady state costs one aggregate
    /// query per TTL window rather than a per-device query every sweep.
    pub bmc_rotation_gate: RotationGate,
    /// Short-TTL cache of the site-wide host-UEFI rotation aggregate, shared
    /// across this replica's per-object ticks. Family-scoped and separate from
    /// `bmc_rotation_gate` so a UEFI sweep never consults BMC counts.
    pub host_uefi_rotation_gate: RotationGate,
    /// Short-TTL cache of the site-wide DPU-UEFI rotation aggregate, shared
    /// across this replica's per-object ticks. A `RotationGate` is single-family,
    /// so DPU UEFI gets its own gate separate from `host_uefi_rotation_gate`: a
    /// DPU sweep queries only `dpu_uefi` counts, keyed by each DPU's BMC MAC.
    pub dpu_uefi_rotation_gate: RotationGate,
    /// Short-TTL cache of the site-wide DPU BMC `service` rotation aggregate,
    /// shared across this replica's per-object ticks. Single-family like the
    /// others, keyed by each DPU's BMC MAC; only BF4 DPUs are ever enrolled, so
    /// it reads zero on fleets without them. Convergence rides the same
    /// `RotatingBmc` state as BMC root (a single root-authenticated Redfish call
    /// with no host-power impact), so this gate only decides *entry*.
    pub dpu_bmc_service_rotation_gate: RotationGate,
    /// Short-TTL cache of the site-wide NIC lockdown IKM rotation aggregate,
    /// shared across this replica's per-object ticks. Family-scoped to
    /// `lockdown_ikm` and keyed by each NIC's MAC.
    pub nic_lockdown_rotation_gate: RotationGate,
    /// Shared registry backing the generic per-object health metrics.
    pub per_object_metrics_registry: Arc<PerObjectMetricsRegistry>,
    /// Trait/association info gauges for the per-object metrics endpoint,
    /// present when per-object state metrics are enabled for machines.
    pub per_object_info: Option<MachinePerObjectInfo>,
}

impl MachineStateHandlerServices {
    pub async fn create_redfish_client_from_machine(
        &self,
        machine: &Machine<impl MachineIdSubtypeTrait>,
    ) -> Result<Box<dyn Redfish>, StateHandlerError> {
        let bmc_access_info = self.bmc_access_info_for_machine(machine).await?;
        self.redfish_client_pool
            .client_by_info(&bmc_access_info)
            .await
            .map_err(StateHandlerError::from)
    }

    /// Resolves a machine's BMC access info; credential-lifecycle call sites
    /// hand this to [`BmcCredentialOps`], which builds its own direct client.
    pub(crate) async fn bmc_access_info_for_machine(
        &self,
        machine: &Machine<impl MachineIdSubtypeTrait>,
    ) -> Result<carbide_utils::redfish::BmcAccessInfo, StateHandlerError> {
        let addr = machine
            .bmc_addr()
            .ok_or_else(|| StateHandlerError::MissingData {
                object_id: machine.id.to_string(),
                missing: "BMC Endpoint Information (bmc_info.ip)",
            })?;
        Ok(db::machine_interface::lookup_bmc_access_info(
            &self.db_pool,
            addr.ip(),
            Some(addr.port()),
        )
        .await?)
    }
}
