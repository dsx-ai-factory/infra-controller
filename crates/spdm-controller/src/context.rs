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

use carbide_redfish::libredfish::RedfishClientPool;
use sqlx::PgPool;
use state_controller::state_handler::StateHandlerContextObjects;

use crate::metrics::AttestationMetrics;

pub struct SpdmStateHandlerContextObjects {}

#[derive(Clone)]
pub struct SpdmStateHandlerServices {
    pub db_pool: PgPool,
    pub redfish_client_pool: Arc<dyn RedfishClientPool>,
}

impl StateHandlerContextObjects for SpdmStateHandlerContextObjects {
    type Services = SpdmStateHandlerServices;
    type ObjectMetrics = AttestationMetrics;
}
