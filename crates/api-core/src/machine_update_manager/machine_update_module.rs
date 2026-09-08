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

use std::collections::{HashMap, HashSet};
use std::fmt;

use async_trait::async_trait;
use carbide_uuid::machine::{HostMachineId, MachineId};
use model::machine::ManagedHostStateSnapshot;
use sqlx::PgConnection;

use crate::CarbideResult;

/// Used by [MachineUpdateManager](crate::machine_update_manager::MachineUpdateManager) to initiate
/// machine updates.  A module is responsible for managing its own updates and accurately reporting
/// the number of outstanding updates.
///
/// NOTE: Updating machines are treated as managed hosts and identified by the host machine id.  DPU
/// updates are identified by using the host machine id, and the host/DPU pair should be treated as one.
#[async_trait]
pub(crate) trait MachineUpdateModule: Send + Sync + fmt::Display {
    async fn get_updates_in_progress(
        &self,
        txn: &mut PgConnection,
    ) -> CarbideResult<HashSet<MachineId>>;

    async fn start_updates(
        &self,
        pool: &sqlx::Pool<sqlx::Postgres>,
        available_updates: i32,
        updating_host_machines: &HashSet<MachineId>,
        snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<HashSet<MachineId>>;

    async fn clear_completed_updates(&self, txn: &mut PgConnection) -> CarbideResult<()>;

    async fn update_metrics(
        &self,
        pool: &sqlx::Pool<sqlx::Postgres>,
        snapshots: &HashMap<HostMachineId, ManagedHostStateSnapshot>,
    ) -> CarbideResult<()>;
}
