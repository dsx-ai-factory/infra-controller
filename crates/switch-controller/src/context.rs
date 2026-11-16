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
use model::switch::Switch;
use sqlx::PgPool;
use state_controller::state_handler::{
    ExternalServiceError, StateHandlerContextObjects, StateHandlerError,
};

use crate::metrics::SwitchMetrics;

pub struct SwitchStateHandlerContextObjects {}

#[derive(Clone)]
pub struct SwitchStateHandlerServices {
    /// Event metric destination shared by the controller and credential rotation engine.
    pub instrumentation: carbide_instrument::Instrumentation,
    pub db_pool: PgPool,
    pub component_manager: Option<Arc<ComponentManager>>,
    pub credential_manager: Arc<dyn CredentialManager>,
    /// RMS `SwitchService` values passed to certificate configuration calls.
    pub switch_mtls_services: Vec<i32>,
    /// Shared registry backing the generic per-object health metrics.
    pub per_object_metrics_registry: Arc<PerObjectMetricsRegistry>,
    /// Libredfish pool used to converge the switch BMC credential. The
    /// same shared instance the machine-controller uses.
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
    /// a Ready switch never enters `RotatingBmc` on its own; the operator
    /// force-converge escape hatch still works regardless.
    pub bmc_rotation_enabled: bool,
}

impl SwitchStateHandlerServices {
    /// Connects to `switch`'s BMC through the configured `bmc_backend`.
    pub(crate) async fn connect_bmc(
        &self,
        switch: &Switch,
    ) -> Result<BackendBmc, StateHandlerError> {
        let missing = |missing| StateHandlerError::MissingData {
            object_id: switch.id.to_string(),
            missing,
        };
        let bmc_info = switch
            .bmc_info
            .as_ref()
            .ok_or_else(|| missing("bmc_info"))?;
        let ip = bmc_info.ip.ok_or_else(|| missing("bmc_ip"))?;
        let access = BmcAccessInfo {
            host: ip.to_string(),
            port: bmc_info.port,
            mac_address: bmc_info.mac.ok_or_else(|| missing("bmc_mac"))?,
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

impl StateHandlerContextObjects for SwitchStateHandlerContextObjects {
    type ObjectMetrics = SwitchMetrics;
    type Services = SwitchStateHandlerServices;
}
