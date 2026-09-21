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

//! Storage for a direct-dispatch switch NVOS system-image update that is
//! staged behind an in-flight firmware-object job on the same switch.
//!
//! RMS serializes work per node, so a switch update covering both
//! firmware-object components and NVOS cannot submit both applies at once. The
//! backend submits the firmware-object job, records the system-image phase
//! here, and dispatches it from a later status poll once the firmware-object
//! job reaches a terminal state.
//!
//! Rows are keyed by BMC MAC, the switch's stable identity across ingestion,
//! matching [`crate::direct_dispatch_firmware_job`]. A switch has at most one
//! staged system-image update: a re-dispatch replaces it.
//!
//! The artifact access token is not stored. Callers keep it in memory and use
//! [`PendingSystemImageUpdate::requires_access_token`] to tell a staged update
//! that needs a token from one that downloads unauthenticated, so a poll after
//! a restart can fail the former explicitly rather than submit it without
//! credentials.

use mac_address::MacAddress;

use crate::DatabaseError;

/// A switch NVOS system-image update awaiting its firmware-object job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingSystemImageUpdate {
    /// Firmware-object JSON the staged apply will send to the backend.
    pub config_json: String,

    /// Whether the staged apply needs a caller-supplied artifact access token.
    /// `false` means it downloads unauthenticated and can be dispatched by any
    /// instance; `true` means only an instance still holding the token in
    /// memory can dispatch it.
    pub requires_access_token: bool,
}

/// Stage (or replace) the system-image update pending for `bmc_mac`.
pub async fn save(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
    pending: &PendingSystemImageUpdate,
) -> Result<(), DatabaseError> {
    let sql = "INSERT INTO switch_pending_system_image_updates \
                   (bmc_mac, config_json, requires_access_token) \
               VALUES ($1, $2, $3) \
               ON CONFLICT (bmc_mac) \
               DO UPDATE SET config_json = EXCLUDED.config_json, \
                             requires_access_token = EXCLUDED.requires_access_token, \
                             created = now()";
    sqlx::query(sql)
        .bind(bmc_mac)
        .bind(&pending.config_json)
        .bind(pending.requires_access_token)
        .execute(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(())
}

/// Fetch the system-image update staged for `bmc_mac`, if any.
pub async fn get(
    db: &sqlx::PgPool,
    bmc_mac: MacAddress,
) -> Result<Option<PendingSystemImageUpdate>, DatabaseError> {
    let sql = "SELECT config_json, requires_access_token \
               FROM switch_pending_system_image_updates WHERE bmc_mac = $1";
    let row: Option<(String, bool)> = sqlx::query_as(sql)
        .bind(bmc_mac)
        .fetch_optional(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(row.map(
        |(config_json, requires_access_token)| PendingSystemImageUpdate {
            config_json,
            requires_access_token,
        },
    ))
}

/// Drop the staged system-image update for `bmc_mac`, if present.
///
/// Called once the phase reaches a decision: dispatched, abandoned because the
/// firmware-object job failed, or abandoned because the access token did not
/// survive a restart. Deleting an absent row is a no-op.
pub async fn delete(db: &sqlx::PgPool, bmc_mac: MacAddress) -> Result<(), DatabaseError> {
    let sql = "DELETE FROM switch_pending_system_image_updates WHERE bmc_mac = $1";
    sqlx::query(sql)
        .bind(bmc_mac)
        .execute(db)
        .await
        .map_err(|e| DatabaseError::new(sql, e))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use mac_address::MacAddress;

    use super::{PendingSystemImageUpdate, delete, get, save};

    fn mac(last: u8) -> MacAddress {
        MacAddress::new([0x02, 0x00, 0x00, 0x00, 0x00, last])
    }

    fn pending(config_json: &str, requires_access_token: bool) -> PendingSystemImageUpdate {
        PendingSystemImageUpdate {
            config_json: config_json.to_string(),
            requires_access_token,
        }
    }

    #[crate::sqlx_test]
    async fn staged_update_round_trips_and_is_replaced_per_switch(pool: sqlx::PgPool) {
        // A switch with nothing staged has no pending update.
        assert_eq!(get(&pool, mac(1)).await.unwrap(), None);

        save(&pool, mac(1), &pending(r#"{"Id":"fw-a"}"#, true))
            .await
            .unwrap();
        assert_eq!(
            get(&pool, mac(1)).await.unwrap(),
            Some(pending(r#"{"Id":"fw-a"}"#, true))
        );

        // Re-dispatching to the same switch replaces the staged update rather
        // than leaving the superseded one behind.
        save(&pool, mac(1), &pending(r#"{"Id":"fw-b"}"#, false))
            .await
            .unwrap();
        assert_eq!(
            get(&pool, mac(1)).await.unwrap(),
            Some(pending(r#"{"Id":"fw-b"}"#, false))
        );

        // Staging is per switch: another switch is unaffected.
        assert_eq!(get(&pool, mac(2)).await.unwrap(), None);

        delete(&pool, mac(1)).await.unwrap();
        assert_eq!(get(&pool, mac(1)).await.unwrap(), None);

        // Deleting an absent row is a no-op, not an error.
        delete(&pool, mac(1)).await.unwrap();
    }
}
