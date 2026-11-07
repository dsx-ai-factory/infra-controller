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

//! Deferred database write operations for the switch state handler.

use async_trait::async_trait;
use carbide_uuid::switch::SwitchId;
use health_report::HealthReport;
use sqlx::PgTransaction;
use state_controller::db_write_batch::WriteOp;
use state_controller::state_handler::StateHandlerError;

/// Persists a snapshot of a switch's aggregate health into `switch_health_history`.
///
/// The write is deferred to the end of the state-handler iteration via the
/// controller's `DbWriteBatch`; [`db::health_history::persist`] deduplicates
/// consecutive identical observations by content hash.
pub struct PersistSwitchHealthHistory {
    pub switch_id: SwitchId,
    pub health_report: HealthReport,
}

#[async_trait]
impl WriteOp for PersistSwitchHealthHistory {
    async fn apply<'a, 't: 'a>(
        self: Box<Self>,
        txn: &'a mut PgTransaction<'t>,
    ) -> Result<(), StateHandlerError> {
        db::health_history::persist(
            txn,
            db::health_history::HealthHistoryTableId::Switch,
            &self.switch_id,
            &self.health_report,
        )
        .await?;
        Ok(())
    }
}
