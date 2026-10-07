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

use carbide_kms_provider::{EncryptedDek, KmsBackend};
use carbide_uuid::secret::SecretId;
use db::work_lock_manager::{AcquireLockError, WorkLockManagerHandle};
use sqlx::PgPool;

use super::PgSecretsError;
use super::routing::SecretRouting;

// Replicas only contend when they name the same work, so keep this key stable
// across releases.
pub(super) const RE_WRAP_WORK_KEY: &str = "secrets::re_wrap_stale";

/// What a re-wrap pass did, in journal rows.
pub(crate) struct ReWrapStaleResult {
    /// Rows whose DEK was re-wrapped to the routed KEK.
    pub(crate) re_wrapped: u64,
    /// Rows already wrapped by the routed KEK.
    pub(crate) already_current: u64,
    /// Rows still wrapped by a KEK outside the routing config after the
    /// walk. Zero means every unrouted KEK can be retired; nonzero right
    /// after a run means concurrent writers landed rows mid-walk -- run
    /// re-wrap again once the fleet's config has converged.
    pub(crate) stale_remaining: u64,
}

/// A re-wrapped DEK waiting to be written back to its row.
struct PendingReWrap {
    secret_id: SecretId,
    wrapped: EncryptedDek,
    kek_id: String,
}

/// Re-wrap every journal row whose KEK no longer matches what routing
/// assigns its path -- the operator's one verb after rotating a key in
/// config: make the table agree with the config.
///
/// Only the DEK-wrapping columns change; the encrypted values are never
/// touched. The table is walked once in journal order, each batch's KMS
/// work happens before its write transaction opens (with Transit, those
/// are network calls that must not run while a transaction is held), and
/// batches commit independently, so an interrupted run keeps its progress.
/// Historical journal entries are re-wrapped too: they must stay
/// decryptable, and re-wrapping them is what lets an old KEK be retired
/// completely.
pub(crate) async fn re_wrap_stale(
    pool: &PgPool,
    work_lock_manager: &WorkLockManagerHandle,
    kms: &dyn KmsBackend,
    routing: &SecretRouting,
    batch_size: i64,
) -> Result<ReWrapStaleResult, PgSecretsError> {
    // Keep the full walk single-flight without reserving another pool
    // connection across KMS calls. A crashed owner can leave the key busy
    // until the manager's lease expires.
    let _work_lock = match work_lock_manager
        .try_acquire_lock(RE_WRAP_WORK_KEY.into())
        .await
    {
        Ok(lock) => lock,
        Err(AcquireLockError::WorkAlreadyLocked(_)) => {
            return Err(PgSecretsError::ReWrapInProgress);
        }
        Err(error) => return Err(error.into()),
    };

    let mut result = ReWrapStaleResult {
        re_wrapped: 0,
        already_current: 0,
        stale_remaining: 0,
    };

    let mut cursor: Option<i64> = None;
    loop {
        let batch = db::secrets::find_batch_after(pool, cursor, batch_size).await?;
        let Some(last) = batch.last() else {
            break;
        };
        cursor = Some(last.seq);

        // Unwrap and re-wrap stale DEKs first, against rows as read --
        // no transaction is open yet.
        let mut pending = Vec::new();
        for row in &batch {
            let target_kek = routing.active_kek_for_path(&row.path)?;
            if row.kek_id == target_kek {
                result.already_current += 1;
                continue;
            }

            let dek = kms
                .decrypt_dek(
                    &row.kek_id,
                    &EncryptedDek {
                        ciphertext: row.encrypted_dek.clone(),
                        nonce: row.dek_nonce.clone(),
                    },
                )
                .await?;
            let wrapped = kms.encrypt_dek(target_kek, &dek).await?;
            pending.push(PendingReWrap {
                secret_id: row.secret_id,
                wrapped,
                kek_id: target_kek.to_string(),
            });
        }

        if pending.is_empty() {
            continue;
        }

        // Then one short, write-only transaction per batch.
        let mut txn = pool
            .begin()
            .await
            .map_err(|e| PgSecretsError::Database(db::DatabaseError::acquire(e)))?;
        for rewrap in &pending {
            db::secrets::update_dek_wrap(
                &mut txn,
                rewrap.secret_id,
                &rewrap.wrapped.ciphertext,
                &rewrap.wrapped.nonce,
                &rewrap.kek_id,
            )
            .await?;
        }
        txn.commit().await.map_err(|e| {
            PgSecretsError::Database(db::DatabaseError::new("commit re-wrap batch", e))
        })?;
        result.re_wrapped += pending.len() as u64;
    }

    // Report what is still wrapped by KEKs outside the routing config --
    // the operator's retire-the-old-key signal.
    let routed: Vec<String> = routing
        .routes()
        .map(|(_, kek_id)| kek_id.to_string())
        .collect();
    result.stale_remaining = db::secrets::count_wrapped_outside(pool, &routed).await? as u64;

    Ok(result)
}
