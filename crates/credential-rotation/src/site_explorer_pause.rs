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

//! Pause site-explorer probing of a BMC while its credentials are changed.
//!
//! Site-explorer authenticates BMCs directly and, on a 401, persists a sticky
//! `Unauthorized` latch (`AvoidLockout`) in `explored_endpoints` that blocks
//! allocations and never self-heals. Whenever a controller changes a BMC's
//! credential -- a credential rotation, or the factory-reset-then-restore run
//! during tenant release -- there is a window where the hardware carries one
//! password but Vault (or the just-issued reset) leaves another in play; a probe
//! in that window 401s and latches.
//!
//! To close that window, a controller records a
//! [`BmcSuppressionSubsystem::SiteExplorer`] suppression for the BMC MAC and
//! waits until site-explorer has *acknowledged* it before changing any
//! credential. Site-explorer stamps `acknowledged_at` only after it has claimed
//! the endpoint idle at the top of its single-flight iteration, so an
//! acknowledged suppression is an exact "site-explorer has observed this, no
//! probe is in flight, and every future probe skips this MAC" barrier. With the
//! probe held off, no latch can form, so nothing needs to be cleared on the
//! happy path; the controller simply deletes its suppression when it returns to
//! a steady state.
//!
//! This module is state-machine-neutral so the machine-, switch-, and
//! power-shelf-controllers' rotation flows and the machine-controller's
//! host-BMC factory-reset flow share one implementation. Each caller passes its
//! own [`BmcSuppressionSource`] so deletes stay scoped to the rows that caller
//! owns (a factory reset never removes a rotation's suppression, or vice versa).
//! The whole gate is re-derived from the suppression row each tick (its
//! `requested_at` is the wait-budget clock and its `acknowledged_at` is the
//! barrier), so callers need no new persisted sub-state.

use std::time::Duration;

use chrono::Utc;
use db::DatabaseError;
use mac_address::MacAddress;
use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem, NewBmcSuppression};
use sqlx::{PgConnection, PgPool};

/// How long a controller waits for site-explorer to acknowledge the suppression
/// before proceeding anyway.
///
/// The wait normally resolves within one site-explorer iteration. The budget is
/// a backstop for a disabled or unavailable site-explorer, which can never
/// acknowledge and also can never latch -- so proceeding after the budget is
/// safe. It is deliberately several site-explorer run intervals long so a busy
/// or briefly-contended explorer is never cut short.
pub const SITE_EXPLORER_PAUSE_BUDGET: Duration = Duration::from_secs(300);

/// Whether the caller may change credentials now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateDecision {
    /// Site-explorer has acknowledged the suppression (or the wait budget is
    /// exhausted): the caller may proceed with the rotation this tick.
    Proceed,
    /// Site-explorer has not yet acknowledged the suppression and the budget
    /// remains: the caller must wait and re-check on a later sweep.
    Wait,
}

/// Ensure every in-scope BMC MAC is suppressed for site-explorer under
/// `source` and report whether it is safe to change credentials this tick.
///
/// Idempotent: re-running each tick preserves this source's timestamps. Other
/// sources' rows are not overwritten. An empty `macs` is always
/// [`GateDecision::Proceed`].
pub async fn gate_before_credential_change(
    pool: &PgPool,
    macs: &[MacAddress],
    source: BmcSuppressionSource,
) -> Result<GateDecision, DatabaseError> {
    if macs.is_empty() {
        return Ok(GateDecision::Proceed);
    }

    let mut txn = pool
        .begin()
        .await
        .map_err(|e| DatabaseError::query("begin site-explorer pause gate transaction", e))?;
    for mac in macs {
        db::bmc_suppression::upsert(
            &mut txn,
            &NewBmcSuppression {
                bmc_mac_address: *mac,
                subsystem: BmcSuppressionSubsystem::SiteExplorer,
                source,
                reason: "site explorer pause".to_string(),
            },
        )
        .await?;
    }
    let rows = db::bmc_suppression::find_many(
        &mut *txn,
        macs,
        BmcSuppressionSubsystem::SiteExplorer,
        source,
    )
    .await?;
    txn.commit()
        .await
        .map_err(|e| DatabaseError::query("commit site-explorer pause gate transaction", e))?;

    let all_acknowledged = macs.iter().all(|mac| {
        rows.iter()
            .any(|row| row.bmc_mac_address == *mac && row.acknowledged_at.is_some())
    });
    if all_acknowledged {
        return Ok(GateDecision::Proceed);
    }

    // Escape hatch for a disabled/unavailable site-explorer that will never
    // acknowledge: proceed once every still-unacknowledged suppression has itself
    // outlived the pause budget. Measured as the *newest* unacknowledged
    // `requested_at` -- once that has aged past the budget, so has every older
    // one.
    let newest_unacknowledged = rows
        .iter()
        .filter(|row| row.acknowledged_at.is_none())
        .map(|row| row.requested_at)
        .max();
    if let Some(newest_unacknowledged) = newest_unacknowledged {
        let waited = Utc::now().signed_duration_since(newest_unacknowledged);
        let budget = chrono::Duration::from_std(SITE_EXPLORER_PAUSE_BUDGET)
            .expect("SITE_EXPLORER_PAUSE_BUDGET fits in chrono::Duration");
        if waited > budget {
            tracing::warn!(
                waited_secs = waited.num_seconds(),
                ?source,
                "proceeding with BMC credential change without site-explorer acknowledgement: \
                 pause budget exceeded (site-explorer disabled or unavailable?)"
            );
            return Ok(GateDecision::Proceed);
        }
    }

    Ok(GateDecision::Wait)
}

/// Delete the suppressions `source` created for `macs`, releasing site-explorer
/// to resume probing if no other source still holds a row.
///
/// The delete is issued on the caller's transaction so it commits atomically
/// with the state transition that ends the credential change.
pub async fn resume_after_credential_change(
    txn: &mut PgConnection,
    macs: &[MacAddress],
    source: BmcSuppressionSource,
) -> Result<(), DatabaseError> {
    if macs.is_empty() {
        return Ok(());
    }
    db::bmc_suppression::delete_many_for_source(
        txn,
        macs,
        BmcSuppressionSubsystem::SiteExplorer,
        source,
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use mac_address::MacAddress;
    use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem};
    use sqlx::PgPool;

    use super::{GateDecision, gate_before_credential_change};

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0, 0, 0, 0, last])
    }

    #[carbide_macros::sqlx_test]
    async fn an_empty_scope_proceeds(pool: PgPool) {
        assert_eq!(
            gate_before_credential_change(&pool, &[], BmcSuppressionSource::BmcCredentialRotation,)
                .await
                .unwrap(),
            GateDecision::Proceed
        );
    }

    #[carbide_macros::sqlx_test]
    async fn waits_until_every_mac_is_acknowledged(pool: PgPool) {
        let macs = [mac(1), mac(2)];

        // First pass records the suppressions; nothing is acknowledged yet.
        assert_eq!(
            gate_before_credential_change(
                &pool,
                &macs,
                BmcSuppressionSource::BmcCredentialRotation,
            )
            .await
            .unwrap(),
            GateDecision::Wait
        );

        // Acknowledging only one leaves the gate waiting.
        let mut txn = pool.begin().await.unwrap();
        assert!(
            db::bmc_suppression::acknowledge(
                &mut txn,
                mac(1),
                BmcSuppressionSubsystem::SiteExplorer
            )
            .await
            .unwrap()
        );
        txn.commit().await.unwrap();
        assert_eq!(
            gate_before_credential_change(
                &pool,
                &macs,
                BmcSuppressionSource::BmcCredentialRotation,
            )
            .await
            .unwrap(),
            GateDecision::Wait
        );

        // Once site-explorer has acknowledged both, the gate proceeds.
        let mut txn = pool.begin().await.unwrap();
        assert!(
            db::bmc_suppression::acknowledge(
                &mut txn,
                mac(2),
                BmcSuppressionSubsystem::SiteExplorer
            )
            .await
            .unwrap()
        );
        txn.commit().await.unwrap();
        assert_eq!(
            gate_before_credential_change(
                &pool,
                &macs,
                BmcSuppressionSource::BmcCredentialRotation,
            )
            .await
            .unwrap(),
            GateDecision::Proceed
        );
    }
}
