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

//! Database-backed tests for the Postgres credential manager and re-wrap.
//! These build the manager directly on the test pool with local key
//! material -- no API fixture needed.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use carbide_kms_provider::{EncryptedDek, IntegratedKmsProvider, KmsBackend, KmsError};
use carbide_secrets::SecretsError;
use carbide_secrets::credentials::{
    CredentialKey, CredentialReader, CredentialWriter, Credentials,
};
use db::work_lock_manager::{self, AcquireLockError, KeepaliveConfig, WorkLockManagerHandle};
use tokio::sync::Notify;
use tokio::task::JoinSet;
use zeroize::Zeroizing;

use super::re_wrap::{RE_WRAP_WORK_KEY, re_wrap_stale};
use super::routing::SecretRouting;
use super::{ImportResult, PgSecretsError, PostgresCredentialManager};

fn test_key(seed: u8) -> [u8; 32] {
    let mut key = [0u8; 32];
    for (i, byte) in key.iter_mut().enumerate() {
        *byte = seed.wrapping_add(i as u8);
    }
    key
}

/// A KMS with one key per (kek_id, seed) pair.
fn kms_with_keys(keys: &[(&str, u8)]) -> Arc<dyn KmsBackend> {
    let map: HashMap<String, [u8; 32]> = keys
        .iter()
        .map(|(kek_id, seed)| (kek_id.to_string(), test_key(*seed)))
        .collect();
    Arc::new(IntegratedKmsProvider::new(map))
}

struct PausingKms {
    inner: Arc<dyn KmsBackend>,
    paused_once: AtomicBool,
    pause_reached: Notify,
    resume: Notify,
}

impl PausingKms {
    fn new(inner: Arc<dyn KmsBackend>) -> Self {
        Self {
            inner,
            paused_once: AtomicBool::new(false),
            pause_reached: Notify::new(),
            resume: Notify::new(),
        }
    }

    async fn wait_until_paused(&self) {
        self.pause_reached.notified().await;
    }

    fn resume(&self) {
        self.resume.notify_one();
    }
}

#[async_trait]
impl KmsBackend for PausingKms {
    async fn encrypt_dek(&self, kek_id: &str, dek: &[u8; 32]) -> Result<EncryptedDek, KmsError> {
        self.inner.encrypt_dek(kek_id, dek).await
    }

    async fn decrypt_dek(
        &self,
        kek_id: &str,
        encrypted: &EncryptedDek,
    ) -> Result<Zeroizing<[u8; 32]>, KmsError> {
        self.inner.decrypt_dek(kek_id, encrypted).await
    }

    fn can_decrypt_kek(&self, kek_id: &str) -> bool {
        self.inner.can_decrypt_kek(kek_id)
    }

    fn kek_ids(&self) -> Vec<String> {
        self.inner.kek_ids()
    }

    async fn generate_and_wrap_dek(
        &self,
        kek_id: &str,
    ) -> Result<(Zeroizing<[u8; 32]>, EncryptedDek), KmsError> {
        if !self.paused_once.swap(true, Ordering::AcqRel) {
            self.pause_reached.notify_one();
            self.resume.notified().await;
        }
        self.inner.generate_and_wrap_dek(kek_id).await
    }
}

fn catch_all_routing(kek_id: &str) -> SecretRouting {
    SecretRouting::new(vec![("/".to_string(), kek_id.to_string())])
}

fn manager(
    pool: &sqlx::PgPool,
    routing: SecretRouting,
    kms: Arc<dyn KmsBackend>,
) -> PostgresCredentialManager {
    PostgresCredentialManager::new(pool.clone(), routing, kms)
}

async fn start_test_work_lock_manager(pool: &sqlx::PgPool) -> (WorkLockManagerHandle, JoinSet<()>) {
    let mut tasks = JoinSet::new();
    let handle = work_lock_manager::start(&mut tasks, pool.clone(), Default::default())
        .await
        .expect("start work lock manager");
    (handle, tasks)
}

fn ufm_key(fabric: &str) -> CredentialKey {
    CredentialKey::UfmAuth {
        fabric: fabric.to_string(),
    }
}

fn cred(user: &str, pass: &str) -> Credentials {
    Credentials::UsernamePassword {
        username: user.to_string(),
        password: pass.to_string(),
    }
}

// Verifies the journal behavior behind set/get: every set appends, and get
// returns the newest entry.
#[crate::sqlx_test]
async fn set_get_round_trip_and_journal_latest_wins(pool: sqlx::PgPool) {
    let mgr = manager(&pool, catch_all_routing("k1"), kms_with_keys(&[("k1", 1)]));
    let key = ufm_key("fab1");

    mgr.set_credentials(&key, &cred("admin", "first"))
        .await
        .expect("first set");
    mgr.set_credentials(&key, &cred("admin", "second"))
        .await
        .expect("second set");

    let current = mgr.get_credentials(&key).await.expect("get");
    assert_eq!(current, Some(cred("admin", "second")));

    let history = mgr.get_history(&key).await.expect("history");
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].credentials, cred("admin", "second"));
    assert_eq!(history[1].credentials, cred("admin", "first"));
}

// Verifies create-only semantics: the second create fails and leaves the
// first value in place.
#[crate::sqlx_test]
async fn create_fails_when_credential_exists(pool: sqlx::PgPool) {
    let mgr = manager(&pool, catch_all_routing("k1"), kms_with_keys(&[("k1", 1)]));
    let key = ufm_key("fab1");

    mgr.create_credentials(&key, &cred("admin", "original"))
        .await
        .expect("first create");

    let second = mgr
        .create_credentials(&key, &cred("admin", "usurper"))
        .await;
    let err = second.expect_err("second create must fail");
    let SecretsError::GenericError(report) = &err else {
        panic!("unexpected error: {err}");
    };
    let Some(PgSecretsError::AlreadyExists(path)) = report.downcast_ref::<PgSecretsError>() else {
        panic!("unexpected error: {err}");
    };
    assert_eq!(path, key.to_key_str().as_ref());

    let current = mgr.get_credentials(&key).await.expect("get");
    assert_eq!(current, Some(cred("admin", "original")));
}

// Verifies that delete removes the whole journal, not just the newest
// entry -- the same semantics Vault's delete gave callers.
#[crate::sqlx_test]
async fn delete_removes_all_journal_entries(pool: sqlx::PgPool) {
    let mgr = manager(&pool, catch_all_routing("k1"), kms_with_keys(&[("k1", 1)]));
    let key = ufm_key("fab1");

    mgr.set_credentials(&key, &cred("admin", "first"))
        .await
        .expect("first set");
    mgr.set_credentials(&key, &cred("admin", "second"))
        .await
        .expect("second set");
    mgr.delete_credentials(&key).await.expect("delete");

    assert_eq!(mgr.get_credentials(&key).await.expect("get"), None);
    assert!(mgr.get_history(&key).await.expect("history").is_empty());
}

// Verifies that the journal order is true write order even when two
// entries record the identical created_at (Postgres fixes now() per
// transaction): the second insert wins, by seq, not by chance.
#[crate::sqlx_test]
async fn second_write_wins_on_created_at_ties(pool: sqlx::PgPool) {
    let kms = kms_with_keys(&[("k1", 1)]);
    let routing = catch_all_routing("k1");
    let path = "tie/test/path";

    let mut txn = pool.begin().await.expect("begin");
    let first = super::encrypt_envelope(&routing, kms.as_ref(), path, b"\"first\"")
        .await
        .expect("encrypt first");
    db::secrets::insert(&mut txn, &first.as_new_entry(path))
        .await
        .expect("insert first");
    let second = super::encrypt_envelope(&routing, kms.as_ref(), path, b"\"second\"")
        .await
        .expect("encrypt second");
    db::secrets::insert(&mut txn, &second.as_new_entry(path))
        .await
        .expect("insert second");
    txn.commit().await.expect("commit");

    let newest = db::secrets::get_latest(&pool, path)
        .await
        .expect("get_latest")
        .expect("row");
    assert_eq!(
        newest.created_at,
        db::secrets::get_history(&pool, path)
            .await
            .expect("history")[1]
            .created_at,
        "both entries record the transaction's shared now()"
    );
    assert_eq!(
        newest.encrypted_value,
        second.as_new_entry(path).encrypted_value,
        "the later insert must be the newest entry"
    );
}

// Verifies the rotation rollback story: deleting the newest journal entry
// makes the previous credential current again.
#[crate::sqlx_test]
async fn delete_by_id_restores_previous_credential(pool: sqlx::PgPool) {
    let mgr = manager(&pool, catch_all_routing("k1"), kms_with_keys(&[("k1", 1)]));
    let key = ufm_key("fab1");

    mgr.set_credentials(&key, &cred("admin", "v1"))
        .await
        .expect("set v1");
    mgr.set_credentials(&key, &cred("admin", "v2"))
        .await
        .expect("set v2");

    let newest = &mgr.get_history(&key).await.expect("history")[0];
    assert_eq!(newest.credentials, cred("admin", "v2"));

    let fetched = mgr
        .get_by_id(newest.secret_id)
        .await
        .expect("get_by_id")
        .expect("entry");
    assert_eq!(fetched.credentials, cred("admin", "v2"));

    assert!(mgr.delete_by_id(newest.secret_id).await.expect("delete"));
    assert_eq!(
        mgr.get_credentials(&key).await.expect("get"),
        Some(cred("admin", "v1")),
        "the previous entry is current again"
    );
}

// Verifies the empty-password tombstone behavior the Vault reader
// established: several delete flows "delete" by writing an empty password,
// and reads must answer None for it.
#[crate::sqlx_test]
async fn empty_password_reads_as_none(pool: sqlx::PgPool) {
    let mgr = manager(&pool, catch_all_routing("k1"), kms_with_keys(&[("k1", 1)]));
    let key = ufm_key("fab1");

    mgr.set_credentials(&key, &cred("admin", "live"))
        .await
        .expect("set live");
    mgr.set_credentials(&key, &cred("admin", ""))
        .await
        .expect("set tombstone");

    assert_eq!(
        mgr.get_credentials(&key).await.expect("get"),
        None,
        "an empty-password tombstone must read as no credential"
    );
    assert_eq!(
        mgr.get_history(&key).await.expect("history").len(),
        2,
        "the journal keeps the tombstone entry itself"
    );
}

// Verifies the associated-data binding end to end: a ciphertext copied
// onto another path fails to decrypt instead of serving the wrong
// credential.
#[crate::sqlx_test]
async fn ciphertext_copied_to_another_path_does_not_decrypt(pool: sqlx::PgPool) {
    let mgr = manager(&pool, catch_all_routing("k1"), kms_with_keys(&[("k1", 1)]));
    let key_a = ufm_key("fab-a");
    let key_b = ufm_key("fab-b");

    mgr.set_credentials(&key_a, &cred("admin", "a-secret"))
        .await
        .expect("set");

    // Copy fab-a's encrypted columns onto fab-b's path, the way an
    // attacker with table access (but no keys) would.
    sqlx::query(
        "INSERT INTO secrets
             (secret_id, path, encrypted_value, nonce, kek_id, encrypted_dek, dek_nonce)
         SELECT gen_random_uuid(), $2, encrypted_value, nonce, kek_id, encrypted_dek, dek_nonce
         FROM secrets WHERE path = $1",
    )
    .bind(key_a.to_key_str().as_ref())
    .bind(key_b.to_key_str().as_ref())
    .execute(&pool)
    .await
    .expect("transplant row");

    let stolen = mgr.get_credentials(&key_b).await;
    assert!(
        stolen.is_err(),
        "a transplanted ciphertext must fail decryption, got: {stolen:?}"
    );
}

#[crate::sqlx_test]
async fn bulk_secret_write_blocks_create_before_exists_check(pool: sqlx::PgPool) {
    let key = ufm_key("bulk-write-create-race");
    let path = key.to_key_str().to_string();
    let create_manager = manager(&pool, catch_all_routing("k1"), kms_with_keys(&[("k1", 1)]));
    let mut bulk_txn = pool.begin().await.expect("begin bulk transaction");
    db::secrets::lock_for_bulk_write(&mut bulk_txn)
        .await
        .expect("lock secrets for bulk write");

    let create_task = tokio::spawn(async move {
        create_manager
            .create_credentials(&key, &cred("create", "value"))
            .await
    });

    // `insert_if_missing` needs to wait on its table lock before it checks the
    // path. If it checked first and only blocked at `INSERT`, both transactions
    // could decide that the path was missing.
    let wait_result = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM pg_locks
                    WHERE database = (
                            SELECT oid FROM pg_database
                            WHERE datname = current_database()
                        )
                      AND relation = 'secrets'::regclass
                      AND mode = 'RowExclusiveLock'
                      AND NOT granted
                )",
            )
            .fetch_one(&pool)
            .await
            .expect("check create table lock");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    if wait_result.is_err() {
        bulk_txn
            .rollback()
            .await
            .expect("roll back timed-out bulk transaction");
        create_task.abort();
        panic!("create did not wait before its existence check");
    }

    db::secrets::insert(
        &mut bulk_txn,
        &db::secrets::NewSecretEntry {
            path: &path,
            encrypted_value: b"bulk",
            nonce: b"nonce",
            kek_id: "kek",
            encrypted_dek: b"dek",
            dek_nonce: b"dek-nonce",
        },
    )
    .await
    .expect("insert bulk secret");
    bulk_txn.commit().await.expect("commit bulk transaction");

    let create_error = create_task
        .await
        .expect("create task panicked")
        .expect_err("create must observe the bulk entry after its table lock is granted");
    assert!(
        create_error.to_string().contains("already exists"),
        "unexpected create error: {create_error}"
    );
    let row_count: i64 = sqlx::query_scalar("SELECT count(*) FROM secrets WHERE path = $1")
        .bind(path)
        .fetch_one(&pool)
        .await
        .expect("count raced secret entries");
    assert_eq!(row_count, 1, "the create race must leave one journal entry");
}

#[crate::sqlx_test]
async fn create_waiting_on_path_lock_does_not_block_bulk_write(pool: sqlx::PgPool) {
    let key = ufm_key("legacy-path-lock-race");
    let path = key.to_key_str().to_string();
    let create_manager = manager(&pool, catch_all_routing("k1"), kms_with_keys(&[("k1", 1)]));

    // Model a create from an older replica: it takes the path advisory lock,
    // checks for an existing entry, and does not request its table-write lock
    // until the later INSERT.
    let mut legacy_txn = pool.begin().await.expect("begin legacy create transaction");
    db::secrets::lock_path_advisory(&mut legacy_txn, &path)
        .await
        .expect("take legacy path advisory lock");
    assert!(
        !db::secrets::exists(&mut *legacy_txn, &path)
            .await
            .expect("check legacy path"),
        "test path must start empty"
    );

    let create_task = tokio::spawn(async move {
        create_manager
            .create_credentials(&key, &cred("create", "value"))
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS(
                    SELECT 1 FROM pg_locks
                    WHERE database = (
                            SELECT oid FROM pg_database
                            WHERE datname = current_database()
                        )
                      AND locktype = 'advisory'
                      AND NOT granted
                )",
            )
            .fetch_one(&pool)
            .await
            .expect("check create advisory lock");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("create did not wait on the legacy path lock");

    let mut bulk_txn = pool.begin().await.expect("begin bulk transaction");
    sqlx::query("SET LOCAL lock_timeout = '250ms'")
        .execute(&mut *bulk_txn)
        .await
        .expect("set bulk table-lock timeout");
    let bulk_lock_result = db::secrets::lock_for_bulk_write(&mut bulk_txn).await;
    if let Err(error) = bulk_lock_result {
        create_task.abort();
        drop(bulk_txn);
        drop(legacy_txn);
        panic!("create held a table-write lock while waiting on its path lock: {error}");
    }
    db::secrets::insert(
        &mut bulk_txn,
        &db::secrets::NewSecretEntry {
            path: &path,
            encrypted_value: b"bulk",
            nonce: b"nonce",
            kek_id: "kek",
            encrypted_dek: b"dek",
            dek_nonce: b"dek-nonce",
        },
    )
    .await
    .expect("insert bulk secret");
    bulk_txn.commit().await.expect("commit bulk transaction");
    legacy_txn
        .rollback()
        .await
        .expect("roll back legacy transaction");

    let create_error = create_task
        .await
        .expect("create task panicked")
        .expect_err("create must observe the bulk entry after the legacy lock is released");
    assert!(
        create_error.to_string().contains("already exists"),
        "unexpected create error: {create_error}"
    );
}

#[crate::sqlx_test]
async fn vault_import_missing_only_preserves_existing_and_marker_stops_late_batch(
    pool: sqlx::PgPool,
) {
    let routing = catch_all_routing("k1");
    let kms = kms_with_keys(&[("k1", 1)]);
    let pg_manager = manager(&pool, routing.clone(), kms.clone());
    let existing_key = ufm_key("existing");
    let missing_key = ufm_key("missing");
    pg_manager
        .set_credentials(&existing_key, &cred("postgres", "existing"))
        .await
        .expect("seed existing credential");

    let (work_lock_manager, work_lock_tasks) = start_test_work_lock_manager(&pool).await;
    let work_lock = work_lock_manager
        .try_acquire_lock("vault-import-missing-only".to_string())
        .await
        .expect("acquire vault import work lock");
    let secrets = vec![
        (
            existing_key.to_key_str().to_string(),
            cred("vault", "existing"),
        ),
        (
            missing_key.to_key_str().to_string(),
            cred("vault", "missing"),
        ),
    ];

    let result = super::import_vault_secrets(
        &pool,
        &work_lock,
        &routing,
        kms.as_ref(),
        &secrets,
        super::ImportApproach::MissingOnly,
    )
    .await
    .expect("import missing Vault credentials");
    assert_eq!(
        result,
        ImportResult::Completed {
            imported: 1,
            skipped: 1,
        }
    );
    assert_eq!(
        pg_manager
            .get_credentials(&existing_key)
            .await
            .expect("read existing credential"),
        Some(cred("postgres", "existing"))
    );
    assert_eq!(
        pg_manager
            .get_credentials(&missing_key)
            .await
            .expect("read imported credential"),
        Some(cred("vault", "missing"))
    );

    let late_key = ufm_key("late");
    let late_result = super::import_vault_secrets(
        &pool,
        &work_lock,
        &routing,
        kms.as_ref(),
        &[(late_key.to_key_str().to_string(), cred("vault", "late"))],
        super::ImportApproach::All,
    )
    .await
    .expect("recognize completed Vault import");
    assert_eq!(late_result, ImportResult::AlreadyComplete);
    assert_eq!(
        pg_manager
            .get_credentials(&late_key)
            .await
            .expect("read late credential"),
        None,
        "the permanent marker must keep a late batch out"
    );

    work_lock
        .release()
        .await
        .expect("release vault import work lock");
    drop(work_lock_manager);
    tokio::time::timeout(Duration::from_secs(3), work_lock_tasks.join_all())
        .await
        .expect("WorkLockManager did not shut down in a timely manner");
}

#[crate::sqlx_test]
async fn vault_import_missing_only_restores_path_deleted_during_preparation(pool: sqlx::PgPool) {
    let routing = catch_all_routing("k1");
    let kms = kms_with_keys(&[("k1", 1)]);
    let pg_manager = manager(&pool, routing.clone(), kms.clone());
    let existing_key = ufm_key("deleted-during-preparation");
    let missing_key = ufm_key("missing-during-preparation");
    pg_manager
        .set_credentials(&existing_key, &cred("postgres", "existing"))
        .await
        .expect("seed existing credential");

    let (work_lock_manager, work_lock_tasks) = start_test_work_lock_manager(&pool).await;
    let work_lock = work_lock_manager
        .try_acquire_lock("vault-import-missing-only-delete-race".to_string())
        .await
        .expect("acquire vault import work lock");
    let pausing_kms = Arc::new(PausingKms::new(kms));
    let secrets = vec![
        (
            existing_key.to_key_str().to_string(),
            cred("vault", "restored"),
        ),
        (
            missing_key.to_key_str().to_string(),
            cred("vault", "missing"),
        ),
    ];
    let import_pool = pool.clone();
    let import_routing = routing.clone();
    let import_kms = pausing_kms.clone();
    let import_task = tokio::spawn(async move {
        let result = super::import_vault_secrets(
            &import_pool,
            &work_lock,
            &import_routing,
            import_kms.as_ref(),
            &secrets,
            super::ImportApproach::MissingOnly,
        )
        .await;
        (result, work_lock)
    });

    tokio::time::timeout(Duration::from_secs(3), pausing_kms.wait_until_paused())
        .await
        .expect("Vault import did not pause during preparation");
    pg_manager
        .delete_credentials(&existing_key)
        .await
        .expect("delete credential during preparation");
    pausing_kms.resume();

    let (result, work_lock) = tokio::time::timeout(Duration::from_secs(3), import_task)
        .await
        .expect("Vault import did not finish")
        .expect("Vault import task panicked");
    assert_eq!(
        result.expect("import Vault credentials"),
        ImportResult::Completed {
            imported: 2,
            skipped: 0,
        }
    );
    assert_eq!(
        pg_manager
            .get_credentials(&existing_key)
            .await
            .expect("read restored credential"),
        Some(cred("vault", "restored"))
    );
    assert_eq!(
        pg_manager
            .get_credentials(&missing_key)
            .await
            .expect("read missing credential"),
        Some(cred("vault", "missing"))
    );

    work_lock
        .release()
        .await
        .expect("release vault import work lock");
    drop(work_lock_manager);
    tokio::time::timeout(Duration::from_secs(3), work_lock_tasks.join_all())
        .await
        .expect("WorkLockManager did not shut down in a timely manner");
}

#[crate::sqlx_test]
async fn stale_vault_import_owner_cannot_write_or_mark_after_takeover(pool: sqlx::PgPool) {
    let keepalive_config = KeepaliveConfig {
        interval: Duration::from_secs(60),
        timeout: Duration::from_millis(100),
    };
    let mut work_lock_tasks = JoinSet::new();
    let owner_manager =
        work_lock_manager::start(&mut work_lock_tasks, pool.clone(), keepalive_config)
            .await
            .expect("start owner work lock manager");
    let replacement_manager =
        work_lock_manager::start(&mut work_lock_tasks, pool.clone(), keepalive_config)
            .await
            .expect("start replacement work lock manager");
    let work_key = "vault-import-takeover".to_string();
    let owner_lock = owner_manager
        .try_acquire_lock(work_key.clone())
        .await
        .expect("acquire owner work lock");

    let routing = catch_all_routing("k1");
    let owner_kms = Arc::new(PausingKms::new(kms_with_keys(&[("k1", 1)])));
    let first_key = ufm_key("first");
    let second_key = ufm_key("second");
    let owner_secrets = vec![
        (first_key.to_key_str().to_string(), cred("old", "first")),
        (second_key.to_key_str().to_string(), cred("old", "second")),
    ];
    let owner_pool = pool.clone();
    let owner_routing = routing.clone();
    let owner_kms_for_import = owner_kms.clone();
    let owner_import = tokio::spawn(async move {
        let result = super::import_vault_secrets(
            &owner_pool,
            &owner_lock,
            &owner_routing,
            owner_kms_for_import.as_ref(),
            &owner_secrets,
            super::ImportApproach::All,
        )
        .await;
        (result, owner_lock)
    });
    tokio::time::timeout(Duration::from_secs(3), owner_kms.wait_until_paused())
        .await
        .expect("owner import did not reach KMS pause");

    tokio::time::sleep(Duration::from_millis(200)).await;
    let acquisition_started = Instant::now();
    let replacement_lock = loop {
        match replacement_manager.try_acquire_lock(work_key.clone()).await {
            Ok(work_lock) => break work_lock,
            Err(AcquireLockError::WorkAlreadyLocked(_))
                if acquisition_started.elapsed() < Duration::from_secs(3) =>
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Err(error) => panic!("could not acquire replacement work lock: {error}"),
        }
    };

    owner_kms.resume();
    let (owner_result, owner_lock) = tokio::time::timeout(Duration::from_secs(3), owner_import)
        .await
        .expect("stale owner import did not finish")
        .expect("stale owner import task panicked");
    let owner_error = owner_result.expect_err("stale owner import must be fenced out");
    assert!(
        matches!(
            &owner_error,
            PgSecretsError::Database(db::DatabaseError::FailedPrecondition(_))
        ),
        "unexpected stale-owner error: {owner_error}"
    );
    owner_lock
        .release()
        .await
        .expect_err("stale owner must not release the replacement lock");

    let replacement_kms = kms_with_keys(&[("k1", 1)]);
    let replacement_secrets = vec![
        (first_key.to_key_str().to_string(), cred("new", "first")),
        (second_key.to_key_str().to_string(), cred("new", "second")),
    ];
    let replacement_result = super::import_vault_secrets(
        &pool,
        &replacement_lock,
        &routing,
        replacement_kms.as_ref(),
        &replacement_secrets,
        super::ImportApproach::MissingOnly,
    )
    .await
    .expect("replacement import must complete");
    assert_eq!(
        replacement_result,
        ImportResult::Completed {
            imported: 2,
            skipped: 0,
        }
    );

    let pg_manager = manager(&pool, routing, replacement_kms);
    assert_eq!(
        pg_manager
            .get_credentials(&first_key)
            .await
            .expect("read first credential"),
        Some(cred("new", "first"))
    );
    assert_eq!(
        pg_manager
            .get_credentials(&second_key)
            .await
            .expect("read second credential"),
        Some(cred("new", "second"))
    );
    assert_eq!(
        pg_manager
            .get_history(&first_key)
            .await
            .expect("read first history")
            .len(),
        1,
        "stale owner must not append the first credential"
    );
    assert_eq!(
        pg_manager
            .get_history(&second_key)
            .await
            .expect("read second history")
            .len(),
        1,
        "stale owner must not append the second credential"
    );
    let marker_count: i64 = sqlx::query_scalar("SELECT count(*) FROM secrets WHERE path = $1")
        .bind(super::VAULT_IMPORT_MARKER_PATH)
        .fetch_one(&pool)
        .await
        .expect("count vault import markers");
    assert_eq!(marker_count, 1, "only the replacement may write the marker");

    replacement_lock
        .release()
        .await
        .expect("release replacement work lock");
    drop(owner_manager);
    drop(replacement_manager);
    tokio::time::timeout(Duration::from_secs(3), work_lock_tasks.join_all())
        .await
        .expect("work lock managers did not shut down in a timely manner");
}

// Verifies that re-wrap moves every stale row to the KEK routing assigns,
// the rows still decrypt afterwards, and a second run finds nothing to do.
#[crate::sqlx_test]
async fn re_wrap_stale_moves_rows_and_is_idempotent(pool: sqlx::PgPool) {
    let kms = kms_with_keys(&[("old-key", 1), ("new-key", 2)]);
    let (work_lock_manager, _work_lock_tasks) = start_test_work_lock_manager(&pool).await;

    // Write under old-key: two credentials, one with two journal entries.
    let mgr_old = manager(&pool, catch_all_routing("old-key"), kms.clone());
    mgr_old
        .set_credentials(&ufm_key("fab1"), &cred("admin", "one"))
        .await
        .expect("set fab1");
    mgr_old
        .set_credentials(&ufm_key("fab1"), &cred("admin", "two"))
        .await
        .expect("set fab1 again");
    mgr_old
        .set_credentials(&ufm_key("fab2"), &cred("admin", "three"))
        .await
        .expect("set fab2");

    // Rotate: routing now assigns new-key to everything.
    let routing = catch_all_routing("new-key");
    let result = re_wrap_stale(&pool, &work_lock_manager, kms.as_ref(), &routing, 2)
        .await
        .expect("re-wrap");
    assert_eq!(result.re_wrapped, 3);
    assert_eq!(result.already_current, 0);
    assert_eq!(
        result.stale_remaining, 0,
        "old-key is unrouted and nothing is left on it"
    );

    // Every row decrypts, and historical entries moved too.
    let mgr_new = manager(&pool, routing.clone(), kms.clone());
    assert_eq!(
        mgr_new
            .get_credentials(&ufm_key("fab1"))
            .await
            .expect("get fab1"),
        Some(cred("admin", "two"))
    );
    assert_eq!(
        mgr_new
            .get_history(&ufm_key("fab1"))
            .await
            .expect("history")
            .len(),
        2
    );
    assert!(
        mgr_new
            .get_all_for_kek_id("old-key")
            .await
            .expect("old rows")
            .is_empty()
    );

    // A second run reports everything current and changes nothing.
    let again = re_wrap_stale(&pool, &work_lock_manager, kms.as_ref(), &routing, 2)
        .await
        .expect("re-wrap again");
    assert_eq!(again.re_wrapped, 0);
    assert_eq!(again.already_current, 3);
}

// Verifies that the re-wrap counters are exact when rows move between two
// KEKs that are both still routed -- the single-pass walk classifies each
// row exactly once.
#[crate::sqlx_test]
async fn re_wrap_stale_counts_each_row_once_across_routed_keks(pool: sqlx::PgPool) {
    let kms = kms_with_keys(&[("k1", 1), ("k2", 2)]);
    let (work_lock_manager, _work_lock_tasks) = start_test_work_lock_manager(&pool).await;

    // Both paths start under k1.
    let routing_old = catch_all_routing("k1");
    for path in ["alpha/one", "beta/two"] {
        let envelope = super::encrypt_envelope(&routing_old, kms.as_ref(), path, b"{}")
            .await
            .expect("encrypt");
        let mut conn = pool.acquire().await.expect("acquire");
        db::secrets::insert(&mut conn, &envelope.as_new_entry(path))
            .await
            .expect("insert");
    }

    // New routing sends beta/ to k2 while k1 stays routed for the rest.
    let routing_new = SecretRouting::new(vec![
        ("/".to_string(), "k1".to_string()),
        ("beta".to_string(), "k2".to_string()),
    ]);

    let result = re_wrap_stale(&pool, &work_lock_manager, kms.as_ref(), &routing_new, 1)
        .await
        .expect("re-wrap");
    assert_eq!(result.re_wrapped, 1, "only beta/two moved");
    assert_eq!(result.already_current, 1, "alpha/one was already routed");
    assert_eq!(result.stale_remaining, 0);

    let again = re_wrap_stale(&pool, &work_lock_manager, kms.as_ref(), &routing_new, 1)
        .await
        .expect("re-wrap again");
    assert_eq!(again.re_wrapped, 0);
    assert_eq!(again.already_current, 2);
}

// A second re-wrap reports contention while the shared work key is held. Once
// the guard drops, the manager processes its release before this caller's
// acquire and the next pass can start immediately.
#[crate::sqlx_test]
async fn re_wrap_stale_rejects_overlapping_run_and_releases_lock(pool: sqlx::PgPool) {
    let kms = kms_with_keys(&[("k1", 1)]);
    let routing = catch_all_routing("k1");
    let (work_lock_manager, _work_lock_tasks) = start_test_work_lock_manager(&pool).await;

    let lock = work_lock_manager
        .try_acquire_lock(RE_WRAP_WORK_KEY.into())
        .await
        .expect("acquire re-wrap lock");
    let error = re_wrap_stale(&pool, &work_lock_manager, kms.as_ref(), &routing, 1)
        .await
        .err()
        .expect("an overlapping re-wrap must be rejected");
    assert!(
        matches!(&error, PgSecretsError::ReWrapInProgress),
        "unexpected error: {error}"
    );

    drop(lock);
    let result = re_wrap_stale(&pool, &work_lock_manager, kms.as_ref(), &routing, 1)
        .await
        .expect("re-wrap after lock release");
    assert_eq!(result.re_wrapped, 0);
    assert_eq!(result.already_current, 0);
    assert_eq!(result.stale_remaining, 0);
}
