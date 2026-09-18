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

//! Credential storage in Postgres, replacing Vault KV.
//!
//! Values are envelope-encrypted: every write generates a new data
//! encryption key (DEK), encrypts the credential with it, and asks the KMS
//! backend to wrap the DEK under a key encryption key (KEK). The row
//! records which KEK wrapped it, so reads never consult routing -- routing
//! ([`SecretRouting`]) only decides which KEK new writes use, and the
//! ciphertext is bound to its path so a row copied onto another path will
//! not decrypt.
//!
//! Reads keep two behaviors the rest of the system learned from the Vault
//! reader: the newest journal entry wins (ordered by the database-assigned
//! `seq`), and an entry whose password is empty reads as no credential at
//! all -- several delete flows "delete" by writing an empty-password
//! tombstone.

use std::sync::Arc;

use async_trait::async_trait;
use carbide_kms_provider::{EncryptedDek, KmsBackend};
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{
    CredentialKey, CredentialManager, CredentialReader, CredentialWriter, Credentials,
};
use db::ConditionalWrite;
use db::secrets::{NewSecretEntry, SecretAlreadyExists};
use model::secrets::SecretRow;
use serde::Deserialize;
use sqlx::PgPool;
use zeroize::Zeroizing;

mod import;
mod metrics;
mod re_wrap;
mod routing;
#[cfg(test)]
mod tests;

pub use import::{import_vault_secrets, is_vault_import_complete};
pub use metrics::{OperationTimer, SecretsOperation};
pub(crate) use re_wrap::re_wrap_stale;
pub use routing::SecretRouting;

/// The KMS and routing handles that secrets admin operations (re-wrap)
/// need. None on the `Api` when the `[secrets]` config section is absent.
pub struct SecretsContext {
    pub routing: SecretRouting,
    pub kms: Arc<dyn KmsBackend>,
}

/// Reject a nonsensical `[secrets].backends` list at boot: empty (at least one
/// backend is required -- the local-override readers alone can't be the whole
/// credential source), or a backend named twice (dead after the first, by
/// first-match-wins). The order of the backends is the operator's choice.
pub fn validate_backends(backends: &[crate::cfg::file::CredentialBackend]) -> eyre::Result<()> {
    if backends.is_empty() {
        return Err(eyre::eyre!(
            "[secrets].backends is empty; at least one backend (postgres or vault) is required"
        ));
    }
    let unique: std::collections::HashSet<_> = backends.iter().collect();
    if unique.len() != backends.len() {
        return Err(eyre::eyre!(
            "[secrets].backends names a backend more than once"
        ));
    }
    Ok(())
}

/// The secret path that records vault import completion. It starts with a
/// slash on purpose: real credential paths never do, so no `CredentialKey`
/// can collide with it, and the kek-scoped journal queries exclude it.
pub(crate) const VAULT_IMPORT_MARKER_PATH: &str = "/_vault_import";

/// How to treat secrets that already exist in Postgres during an import.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportApproach {
    /// Import only secrets whose path has no entries yet.
    #[default]
    MissingOnly,
    /// Import everything, appending a new journal entry per secret even
    /// when the path already has entries.
    All,
}

/// What happened after a Vault import acquired its database interlock.
#[derive(Debug, PartialEq, Eq)]
pub enum ImportResult {
    /// This transaction committed the secrets and completion marker.
    Completed {
        /// Secrets written to Postgres.
        imported: u64,
        /// Secrets left alone because their path already had entries
        /// (`MissingOnly` only).
        skipped: u64,
    },
    /// The permanent marker was already present after taking the import
    /// interlock, so this transaction wrote nothing.
    AlreadyComplete,
}

/// Errors from the Postgres secrets backend.
#[derive(Debug, thiserror::Error)]
pub enum PgSecretsError {
    #[error("database error: {0}")]
    Database(#[from] db::DatabaseError),

    #[error("routing configuration error: {0}")]
    RoutingConfig(String),

    #[error("credential already exists for path: {0}")]
    AlreadyExists(String),

    #[error("a re-wrap is already running")]
    ReWrapInProgress,

    #[error("work lock error: {0}")]
    AcquireLock(#[from] db::work_lock_manager::AcquireLockError),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("KMS error: {0}")]
    Kms(#[from] carbide_kms_provider::KmsError),
}

impl From<PgSecretsError> for SecretsError {
    fn from(err: PgSecretsError) -> Self {
        SecretsError::GenericError(eyre::Report::new(err))
    }
}

/// A decrypted journal entry, returned by the history and lookup methods.
#[allow(dead_code)] // Staged for credential rotation: https://github.com/NVIDIA/infra-controller/issues/367.
struct SecretEntry {
    /// Identifies this journal entry.
    secret_id: carbide_uuid::secret::SecretId,
    /// The journal order -- higher means written later.
    seq: i64,
    /// The credential path.
    path: String,
    /// The decrypted credential value.
    credentials: Credentials,
    /// The KEK that wrapped this entry's DEK.
    kek_id: String,
    /// When this entry was written.
    created_at: chrono::DateTime<chrono::Utc>,
}

/// The `CredentialManager` backed by the Postgres secrets journal. Reads
/// return the newest entry for a path; writes append; delete removes the
/// path's whole history -- the same semantics Vault KV gave the rest of the
/// system.
#[derive(Clone)]
pub struct PostgresCredentialManager {
    pool: PgPool,
    routing: SecretRouting,
    kms: Arc<dyn KmsBackend>,
}

impl PostgresCredentialManager {
    /// Create a manager from a pool, write routing, and KMS backend.
    pub fn new(pool: PgPool, routing: SecretRouting, kms: Arc<dyn KmsBackend>) -> Self {
        Self { pool, routing, kms }
    }

    fn timer(&self, operation: SecretsOperation) -> OperationTimer {
        OperationTimer::start(operation)
    }

    async fn conn(&self) -> Result<sqlx::pool::PoolConnection<sqlx::Postgres>, PgSecretsError> {
        self.pool
            .acquire()
            .await
            .map_err(|e| PgSecretsError::Database(db::DatabaseError::acquire(e)))
    }

    // -- Journal access, used by credential rotation --
    //
    // Nothing in this crate calls these yet: the rotation manager reads
    // history to inspect previous values and deletes a failed attempt's
    // entry by id, which makes the previous entry current again.

    /// Return every journal entry for a credential, newest first.
    #[allow(dead_code)] // Staged for credential rotation: https://github.com/NVIDIA/infra-controller/issues/367.
    async fn get_history(&self, key: &CredentialKey) -> Result<Vec<SecretEntry>, PgSecretsError> {
        let path = key.to_key_str();
        let rows = db::secrets::get_history(&self.pool, &path).await?;
        self.decrypt_rows(rows).await
    }

    /// Return one journal entry by id.
    #[allow(dead_code)] // Staged for credential rotation: https://github.com/NVIDIA/infra-controller/issues/367.
    async fn get_by_id(
        &self,
        secret_id: carbide_uuid::secret::SecretId,
    ) -> Result<Option<SecretEntry>, PgSecretsError> {
        let Some(row) = db::secrets::get_by_id(&self.pool, secret_id).await? else {
            return Ok(None);
        };
        Ok(Some(self.decrypt_row(row).await?))
    }

    /// Return every journal entry wrapped by the given KEK.
    #[allow(dead_code)] // Staged for credential rotation: https://github.com/NVIDIA/infra-controller/issues/367.
    async fn get_all_for_kek_id(&self, kek_id: &str) -> Result<Vec<SecretEntry>, PgSecretsError> {
        let rows = db::secrets::get_all_for_kek_id(&self.pool, kek_id).await?;
        self.decrypt_rows(rows).await
    }

    /// Return the credentials whose newest journal entry is wrapped by the
    /// given KEK.
    #[allow(dead_code)] // Staged for credential rotation: https://github.com/NVIDIA/infra-controller/issues/367.
    async fn get_latest_with_kek_id(
        &self,
        kek_id: &str,
    ) -> Result<Vec<SecretEntry>, PgSecretsError> {
        let rows = db::secrets::get_latest_with_kek_id(&self.pool, kek_id).await?;
        self.decrypt_rows(rows).await
    }

    /// Remove one journal entry by id. Deleting the newest entry makes the
    /// previous one current again.
    #[allow(dead_code)] // Staged for credential rotation: https://github.com/NVIDIA/infra-controller/issues/367.
    async fn delete_by_id(
        &self,
        secret_id: carbide_uuid::secret::SecretId,
    ) -> Result<bool, PgSecretsError> {
        let mut conn = self.conn().await?;
        Ok(db::secrets::delete_by_id(&mut conn, secret_id).await?)
    }

    // -- Envelope encryption --

    /// Decrypt one row: unwrap its DEK through the KMS, then decrypt the
    /// value with the row's path as associated data.
    async fn decrypt_row(&self, row: SecretRow) -> Result<SecretEntry, PgSecretsError> {
        let dek = self
            .kms
            .decrypt_dek(
                &row.kek_id,
                &EncryptedDek {
                    ciphertext: row.encrypted_dek,
                    nonce: row.dek_nonce,
                },
            )
            .await?;
        let plaintext = Zeroizing::new(carbide_kms_provider::crypto::decrypt(
            &dek,
            &row.nonce,
            &row.encrypted_value,
            row.path.as_bytes(),
        )?);

        let credentials: Credentials = serde_json::from_slice(&plaintext)?;
        Ok(SecretEntry {
            secret_id: row.secret_id,
            seq: row.seq,
            path: row.path,
            credentials,
            kek_id: row.kek_id,
            created_at: row.created_at,
        })
    }

    async fn decrypt_rows(&self, rows: Vec<SecretRow>) -> Result<Vec<SecretEntry>, PgSecretsError> {
        let mut entries = Vec::with_capacity(rows.len());
        for row in rows {
            entries.push(self.decrypt_row(row).await?);
        }
        Ok(entries)
    }

    async fn encrypt_envelope(
        &self,
        path: &str,
        data: &[u8],
    ) -> Result<EncryptedEnvelope, PgSecretsError> {
        encrypt_envelope(&self.routing, self.kms.as_ref(), path, data).await
    }
}

/// Encrypt a credential value for `path`: route to the active KEK, generate
/// and wrap a new DEK, and encrypt the value with the path as associated
/// data. Every write -- manager or import -- goes through here, so the
/// path binding cannot diverge between them.
pub(crate) async fn encrypt_envelope(
    routing: &SecretRouting,
    kms: &dyn KmsBackend,
    path: &str,
    data: &[u8],
) -> Result<EncryptedEnvelope, PgSecretsError> {
    let kek_id = routing.active_kek_for_path(path)?;
    let (dek, wrapped_dek) = kms.generate_and_wrap_dek(kek_id).await?;
    let (encrypted_value, nonce) =
        carbide_kms_provider::crypto::encrypt(&dek, data, path.as_bytes())?;
    Ok(EncryptedEnvelope {
        encrypted_value,
        nonce,
        encrypted_dek: wrapped_dek.ciphertext,
        dek_nonce: wrapped_dek.nonce,
        kek_id: kek_id.to_string(),
    })
}

/// The columns produced by one envelope encryption, ready to insert.
pub(crate) struct EncryptedEnvelope {
    encrypted_value: Vec<u8>,
    nonce: Vec<u8>,
    encrypted_dek: Vec<u8>,
    dek_nonce: Vec<u8>,
    kek_id: String,
}

impl EncryptedEnvelope {
    pub(crate) fn as_new_entry<'a>(&'a self, path: &'a str) -> NewSecretEntry<'a> {
        NewSecretEntry {
            path,
            encrypted_value: &self.encrypted_value,
            nonce: &self.nonce,
            kek_id: &self.kek_id,
            encrypted_dek: &self.encrypted_dek,
            dek_nonce: &self.dek_nonce,
        }
    }
}

#[async_trait]
impl CredentialReader for PostgresCredentialManager {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        let timer = self.timer(SecretsOperation::Get);
        let path = key.to_key_str();

        let row = db::secrets::get_latest(&self.pool, &path)
            .await
            .map_err(PgSecretsError::from)?;
        let Some(row) = row else {
            timer.succeed();
            return Ok(None);
        };

        tracing::debug!(
            path = %row.path,
            secret_id = %row.secret_id,
            seq = row.seq,
            kek_id = %row.kek_id,
            created_at = %row.created_at,
            "read secret from postgres"
        );

        let entry = self.decrypt_row(row).await?;

        timer.succeed();
        // An empty password reads as no credential at all, exactly like the
        // Vault reader: the UFM and site-wide BMC delete flows "delete" by
        // writing an empty-password tombstone, and their consumers depend
        // on getting None back.
        match entry.credentials {
            Credentials::UsernamePassword { ref password, .. } if password.is_empty() => Ok(None),
            credentials => Ok(Some(credentials)),
        }
    }
}

#[async_trait]
impl CredentialWriter for PostgresCredentialManager {
    async fn get_credentials_from_writer(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        CredentialReader::get_credentials(self, key).await
    }

    async fn set_credentials(
        &self,
        key: &CredentialKey,
        credentials: &Credentials,
    ) -> Result<(), SecretsError> {
        let timer = self.timer(SecretsOperation::Set);
        let path = key.to_key_str();

        let json_bytes =
            Zeroizing::new(serde_json::to_vec(credentials).map_err(PgSecretsError::from)?);
        let envelope = self.encrypt_envelope(&path, &json_bytes).await?;

        let mut conn = self.conn().await?;
        db::secrets::insert(&mut conn, &envelope.as_new_entry(&path))
            .await
            .map_err(PgSecretsError::from)?;

        timer.succeed();
        Ok(())
    }

    async fn create_credentials(
        &self,
        key: &CredentialKey,
        credentials: &Credentials,
    ) -> Result<(), SecretsError> {
        let timer = self.timer(SecretsOperation::Create);
        let path = key.to_key_str();

        // Encrypt before the transaction opens: the KMS call can be a
        // network round-trip (Transit), and nothing network-bound belongs
        // inside a transaction that holds an advisory lock. The price is
        // one wasted envelope when the credential turns out to exist.
        let json_bytes =
            Zeroizing::new(serde_json::to_vec(credentials).map_err(PgSecretsError::from)?);
        let envelope = self.encrypt_envelope(&path, &json_bytes).await?;

        // Create-only means check-then-insert, and those are two statements:
        // hold the path's advisory lock to serialize creates and the table's
        // write lock to order the check against a bulk import. Vault gave us
        // the first property through its compare-and-set; Postgres needs both
        // locks because the journal has no unique path index to enforce it.
        let mut txn = self
            .pool
            .begin()
            .await
            .map_err(|e| PgSecretsError::Database(db::DatabaseError::acquire(e)))?;
        match db::secrets::insert_if_missing(&mut txn, &envelope.as_new_entry(&path))
            .await
            .map_err(PgSecretsError::from)?
        {
            ConditionalWrite::Applied(()) => {}
            ConditionalWrite::NotApplied(SecretAlreadyExists) => {
                return Err(PgSecretsError::AlreadyExists(path.to_string()).into());
            }
        }
        txn.commit()
            .await
            .map_err(|e| PgSecretsError::Database(db::DatabaseError::new("commit create", e)))?;

        timer.succeed();
        Ok(())
    }

    async fn delete_credentials(&self, key: &CredentialKey) -> Result<(), SecretsError> {
        let timer = self.timer(SecretsOperation::Delete);
        let path = key.to_key_str();

        let mut conn = self.conn().await?;
        db::secrets::delete_all(&mut conn, &path)
            .await
            .map_err(PgSecretsError::from)?;

        timer.succeed();
        Ok(())
    }
}

impl CredentialManager for PostgresCredentialManager {}

#[cfg(test)]
mod backend_validation_tests {
    use super::validate_backends;
    use crate::cfg::file::CredentialBackend;

    #[test]
    fn accepts_any_order_of_distinct_backends() {
        assert!(validate_backends(&[CredentialBackend::Vault]).is_ok());
        assert!(validate_backends(&[CredentialBackend::Postgres]).is_ok());
        assert!(
            validate_backends(&[CredentialBackend::Postgres, CredentialBackend::Vault]).is_ok()
        );
        // Order is the operator's choice -- vault ahead of postgres is fine.
        assert!(
            validate_backends(&[CredentialBackend::Vault, CredentialBackend::Postgres]).is_ok()
        );
    }

    #[test]
    fn rejects_empty_backends() {
        assert!(validate_backends(&[]).is_err());
    }

    #[test]
    fn rejects_a_backend_named_twice() {
        assert!(validate_backends(&[CredentialBackend::Vault, CredentialBackend::Vault]).is_err());
    }
}
