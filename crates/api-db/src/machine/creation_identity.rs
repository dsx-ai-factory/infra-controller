// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Distinguish a machine row from a later ingestion of the same hardware ID.

use carbide_uuid::machine::MachineId;
use chrono::{DateTime, Utc};
use model::machine::ManagedHostState;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

/// `MachineCreationIdentity` captures a row's creation time or its last retained
/// state-history entry while absent. Check it after work outside a transaction
/// before creating or attaching resources to the machine.
/// Successful `machine::create` records state history; force cleanup retains
/// that history, and pruning keeps the latest entry. An absence check must run
/// under the caller's deletion fence, held through creation and commit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MachineCreationIdentity {
    created: Option<DateTime<Utc>>,
    last_history_id: Option<i64>,
    force_deleting: bool,
}

impl MachineCreationIdentity {
    /// `capture` identifies the current machine lifetime and deletion state.
    /// State history survives force cleanup, so an absent row also distinguishes
    /// creation and deletion that happen before the caller returns.
    pub async fn capture(
        database: impl DbReader<'_>,
        machine_id: &MachineId,
    ) -> DatabaseResult<Self> {
        let query = "SELECT
            (SELECT created FROM machines WHERE id = $1),
            (SELECT max(id) FROM machine_state_history WHERE object_id = $1),
            (SELECT controller_state = $2::jsonb FROM machines WHERE id = $1)";
        let (created, last_history_id, force_deleting): (
            Option<DateTime<Utc>>,
            Option<i64>,
            Option<bool>,
        ) = sqlx::query_as(query)
            .bind(machine_id)
            .bind(sqlx::types::Json(&ManagedHostState::ForceDeletion))
            .fetch_one(database)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        Ok(Self {
            created,
            last_history_id: if created.is_none() {
                last_history_id
            } else {
                None
            },
            force_deleting: force_deleting.unwrap_or(false),
        })
    }

    /// `check` rejects a deleted or replacement row, including a create/delete
    /// cycle while the captured machine was absent. This takes no row locks;
    /// callers retain their established interface-before-machine lock order.
    pub async fn check(
        &self,
        database: impl DbReader<'_>,
        machine_id: &MachineId,
    ) -> DatabaseResult<()> {
        let current = Self::capture(database, machine_id).await?;
        if current.force_deleting {
            return Err(DatabaseError::FailedPrecondition(format!(
                "machine {machine_id} is being force deleted",
            )));
        }
        if current != *self {
            return Err(DatabaseError::FailedPrecondition(format!(
                "machine {machine_id} lifetime changed during preparation",
            )));
        }
        Ok(())
    }

    /// `lock_force_deleting` locks the captured row for final force cleanup.
    /// Call after locking its interfaces. Returns false for a missing or
    /// replacement row, so delayed cleanup can finish without changing a later
    /// lifetime. The same original row without ForceDeletion remains an error.
    pub async fn lock_force_deleting(
        &self,
        txn: &mut sqlx::PgConnection,
        machine_id: &MachineId,
    ) -> DatabaseResult<bool> {
        let query =
            "SELECT created, controller_state = $2::jsonb FROM machines WHERE id = $1 FOR UPDATE";
        let current: Option<(DateTime<Utc>, bool)> = sqlx::query_as(query)
            .bind(machine_id)
            .bind(sqlx::types::Json(&ManagedHostState::ForceDeletion))
            .fetch_optional(txn)
            .await
            .map_err(|error| DatabaseError::query(query, error))?;
        let Some((created, force_deleting)) = current else {
            return Ok(false);
        };
        if Some(created) != self.created {
            return Ok(false);
        }
        if !force_deleting {
            return Err(DatabaseError::FailedPrecondition(format!(
                "machine {machine_id} is not marked for force deletion",
            )));
        }
        Ok(true)
    }

    /// `created` returns the captured row's immutable creation time, or `None`
    /// when preparation began before the machine was created.
    pub fn created(&self) -> Option<DateTime<Utc>> {
        self.created
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use model::machine::CURRENT_STATE_MODEL_VERSION;

    use super::*;

    /// Preparation must distinguish an unchanged absence from a completed
    /// create/delete cycle. Cleanup must accept only its original marked row,
    /// skip an obsolete lifetime, and preserve reingested hardware.
    #[crate::sqlx_test]
    async fn creation_identity_rejects_replacement_and_create_delete_while_absent(
        database: sqlx::PgPool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let machine_id =
            MachineId::from_str("fm100dskla0ihp0pn4tv7v1js2k2mo37sl0jjr8141okqg8pjpdpfihaa80")?;
        // An unchanged absence and an unchanged existing row are both valid
        // preparation identities. The later phases must reject only a change.
        let absent = MachineCreationIdentity::capture(&database, &machine_id).await?;
        absent.check(&database, &machine_id).await?;
        let mut txn = database.begin().await?;
        crate::machine::create(
            &mut txn,
            None,
            &machine_id,
            ManagedHostState::Created,
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        let original = MachineCreationIdentity::capture(txn.as_mut(), &machine_id).await?;
        txn.commit().await?;
        original.check(&database, &machine_id).await?;

        // The same row still requires the deletion marker. Mark it through
        // the real state write, then roll back to keep the next phase separate.
        let mut txn = database.begin().await?;
        assert!(matches!(
            original.lock_force_deleting(&mut txn, &machine_id).await,
            Err(DatabaseError::FailedPrecondition(_))
        ));
        txn.rollback().await?;
        let mut txn = database.begin().await?;
        let machine = crate::machine::find_one(
            txn.as_mut(),
            &machine_id,
            model::machine::machine_search_config::MachineSearchConfig::default(),
        )
        .await?
        .expect("original DPU");
        crate::machine::advance(&machine, &mut txn, &ManagedHostState::ForceDeletion, None).await?;
        assert!(original.lock_force_deleting(&mut txn, &machine_id).await?);
        assert!(matches!(
            original.check(txn.as_mut(), &machine_id).await,
            Err(DatabaseError::FailedPrecondition(_))
        ));
        txn.rollback().await?;

        // Delete the original row without erasing the history that records
        // its existence; otherwise a create/delete could look like no change.
        let mut txn = database.begin().await?;
        crate::machine::force_cleanup(&mut txn, &machine_id).await?;
        txn.commit().await?;
        // The machine is absent again, but create's retained history proves
        // that this is a different absence from the one preparation observed.
        assert!(matches!(
            absent.check(&database, &machine_id).await,
            Err(DatabaseError::FailedPrecondition(_))
        ));
        assert!(matches!(
            original.check(&database, &machine_id).await,
            Err(DatabaseError::FailedPrecondition(_))
        ));
        let mut txn = database.begin().await?;
        assert!(!original.lock_force_deleting(&mut txn, &machine_id).await?);
        txn.rollback().await?;

        // Reusing the hardware ID does not make either earlier snapshot
        // current: the replacement has its own creation time.
        let mut txn = database.begin().await?;
        crate::machine::create(
            &mut txn,
            None,
            &machine_id,
            ManagedHostState::Created,
            None,
            CURRENT_STATE_MODEL_VERSION,
        )
        .await?;
        txn.commit().await?;
        let replacement = MachineCreationIdentity::capture(&database, &machine_id).await?;
        replacement.check(&database, &machine_id).await?;
        assert!(matches!(
            original.check(&database, &machine_id).await,
            Err(DatabaseError::FailedPrecondition(_))
        ));
        assert!(matches!(
            absent.check(&database, &machine_id).await,
            Err(DatabaseError::FailedPrecondition(_))
        ));
        let mut txn = database.begin().await?;
        assert!(!original.lock_force_deleting(&mut txn, &machine_id).await?);
        txn.rollback().await?;
        Ok(())
    }
}
