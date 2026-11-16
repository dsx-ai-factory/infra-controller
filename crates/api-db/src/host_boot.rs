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

//! Persistence and compare-and-swap boundaries for Core-owned virtual media.

use mac_address::MacAddress;
use model::host_boot::HostBootIntent;
use sqlx::types::Json;
use sqlx::{FromRow, PgConnection};
use uuid::Uuid;

use crate::db_read::DbReader;
use crate::{DatabaseError, DatabaseResult};

#[derive(FromRow)]
struct IntentRow {
    intent: Json<HostBootIntent>,
    revision: i64,
}

impl IntentRow {
    fn decode(self) -> HostBootIntent {
        let mut intent = self.intent.0;
        intent.revision = self.revision;
        intent
    }
}

/// Load the current intent for one registered expected machine.
pub async fn find(db: impl DbReader<'_>, id: Uuid) -> DatabaseResult<Option<HostBootIntent>> {
    let query = "SELECT intent, revision FROM host_boot_intents WHERE expected_machine_id=$1";
    sqlx::query_as::<_, IntentRow>(query)
        .bind(id)
        .fetch_optional(db)
        .await
        .map(|row| row.map(IntentRow::decode))
        .map_err(|error| DatabaseError::query(query, error))
}

/// Find the opt-in policy for a BMC before or after discovered-machine creation.
pub async fn find_by_bmc_mac(
    db: impl DbReader<'_>,
    mac: MacAddress,
) -> DatabaseResult<Option<HostBootIntent>> {
    let query = "SELECT intent, revision FROM host_boot_intents WHERE bmc_mac_address=$1";
    sqlx::query_as::<_, IntentRow>(query)
        .bind(mac)
        .fetch_optional(db)
        .await
        .map(|row| row.map(IntentRow::decode))
        .map_err(|error| DatabaseError::query(query, error))
}

/// Find an attempt when serving a request from its registered boot interface.
pub async fn find_by_boot_mac(
    db: impl DbReader<'_>,
    mac: MacAddress,
) -> DatabaseResult<Option<HostBootIntent>> {
    let query = "SELECT intent, revision FROM host_boot_intents WHERE boot_mac_address=$1";
    sqlx::query_as::<_, IntentRow>(query)
        .bind(mac)
        .fetch_optional(db)
        .await
        .map(|row| row.map(IntentRow::decode))
        .map_err(|error| DatabaseError::query(query, error))
}

/// Return in-progress attempts; completed policies remain available to lifecycle hooks.
pub async fn list_active(db: impl DbReader<'_>) -> DatabaseResult<Vec<HostBootIntent>> {
    let query = "SELECT intent, revision FROM host_boot_intents WHERE intent->>'phase' NOT IN ('completed','cancelled') AND (intent->>'phase' <> 'failed' OR intent->>'cancelled' = 'true') ORDER BY expected_machine_id";
    sqlx::query_as::<_, IntentRow>(query)
        .fetch_all(db)
        .await
        .map(|rows| rows.into_iter().map(IntentRow::decode).collect())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Insert one request after the caller has locked and validated its expected machine.
pub async fn create(txn: &mut PgConnection, intent: &HostBootIntent) -> DatabaseResult<()> {
    let query = "INSERT INTO host_boot_intents (expected_machine_id,bmc_mac_address,boot_mac_address,intent,revision) VALUES ($1,$2,$3,$4,0)";
    sqlx::query(query)
        .bind(intent.expected_machine_id)
        .bind(intent.bmc_mac_address)
        .bind(intent.boot_mac_address)
        .bind(Json(intent))
        .execute(txn)
        .await
        .map(|_| ())
        .map_err(|error| DatabaseError::query(query, error))
}

/// Save progress only if no API or controller changed the loaded revision.
///
/// `false` means another writer won. No external operation may be dispatched on
/// a failed compare-and-swap. Reload before deciding what can be done next.
pub async fn update(txn: &mut PgConnection, intent: &HostBootIntent) -> DatabaseResult<bool> {
    let query = "UPDATE host_boot_intents SET intent=$3, bmc_mac_address=$4, boot_mac_address=$5, revision=revision+1 WHERE expected_machine_id=$1 AND revision=$2";
    sqlx::query(query)
        .bind(intent.expected_machine_id)
        .bind(intent.revision)
        .bind(Json(intent))
        .bind(intent.bmc_mac_address)
        .bind(intent.boot_mac_address)
        .execute(txn)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|error| DatabaseError::query(query, error))
}

/// Release a fully cleaned policy only if no lifecycle queued another attempt.
pub async fn delete_cleaned(
    txn: &mut PgConnection,
    intent: &HostBootIntent,
) -> DatabaseResult<bool> {
    let query = "DELETE FROM host_boot_intents WHERE expected_machine_id=$1 AND revision=$2 AND intent->>'phase' IN ('completed','cancelled') AND intent->>'media_uri' IS NULL AND intent->>'previous_boot' IS NULL";
    sqlx::query(query)
        .bind(intent.expected_machine_id)
        .bind(intent.revision)
        .execute(txn)
        .await
        .map(|result| result.rows_affected() == 1)
        .map_err(|error| DatabaseError::query(query, error))
}

/// Resolve an already registered BMC endpoint without requiring a new allocation policy.
///
/// An explicitly configured BMC address wins. Otherwise a single known IPv4
/// interface address is preferred, followed by a single IPv6 address. Ambiguous
/// registrations must be corrected rather than choosing an arbitrary endpoint.
pub async fn resolve_bmc_address(
    db: impl DbReader<'_>,
    expected: &model::expected_machine::ExpectedMachine,
) -> DatabaseResult<Option<std::net::IpAddr>> {
    if let Some(ip) = expected
        .data
        .bmc_ip_address
        .or(expected.effective_host_bmc().fixed_ip)
    {
        return Ok(Some(ip));
    }
    let interfaces =
        crate::machine_interface::find_by_mac_address(db, expected.bmc_mac_address).await?;
    let addresses: std::collections::BTreeSet<_> = interfaces
        .into_iter()
        .filter(|interface| {
            interface.interface_type == model::machine_interface::InterfaceType::Bmc
        })
        .flat_map(|interface| interface.addresses)
        .filter(|address| !address.is_unspecified() && !address.is_multicast())
        .collect();
    let ipv4: Vec<_> = addresses
        .iter()
        .filter(|address| address.is_ipv4())
        .copied()
        .collect();
    match (ipv4.as_slice(), addresses.len()) {
        ([ip], _) => Ok(Some(*ip)),
        ([], 0) => Ok(None),
        ([], 1) => Ok(addresses.into_iter().next()),
        _ => Err(DatabaseError::FailedPrecondition(
            "registered BMC has ambiguous addresses; configure bmc_ip_address".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use model::host_boot::{HostBootPhase, HostBootPurpose};

    use super::*;

    #[crate::sqlx_test]
    async fn cancellation_and_release_fence_stale_boot_writers(pool: sqlx::PgPool) {
        let id = Uuid::new_v4();
        sqlx::query("INSERT INTO expected_machines (id,serial_number,bmc_mac_address,bmc_username,bmc_password) VALUES ($1,'BOOTTEST01','02:00:00:00:00:01','test','test')")
            .bind(id).execute(&pool).await.unwrap();
        let row = HostBootIntent {
            expected_machine_id: id,
            bmc_mac_address: "02:00:00:00:00:01".parse().unwrap(),
            boot_mac_address: "02:00:00:00:00:02".parse().unwrap(),
            fixed_ip_address: "192.0.2.10".parse().unwrap(),
            generation: Uuid::new_v4(),
            requested_purpose: HostBootPurpose::Discovery,
            purpose: HostBootPurpose::Discovery,
            artifact_url: "http://example.test/boot.iso".into(),
            artifact_sha256: "a".repeat(64),
            phase: HostBootPhase::Pending,
            media_uri: None,
            previous_boot: None,
            acknowledgement_token: Uuid::new_v4(),
            acknowledged: false,
            cancelled: false,
            phase_started_at: None,
            reset_attempted_at: None,
            last_error: None,
            failed_from_phase: None,
            lifecycle_key: None,
            revision: 0,
        };
        let mut connection = pool.acquire().await.unwrap();
        create(&mut connection, &row).await.unwrap();
        let mut forged_cleanup = row.clone();
        forged_cleanup.phase = HostBootPhase::Completed;
        assert!(
            !delete_cleaned(&mut connection, &forged_cleanup)
                .await
                .unwrap(),
            "stored in-progress phase must block release even if the caller is stale"
        );

        let stale_worker = find(&mut *connection, id).await.unwrap().unwrap();
        let mut cancellation = stale_worker.clone();
        cancellation.cancelled = true;
        cancellation.phase = HostBootPhase::Detaching;
        assert!(update(&mut connection, &cancellation).await.unwrap());
        assert!(
            !update(&mut connection, &stale_worker).await.unwrap(),
            "a stale worker cannot overwrite cancellation"
        );
        let mut recovery = find(&mut *connection, id).await.unwrap().unwrap();
        recovery.phase = HostBootPhase::Failed;
        recovery.failed_from_phase = Some(HostBootPhase::Mounting);
        assert!(update(&mut connection, &recovery).await.unwrap());
        assert_eq!(
            list_active(&mut *connection).await.unwrap().len(),
            1,
            "explicit cancellation must make a failed policy observable for cleanup"
        );
        let mut cleaned = find(&mut *connection, id).await.unwrap().unwrap();
        assert!(cleaned.cancelled);
        let stale_release = cleaned.clone();
        cleaned.phase = HostBootPhase::Cancelled;
        assert!(update(&mut connection, &cleaned).await.unwrap());
        assert!(
            !delete_cleaned(&mut connection, &stale_release)
                .await
                .unwrap(),
            "release must use current revision"
        );
        let current = find(&mut *connection, id).await.unwrap().unwrap();
        assert!(delete_cleaned(&mut connection, &current).await.unwrap());
        assert!(
            !update(&mut connection, &current).await.unwrap(),
            "a stale controller cannot recreate a released policy"
        );
        assert!(find(&mut *connection, id).await.unwrap().is_none());
    }
}
