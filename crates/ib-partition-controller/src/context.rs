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

use carbide_ib_fabric::ib::IBFabricManager;
use model::resource_pool::common::IbPools;
use sqlx::PgPool;
use state_controller::state_handler::StateHandlerContextObjects;

pub struct IBPartitionStateHandlerContextObjects {}

#[derive(Clone)]
pub struct IBPartitionStateHandlerServices {
    pub db_pool: PgPool,
    /// API for interaction with Forge IBFabricManager
    pub ib_fabric_manager: Arc<dyn IBFabricManager>,
    /// Resource pools for ib pkey allocation/release.
    pub ib_pools: IbPools,
}

impl StateHandlerContextObjects for IBPartitionStateHandlerContextObjects {
    type Services = IBPartitionStateHandlerServices;
    type ObjectMetrics = ();
}
