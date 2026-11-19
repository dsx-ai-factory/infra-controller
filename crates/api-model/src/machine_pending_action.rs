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

use carbide_uuid::machine::{HostMachineId, MachineId};
use chrono::{DateTime, Utc};
use config_version::Versioned;
use sqlx::Row;

use crate::machine::ManagedHostState;

/// Work carbide still owes an external system for one machine.
///
/// Mirrors the `machine_pending_action_kind` Postgres enum
/// (`20260810150847_machine_pending_actions.sql`).
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(type_name = "machine_pending_action_kind", rename_all = "snake_case")]
pub enum MachinePendingActionKind {
    /// DPF has staged a DPUService update and parked the DPU in the NodeEffect
    /// phase, waiting for carbide to release the maintenance hold.
    DpuServiceSync,
}

/// Who completed a pending action.
///
/// Mirrors the `machine_pending_action_actor` Postgres enum
/// (`20260814102046_machine_pending_action_completed_by.sql`) and the
/// `UpdateInitiator` RPC enum, which draws the same distinction for
/// reprovisioning.
#[derive(Clone, Copy, Debug, Eq, PartialEq, sqlx::Type)]
#[sqlx(type_name = "machine_pending_action_actor", rename_all = "snake_case")]
pub enum MachinePendingActionActor {
    /// Carbide, having confirmed on its own that the work was safe to do.
    Automatic,
    /// An operator, through the admin API.
    AdminCli,
}

/// One requested action for one machine, outstanding or already completed.
///
/// There is no payload beyond the timing: a `None` `completed_at` means the work
/// is still owed. `requested_at` is the first time the action was requested and
/// survives re-requests, so it measures how long the machine waited rather than
/// when the request was last observed.
/// The table's internal `id` is used for history ordering, not returned in this record.
#[derive(Clone, Debug, Eq, PartialEq, sqlx::FromRow, carbide_macros::DbTable)]
#[db_table(name = "machine_pending_actions")]
pub struct MachinePendingAction {
    pub machine_id: MachineId,
    pub kind: MachinePendingActionKind,
    pub requested_at: DateTime<Utc>,
    /// When the work succeeded, or `None` while it is still owed.
    pub completed_at: Option<DateTime<Utc>>,
    /// Who completed it, set together with [`Self::completed_at`].
    pub completed_by: Option<MachinePendingActionActor>,
}

impl MachinePendingAction {
    /// Whether this action is still owed.
    pub fn is_outstanding(&self) -> bool {
        self.completed_at.is_none()
    }
}

/// A host with an outstanding action, with the host facts a fleet-wide summary
/// groups by.
#[derive(Clone, Debug)]
pub struct PendingActionHost {
    pub host_id: HostMachineId,
    /// See [`MachinePendingAction::requested_at`].
    pub requested_at: DateTime<Utc>,
    /// The host's managed state; its version dates the entry into that state.
    pub state: Versioned<ManagedHostState>,
    /// Whether an instance is allocated on the host.
    pub has_instance: bool,
}

/// Names the host whose stored state could not be decoded. One bad row fails
/// the whole read, so the error has to say which host to repair.
#[derive(Debug, thiserror::Error)]
#[error("machine {host_id}: {source}")]
struct HostStateDecodeError {
    host_id: HostMachineId,
    source: sqlx::error::BoxDynError,
}

impl<'r> sqlx::FromRow<'r, sqlx::postgres::PgRow> for PendingActionHost {
    fn from_row(row: &'r sqlx::postgres::PgRow) -> Result<Self, sqlx::Error> {
        // Read first, so that a state that cannot be decoded can name its host.
        let host_id: HostMachineId = row.try_get("machine_id")?;
        let naming_host = |error: sqlx::Error| match error {
            sqlx::Error::ColumnDecode { index, source } => sqlx::Error::ColumnDecode {
                index,
                source: Box::new(HostStateDecodeError { host_id, source }),
            },
            other => other,
        };
        let state: sqlx::types::Json<ManagedHostState> =
            row.try_get("controller_state").map_err(naming_host)?;
        let state_version = row
            .try_get("controller_state_version")
            .map_err(naming_host)?;
        Ok(Self {
            host_id,
            requested_at: row.try_get("requested_at")?,
            state: Versioned::new(state.0, state_version),
            has_instance: row.try_get("has_instance")?,
        })
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use carbide_uuid::machine::{MachineId, MachineIdSource, MachineType};

    use super::PendingActionHost;
    use crate::machine::ManagedHostState;

    /// The row shape `db::machine_pending_action::find_outstanding_hosts`
    /// selects, with the two state columns supplied by the case.
    const ROW: &str = "SELECT
        $1::varchar AS machine_id,
        now() AS requested_at,
        $2::jsonb AS controller_state,
        $3::varchar AS controller_state_version,
        false AS has_instance";

    #[crate::sqlx_test]
    async fn a_state_that_cannot_be_decoded_names_its_host(pool: sqlx::PgPool) {
        let host_id = MachineId::new(
            MachineIdSource::ProductBoardChassisSerial,
            [7u8; 32],
            MachineType::Host,
        );
        let ready = serde_json::to_value(ManagedHostState::Ready).unwrap();
        let current_version = config_version::ConfigVersion::initial().version_string();
        let named = Some(format!("machine {host_id}"));

        // Input: the stored state and state version of the row.
        // Expect: the column that failed, the host the error names, and
        // whether the cause is kept as its source.
        check_cases_async(
            [
                Case {
                    scenario: "a controller state that is not a state",
                    input: (serde_json::json!({}), current_version),
                    expect: Yields(("\"controller_state\"".to_string(), named.clone(), true)),
                },
                Case {
                    scenario: "a controller state version that is not a version",
                    input: (ready, "not a version".to_string()),
                    expect: Yields((
                        "\"controller_state_version\"".to_string(),
                        named.clone(),
                        true,
                    )),
                },
            ],
            |(state, version)| {
                let pool = pool.clone();
                async move {
                    let row = sqlx::query_as::<_, PendingActionHost>(ROW)
                        .bind(host_id)
                        .bind(sqlx::types::Json(state))
                        .bind(version)
                        .fetch_one(&pool)
                        .await;
                    match row {
                        Err(sqlx::Error::ColumnDecode { index, source }) => Ok((
                            index,
                            source
                                .to_string()
                                .split_once(": ")
                                .map(|(named, _)| named.to_string()),
                            source.source().is_some(),
                        )),
                        other => Err(format!("expected a column decode error, got {other:?}")),
                    }
                }
            },
        )
        .await;
    }
}
