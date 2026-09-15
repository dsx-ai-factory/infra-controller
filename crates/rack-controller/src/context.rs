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

use carbide_health_metrics::PerObjectMetricsRegistry;
use carbide_rack_controller::config::RackConfig;
use carbide_rack_controller::metrics::RackMetrics;
use carbide_secrets::credentials::CredentialManager;
use component_manager::component_manager::ComponentManager;
use component_manager::{NvosUpdateManager, RackFirmwareUpdateManager};
use sqlx::PgPool;
use state_controller::state_handler::StateHandlerContextObjects;

use crate as carbide_rack_controller;
use crate::firmware_object::FirmwareObjectFetcher;

pub struct RackStateHandlerContextObjects {}

/// Dependencies shared by rack state-handler operations.
#[derive(Clone)]
pub struct RackStateHandlerServices {
    pub db_pool: PgPool,

    // TODO: probably this is not the best place for config. But this
    // field is introduced during refactoring. In original code it was
    // full CarbideConfig.
    pub site_config: Arc<RackConfig>,

    /// Backend-neutral rack NVOS update operations.
    pub nvos_update_manager: Option<Arc<dyn NvosUpdateManager>>,

    /// Backend-neutral rack firmware-object update operations.
    pub rack_firmware_update_manager: Option<Arc<dyn RackFirmwareUpdateManager>>,

    pub credential_manager: Arc<dyn CredentialManager>,
    /// Component manager used for switch operations during rack maintenance.
    pub component_manager: Option<Arc<ComponentManager>>,

    /// Switch mTLS services sourced from
    /// `[rack_state_controller].nmx_cluster_switch_mtls_services`. Unread:
    /// rack maintenance does not configure switch certificates.
    pub nmx_cluster_switch_mtls_services: Vec<i32>,

    /// Fetches SOT firmware-object documents selected by rack profiles.
    pub firmware_object_fetcher: Arc<dyn FirmwareObjectFetcher>,

    /// Shared registry backing the generic per-object health metrics.
    pub per_object_metrics_registry: Arc<PerObjectMetricsRegistry>,
}

impl StateHandlerContextObjects for RackStateHandlerContextObjects {
    type ObjectMetrics = RackMetrics;
    type Services = RackStateHandlerServices;
}
