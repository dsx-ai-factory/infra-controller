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

//! Context types for the extension-service state handler.

use std::sync::Arc;

use carbide_machine_controller::dpf::DpfOperations;
use sqlx::PgPool;
use state_controller::state_handler::StateHandlerContextObjects;

/// Controller context for DPF Helm chart lifecycle reconciliation.
pub struct ExtensionServiceStateHandlerContextObjects {}

/// Services available to the extension-service state handler.
#[derive(Clone)]
pub struct ExtensionServiceStateHandlerServices {
    pub db_pool: PgPool,

    /// Absent only when DPF is disabled.
    pub dpf_sdk: Option<Arc<dyn DpfOperations>>,
}

impl StateHandlerContextObjects for ExtensionServiceStateHandlerContextObjects {
    type Services = ExtensionServiceStateHandlerServices;
    type ObjectMetrics = ();
}
