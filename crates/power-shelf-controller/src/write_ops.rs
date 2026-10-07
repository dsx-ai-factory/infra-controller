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

//! Deferred database write operations for the power shelf state handler.

use async_trait::async_trait;
use carbide_uuid::power_shelf::PowerShelfId;
use health_report::HealthReport;
use sqlx::PgTransaction;
use state_controller::db_write_batch::WriteOp;
use state_controller::state_handler::StateHandlerError;

/// Persists a powershelf's health report to the db
pub struct PersistPowerShelfHealthHistory {
    pub power_shelf_id: PowerShelfId,
    pub health_report: HealthReport,
}

#[async_trait]
impl WriteOp for PersistPowerShelfHealthHistory {
    async fn apply<'a, 't: 'a>(
        self: Box<Self>,
        txn: &'a mut PgTransaction<'t>,
    ) -> Result<(), StateHandlerError> {
        db::health_history::persist(
            txn,
            db::health_history::HealthHistoryTableId::PowerShelf,
            &self.power_shelf_id,
            &self.health_report,
        )
        .await?;
        Ok(())
    }
}
