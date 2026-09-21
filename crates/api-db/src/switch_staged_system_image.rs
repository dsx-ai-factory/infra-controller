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

//! Storage for the NVOS system-image phase of a direct-dispatch switch update,
//! held between the two submissions such an update has to be split into.
//!
//! RMS runs one job per node, so a switch update covering both firmware-object
//! components and NVOS cannot submit both applies at once. The backend submits
//! the firmware-object job, stages the system-image phase here, and dispatches
//! it from a later status poll once the firmware-object job is terminal.
//!
//! Rows are keyed by BMC MAC, the switch's stable identity across ingestion,
//! matching [`crate::direct_dispatch_firmware_job`]. A switch has at most one
//! staged phase: staging a new one replaces it.
//!
//! # Lifecycle
//!
//! Until the phase's RMS job id lands in
//! [`crate::direct_dispatch_firmware_job`], this row is the only record that
//! the phase is owed, so it outlives every step that could drop it:
//!
//! - [`stage`] writes it when the firmware-object job is submitted.
//! - [`claim`] leases it to one poll for the duration of an RMS submission.
//!   The lease, rather than a delete, is what keeps two overlapping polls from
//!   dispatching the same apply while still leaving something to recover if the
//!   poll holding it dies mid-dispatch.
//! - [`delete`] drops it only after the resulting job id is recorded elsewhere.
//! - [`fail`] retains it, holding the reason, when the phase ends without
//!   dispatching. Nothing else records that outcome, so without the row a later
//!   poll would see only the firmware-object job's success.
//!
//! # Access tokens
//!
//! The artifact access token is not stored. Callers keep it in memory and use
//! [`StagedUpdate::requires_access_token`] to tell a phase that needs a token
//! from one that downloads unauthenticated, so a poll after a restart can fail
//! the former explicitly rather than submit it without credentials.

use std::time::Duration;

use mac_address::MacAddress;

use crate::DatabaseError;

/// A switch NVOS system-image apply waiting on its firmware-object job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedUpdate {
    /// Firmware-object JSON the apply will send to the backend.
    pub config_json: String,

    /// Whether the apply needs a caller-supplied artifact access token.
    /// `false` means it downloads unauthenticated and any instance can
    /// dispatch it; `true` means only an instance still holding the token in
    /// memory can.
    pub requires_access_token: bool,
}

/// What a switch's staged system-image phase currently holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedPhase {
    /// Still owed. Take it with [`claim`] before submitting it.
    Staged(StagedUpdate),

    /// Ended without dispatching, for this reason.
    Failed(String),
}

/// Stage (or replace) the system-image phase owed by `bmc_mac`.
///
/// Clears any lease or failure left by a superseded update, so the switch is
/// left owing exactly this phase.
pub async fn stage(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    update: &StagedUpdate,
) -> Result<(), DatabaseError> {
    let sql = "INSERT INTO switch_staged_system_image_updates \
                   (bmc_mac, config_json, requires_access_token) \
               VALUES ($1, $2, $3) \
               ON CONFLICT (bmc_mac) \
               DO UPDATE SET config_json = EXCLUDED.config_json, \
                             requires_access_token = EXCLUDED.requires_access_token, \
                             claimed = NULL, \
                             failure = NULL, \
                             created = now()";
    sqlx::query(sql)
        .bind(bmc_mac)
        .bind(&update.config_json)
        .bind(update.requires_access_token)
        .execute(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(())
}

/// Fetch the system-image phase staged for `bmc_mac`, if any.
///
/// A [`StagedPhase::Staged`] result says only that the phase is still owed; it
/// may already be leased to another poll. Use [`claim`] to take it.
pub async fn get(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
) -> Result<Option<StagedPhase>, DatabaseError> {
    let sql = "SELECT config_json, requires_access_token, failure \
               FROM switch_staged_system_image_updates WHERE bmc_mac = $1";
    let row: Option<(String, bool, Option<String>)> = sqlx::query_as(sql)
        .bind(bmc_mac)
        .fetch_optional(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(row.map(
        |(config_json, requires_access_token, failure)| match failure {
            Some(failure) => StagedPhase::Failed(failure),
            None => StagedPhase::Staged(StagedUpdate {
                config_json,
                requires_access_token,
            }),
        },
    ))
}

/// Take the dispatch lease on the phase staged for `bmc_mac`, returning it.
///
/// Atomic: of several callers racing on one switch, exactly one is handed the
/// phase and the rest get `None`, so an apply is submitted once. `None` also
/// covers a switch with nothing staged and one whose phase already failed.
///
/// `lease` bounds how long the holder may take. A caller that dies mid-dispatch
/// never releases the lease, so once it is older than `lease` another caller
/// may take it over; pick a `lease` that comfortably exceeds one submission.
pub async fn claim(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    lease: Duration,
) -> Result<Option<StagedUpdate>, DatabaseError> {
    let sql = "UPDATE switch_staged_system_image_updates SET claimed = now() \
               WHERE bmc_mac = $1 AND failure IS NULL \
                 AND (claimed IS NULL OR claimed < now() - ($2::bigint * INTERVAL '1 second')) \
               RETURNING config_json, requires_access_token";
    let row: Option<(String, bool)> = sqlx::query_as(sql)
        .bind(bmc_mac)
        .bind(i64::try_from(lease.as_secs()).unwrap_or(i64::MAX))
        .fetch_optional(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(
        row.map(|(config_json, requires_access_token)| StagedUpdate {
            config_json,
            requires_access_token,
        }),
    )
}

/// Record that the phase staged for `bmc_mac` ended without dispatching.
///
/// The row is kept, holding `failure` and releasing any lease, because it is
/// the only record of the outcome: there is no RMS job id for a later status
/// read to poll in its place. The failure stands until a new update stages over
/// it. Recording a failure for an absent switch is a no-op.
pub async fn fail(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    failure: &str,
) -> Result<(), DatabaseError> {
    let sql = "UPDATE switch_staged_system_image_updates SET failure = $2, claimed = NULL \
               WHERE bmc_mac = $1";
    sqlx::query(sql)
        .bind(bmc_mac)
        .bind(failure)
        .execute(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(())
}

/// Drop the staged system-image phase for `bmc_mac`, if present.
///
/// Only safe once the phase is recorded somewhere else: after its RMS job id is
/// persisted, or when a new update supersedes it. Deleting an absent row is a
/// no-op.
pub async fn delete(db: &sqlx::PgPool, bmc_mac: MacAddress) -> Result<(), DatabaseError> {
    let sql = "DELETE FROM switch_staged_system_image_updates WHERE bmc_mac = $1";
    sqlx::query(sql)
        .bind(bmc_mac)
        .execute(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use mac_address::MacAddress;

    use super::{StagedPhase, StagedUpdate, claim, delete, fail, get, stage};

    const LEASE: Duration = Duration::from_secs(300);

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, last])
    }

    fn update(config_json: &str, requires_access_token: bool) -> StagedUpdate {
        StagedUpdate {
            config_json: config_json.to_string(),
            requires_access_token,
        }
    }

    #[crate::sqlx_test]
    async fn staged_phase_round_trips_and_is_replaced_per_switch(pool: sqlx::PgPool) {
        // A switch with nothing staged owes no phase.
        assert_eq!(get(&pool, mac(1)).await.unwrap(), None);

        stage(&pool, mac(1), &update(r#"{"Id":"fw-a"}"#, true))
            .await
            .unwrap();
        assert_eq!(
            get(&pool, mac(1)).await.unwrap(),
            Some(StagedPhase::Staged(update(r#"{"Id":"fw-a"}"#, true)))
        );

        // Re-dispatching to the same switch replaces the staged phase rather
        // than leaving the superseded one behind.
        stage(&pool, mac(1), &update(r#"{"Id":"fw-b"}"#, false))
            .await
            .unwrap();
        assert_eq!(
            get(&pool, mac(1)).await.unwrap(),
            Some(StagedPhase::Staged(update(r#"{"Id":"fw-b"}"#, false)))
        );

        // Staging is per switch: another switch is unaffected.
        assert_eq!(get(&pool, mac(2)).await.unwrap(), None);

        delete(&pool, mac(1)).await.unwrap();
        assert_eq!(get(&pool, mac(1)).await.unwrap(), None);

        // Deleting an absent row is a no-op, not an error.
        delete(&pool, mac(1)).await.unwrap();
    }

    // The lease is what stops two overlapping status polls from submitting one
    // switch's apply twice, and what stops a poll killed mid-dispatch from
    // stranding it forever.
    #[crate::sqlx_test]
    async fn claim_is_exclusive_until_the_lease_expires(pool: sqlx::PgPool) {
        stage(&pool, mac(1), &update(r#"{"Id":"fw-a"}"#, true))
            .await
            .unwrap();

        assert_eq!(
            claim(&pool, mac(1), LEASE).await.unwrap(),
            Some(update(r#"{"Id":"fw-a"}"#, true))
        );

        // A second caller is turned away while the first holds the lease, and
        // the phase stays staged for whoever holds it.
        assert_eq!(claim(&pool, mac(1), LEASE).await.unwrap(), None);
        assert_eq!(
            get(&pool, mac(1)).await.unwrap(),
            Some(StagedPhase::Staged(update(r#"{"Id":"fw-a"}"#, true)))
        );

        // Once the lease is older than its duration, the phase is takeable
        // again rather than stranded by the holder that never released it.
        assert_eq!(
            claim(&pool, mac(1), Duration::ZERO).await.unwrap(),
            Some(update(r#"{"Id":"fw-a"}"#, true))
        );

        // Claiming a switch that owes nothing is not an error.
        assert_eq!(claim(&pool, mac(2), LEASE).await.unwrap(), None);
    }

    // A phase that ends without dispatching has no RMS job id behind it, so the
    // row has to keep reporting the failure until a new update supersedes it.
    #[crate::sqlx_test]
    async fn failure_is_retained_and_not_reclaimable(pool: sqlx::PgPool) {
        stage(&pool, mac(1), &update(r#"{"Id":"fw-a"}"#, true))
            .await
            .unwrap();
        claim(&pool, mac(1), LEASE).await.unwrap();

        fail(&pool, mac(1), "firmware-object update did not complete")
            .await
            .unwrap();
        assert_eq!(
            get(&pool, mac(1)).await.unwrap(),
            Some(StagedPhase::Failed(
                "firmware-object update did not complete".to_string()
            ))
        );

        // A failed phase is terminal: it is never dispatched, even though the
        // lease it was holding is released.
        assert_eq!(claim(&pool, mac(1), Duration::ZERO).await.unwrap(), None);

        // Staging a new update clears the failure.
        stage(&pool, mac(1), &update(r#"{"Id":"fw-b"}"#, false))
            .await
            .unwrap();
        assert_eq!(
            get(&pool, mac(1)).await.unwrap(),
            Some(StagedPhase::Staged(update(r#"{"Id":"fw-b"}"#, false)))
        );
    }
}
