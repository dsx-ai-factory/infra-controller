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
use carbide_redfish::libredfish::{BmcCredentialOps, RedfishClientPool};
use carbide_secrets::credentials::CredentialManager;
use component_manager::component_manager::ComponentManager;
use sqlx::PgPool;
use state_controller::state_handler::StateHandlerContextObjects;

use crate::metrics::SwitchMetrics;

pub struct SwitchStateHandlerContextObjects {}

#[derive(Clone)]
pub struct SwitchStateHandlerServices {
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

impl StateHandlerContextObjects for SwitchStateHandlerContextObjects {
    type ObjectMetrics = SwitchMetrics;
    type Services = SwitchStateHandlerServices;
}
