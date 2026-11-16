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

use bmc_backend::{BackendBmc, BmcConnectError, BmcConnector};
use carbide_credential_rotation::RotationGate;
use carbide_health_metrics::PerObjectMetricsRegistry;
use carbide_redfish::libredfish::{BmcCredentialOps, RedfishClientPool};
use carbide_secrets::credentials::CredentialManager;
use carbide_utils::redfish::BmcAccessInfo;
use component_manager::component_manager::ComponentManager;
use model::power_shelf::PowerShelf;
use sqlx::PgPool;
use state_controller::state_handler::{
    ExternalServiceError, StateHandlerContextObjects, StateHandlerError,
};

use crate::metrics::PowerShelfMetrics;

pub struct PowerShelfStateHandlerContextObjects {}

#[derive(Clone)]
pub struct PowerShelfStateHandlerServices {
    /// Event metric destination shared by the controller and credential rotation engine.
    pub instrumentation: carbide_instrument::Instrumentation,
    pub db_pool: PgPool,
    pub component_manager: Option<Arc<ComponentManager>>,
    pub credential_manager: Arc<dyn CredentialManager>,
    /// Shared registry backing the generic per-object health metrics.
    pub per_object_metrics_registry: Arc<PerObjectMetricsRegistry>,
    /// When `true`, Ready accepts rack-level `power_shelf_reprovisioning_requested`
    /// and enters `ReProvisioning::WaitingForRackFirmwareUpgrade`. Defaults to
    /// `false` so power shelves stay out of rack firmware wait unless explicitly
    /// enabled.
    pub rack_firmware_reprovisioning_enabled: bool,
    /// Libredfish pool used to converge the power shelf BMC (PMC) credential
    /// The same shared instance the machine- and switch-controllers use.
    pub redfish_client_pool: Arc<dyn RedfishClientPool>,
    /// Connects moved BMC operations through the site's `bmc_backend`.
    pub bmc_connector: BmcConnector,
    /// Credential-lifecycle operations (password set/rotate/clear,
    /// candidate validation). A sealed trait implemented only by the direct
    /// pool, so handing these to a wrapper pool is a compile error (a
    /// wrong-pool guard, not a wire-path guarantee -- see
    /// [`BmcCredentialOps`]).
    pub bmc_credential_ops: Arc<dyn BmcCredentialOps>,
    /// Short-TTL cache of the site-wide BMC rotation aggregate, shared across
    /// this replica's per-object ticks so the steady state costs one aggregate
    /// query per TTL window rather than a per-device query every sweep.
    pub bmc_rotation_gate: RotationGate,
    /// Site-wide kill-switch for passive BMC credential rotation. When `false`,
    /// a Ready power shelf never enters `RotatingBmc` on its own; the operator
    /// force-converge escape hatch still works regardless.
    pub bmc_rotation_enabled: bool,
}

impl PowerShelfStateHandlerServices {
    /// Connects to `power_shelf`'s BMC through the configured `bmc_backend`.
    pub(crate) async fn connect_bmc(
        &self,
        power_shelf: &PowerShelf,
    ) -> Result<BackendBmc, StateHandlerError> {
        let missing = |missing| StateHandlerError::MissingData {
            object_id: power_shelf.id.to_string(),
            missing,
        };
        let bmc_info = power_shelf
            .bmc_info
            .as_ref()
            .ok_or_else(|| missing("addressable_bmc"))?;
        let (Some(ip), Some(mac_address)) = (bmc_info.ip, bmc_info.mac) else {
            return Err(missing("addressable_bmc"));
        };
        let access = BmcAccessInfo {
            host: ip.to_string(),
            port: bmc_info.port,
            mac_address,
        };
        let report = || async {
            db::explored_endpoints::find_by_ips(&self.db_pool, vec![ip])
                .await?
                .into_iter()
                .next()
                .map(|explored| explored.report)
                .ok_or_else(|| missing("explored endpoint of the BMC"))
        };
        self.bmc_connector
            .connect(&access, report)
            .await
            .map_err(|error| match error {
                BmcConnectError::Report(error) => error,
                BmcConnectError::Libredfish(error) => error.into(),
                BmcConnectError::Drivers(error) => ExternalServiceError::with_source(
                    "redfish",
                    "create_client",
                    error.to_string(),
                    "redfish_client_creation_error",
                    error,
                )
                .into(),
            })
    }
}

impl StateHandlerContextObjects for PowerShelfStateHandlerContextObjects {
    type ObjectMetrics = PowerShelfMetrics;
    type Services = PowerShelfStateHandlerServices;
}
