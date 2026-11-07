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

//! One-time import of Vault secrets into the Postgres journal. `run()`
//! drives this at startup: with `[secrets]` configured, the import either
//! completes (recording a permanent marker) before the process serves
//! traffic, or the process does not start. Vault is never part of the
//! credential chain in this mode.

use carbide_kms_provider::KmsBackend;
use carbide_secrets::credentials::Credentials;
use db::work_lock_manager::WorkLock;
use sqlx::PgPool;
use zeroize::Zeroizing;

use super::routing::SecretRouting;
use super::{ImportApproach, ImportResult, PgSecretsError, VAULT_IMPORT_MARKER_PATH};

/// Import pre-read Vault secrets into Postgres and record completion.
///
/// With `MissingOnly`, every secret is prepared before the transaction, then
/// paths that already have entries are skipped under the `secrets` table lock.
/// With `All`, every secret appends a new journal entry unconditionally.
///
/// Vault and KMS work finishes before the transaction begins. The transaction
/// first takes the marker advisory lock used by the legacy importer, then
/// fences `work_lock`, writes the complete snapshot, and records the permanent
/// marker atomically. The table lock keeps lock-table use constant even when a
/// Vault snapshot has thousands of paths; one query finds existing paths and
/// the selected entries are inserted in bind-limit-sized chunks. Legacy
/// importers serialize with this batch while their session lock remains live,
/// and a worker whose lease was taken over can commit neither stale credentials
/// nor a completion marker.
pub async fn import_vault_secrets(
    pool: &PgPool,
    work_lock: &WorkLock,
    routing: &SecretRouting,
    kms: &dyn KmsBackend,
    secrets: &[(String, Credentials)],
    approach: ImportApproach,
) -> Result<ImportResult, PgSecretsError> {
    let mut skipped = 0;
    let mut prepared_secrets = Vec::with_capacity(secrets.len());

    for (path, credentials) in secrets {
        let json_bytes = Zeroizing::new(serde_json::to_vec(credentials)?);
        let envelope = super::encrypt_envelope(routing, kms, path, &json_bytes).await?;
        prepared_secrets.push((path, envelope));
    }

    let marker_envelope =
        super::encrypt_envelope(routing, kms, VAULT_IMPORT_MARKER_PATH, b"completed").await?;
    let mut txn = pool
        .begin()
        .await
        .map_err(|e| PgSecretsError::Database(db::DatabaseError::acquire(e)))?;

    // This is the same advisory key used by the old session-lock importer.
    // Take it before the table lock so an old importer can finish its writes,
    // then recheck its marker before this batch changes anything.
    db::secrets::lock_path_advisory(&mut txn, VAULT_IMPORT_MARKER_PATH).await?;
    if db::secrets::exists(&mut *txn, VAULT_IMPORT_MARKER_PATH).await? {
        txn.rollback().await.map_err(|e| {
            PgSecretsError::Database(db::DatabaseError::new(
                "roll back completed vault import",
                e,
            ))
        })?;
        return Ok(ImportResult::AlreadyComplete);
    }

    work_lock.fence_transaction(&mut txn).await?;
    db::secrets::lock_for_bulk_write(&mut txn).await?;

    let mut occupied_paths = match approach {
        ImportApproach::MissingOnly => {
            let paths = prepared_secrets
                .iter()
                .map(|(path, _)| path.as_str())
                .collect::<Vec<_>>();
            db::secrets::find_existing_paths(&mut *txn, &paths).await?
        }
        ImportApproach::All => Default::default(),
    };
    let mut entries = Vec::with_capacity(prepared_secrets.len() + 1);
    for (path, envelope) in &prepared_secrets {
        if matches!(approach, ImportApproach::MissingOnly)
            && !occupied_paths.insert((*path).clone())
        {
            skipped += 1;
            continue;
        }
        entries.push(envelope.as_new_entry(path));
    }

    let imported = entries.len() as u64;
    entries.push(marker_envelope.as_new_entry(VAULT_IMPORT_MARKER_PATH));
    db::secrets::insert_many(&mut txn, &entries).await?;
    txn.commit()
        .await
        .map_err(|e| PgSecretsError::Database(db::DatabaseError::new("commit vault import", e)))?;

    Ok(ImportResult::Completed { imported, skipped })
}

/// Whether the vault import has already completed (the marker secret
/// exists).
pub async fn is_vault_import_complete(pool: &PgPool) -> Result<bool, PgSecretsError> {
    Ok(db::secrets::exists(pool, VAULT_IMPORT_MARKER_PATH).await?)
}
