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

//! Re-encryption must not overwrite credentials replaced during remote key access.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use carbide_secrets::credentials::{
    CompositeCredentialManager, CredentialKey, CredentialReader, CredentialWriter, Credentials,
};
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_secrets::{SecretsError, key_encryption};
use db::tenant_identity_config::{self, ReencryptionNotApplied};
use model::metadata::Metadata;
use model::tenant::identity_config::SigningAlgorithm;
use model::tenant::{
    IdentityConfig, KeyId, SigningKeyMaterial, TenantOrganizationId, TokenDelegation,
    TokenDelegationAuthMethodConfig,
};
use p256::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
use rpc::forge::ReencryptTenantIdentitySecretsRequest;
use rpc::forge::forge_server::Forge;
use tokio::sync::Notify;
use tonic::Request;

use crate::test_support::builder::TestApiBuilder;
use crate::tests::common::api_fixtures::create_test_env;

// Pause the first old-key read before the write transaction, so another writer
// can commit. Later reads proceed normally for the fresh-invocation checks.
struct PausedKeyReader {
    credentials: Arc<TestCredentialManager>,
    pause_once: AtomicBool,
    paused: Notify,
    resume: Notify,
}

#[async_trait]
impl CredentialReader for PausedKeyReader {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        if matches!(key, CredentialKey::MachineIdentityEncryptionKey { key_id } if key_id == "old")
            && self.pause_once.swap(false, Ordering::SeqCst)
        {
            self.paused.notify_one();
            self.resume.notified().await;
        }
        self.credentials.get_credentials(key).await
    }
}

fn identity_config() -> IdentityConfig {
    IdentityConfig {
        issuer: "https://issuer.example.com".parse().unwrap(),
        default_audience: "api".to_string(),
        allowed_audiences: vec!["api".to_string()],
        token_ttl_sec: 3600,
        subject_prefix: "spiffe://issuer.example.com".to_string(),
        enabled: true,
        rotate_key: false,
        algorithm: SigningAlgorithm::Es256,
        encryption_key_id: "old".parse().unwrap(),
        signing_key_overlap_sec: None,
    }
}

fn signing_key(seed: u8, key_id: &str, encryption_key: &[u8; 32]) -> SigningKeyMaterial {
    let secret = p256::SecretKey::from_slice(&[seed; 32]).unwrap();
    let public = secret
        .public_key()
        .to_public_key_pem(LineEnding::LF)
        .unwrap();
    let private = secret.to_pkcs8_pem(LineEnding::LF).unwrap();
    SigningKeyMaterial {
        key_id: KeyId::from_public_key_material(&public),
        encrypted_signing_key: key_encryption::encrypt(private.as_bytes(), encryption_key, key_id)
            .unwrap()
            .parse()
            .unwrap(),
        signing_key_public: public.parse().unwrap(),
    }
}

fn delegation(client_secret: &str) -> TokenDelegation {
    TokenDelegation {
        token_endpoint: "https://auth.example.com/token".to_string(),
        subject_token_audience: "api".to_string(),
        auth_method_config: TokenDelegationAuthMethodConfig::ClientSecretBasic {
            client_id: "client".to_string(),
            client_secret: client_secret.to_string(),
        },
    }
}

async fn set_delegation(pool: &sqlx::PgPool, org_id: &TenantOrganizationId, client_secret: &str) {
    let delegation = delegation(client_secret);
    let (method, plaintext) = delegation.to_db_format();
    let ciphertext = key_encryption::encrypt(plaintext.as_bytes(), &[2; 32], "target")
        .unwrap()
        .parse()
        .unwrap();
    let mut txn = pool.begin().await.unwrap();
    tenant_identity_config::set_token_delegation(
        org_id,
        &delegation,
        method,
        &ciphertext,
        &mut txn,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn reencrypt_rejects_replaced_sources(pool: sqlx::PgPool) {
    enum Change {
        RotateSigningKey,
        ReplaceDelegation,
        DeleteConfig,
    }
    struct Case {
        name: &'static str,
        change: Change,
        rejection: ReencryptionNotApplied,
    }
    let env = create_test_env(pool.clone()).await;
    let credentials = Arc::new(TestCredentialManager::default());
    for (key_id, bytes) in [("old", [1; 32]), ("target", [2; 32])] {
        credentials
            .set_credentials(
                &CredentialKey::MachineIdentityEncryptionKey {
                    key_id: key_id.to_string(),
                },
                &Credentials::UsernamePassword {
                    username: String::new(),
                    password: base64::engine::general_purpose::STANDARD.encode(bytes),
                },
            )
            .await
            .unwrap();
    }
    let mut config = env.config.as_ref().clone();
    config.machine_identity.enabled = true;
    config.machine_identity.current_encryption_key_id = Some("target".to_string());
    let config = Arc::new(config);

    for Case {
        name,
        change,
        rejection,
    } in [
        Case {
            name: "rotate",
            change: Change::RotateSigningKey,
            rejection: ReencryptionNotApplied::SourceChanged,
        },
        Case {
            name: "delegation",
            change: Change::ReplaceDelegation,
            rejection: ReencryptionNotApplied::SourceChanged,
        },
        Case {
            name: "delete",
            change: Change::DeleteConfig,
            rejection: ReencryptionNotApplied::ConfigNotFound,
        },
    ] {
        let org_id: TenantOrganizationId = name.parse().unwrap();
        let mut txn = pool.begin().await.unwrap();
        db::tenant::create_and_persist(
            name.to_string(),
            Metadata {
                name: name.to_string(),
                ..Default::default()
            },
            None,
            &mut txn,
        )
        .await
        .unwrap();
        tenant_identity_config::set(
            &org_id,
            &identity_config(),
            Some(signing_key(1, "old", &[1; 32])),
            &mut txn,
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        // Delegation is already on the target key, but the plan still writes it.
        set_delegation(&pool, &org_id, "original").await;
        let reader = Arc::new(PausedKeyReader {
            credentials: credentials.clone(),
            pause_once: AtomicBool::new(true),
            paused: Notify::new(),
            resume: Notify::new(),
        });
        let api = TestApiBuilder::new(
            pool.clone(),
            env.common_pools.clone(),
            env.api.work_lock_manager_handle.clone(),
        )
        .with_runtime_config(config.clone())
        .with_credential_manager(Arc::new(CompositeCredentialManager::new(
            reader.clone(),
            credentials.clone(),
        )))
        .build();
        let request = || {
            Request::new(ReencryptTenantIdentitySecretsRequest {
                organization_id: Some(name.to_string()),
                dry_run: false,
            })
        };
        let update = async {
            reader.paused.notified().await;
            match change {
                Change::RotateSigningKey => {
                    let mut config = identity_config();
                    config.rotate_key = true;
                    config.signing_key_overlap_sec = Some(3600);
                    let mut txn = pool.begin().await.unwrap();
                    tenant_identity_config::set(
                        &org_id,
                        &config,
                        Some(signing_key(2, "target", &[2; 32])),
                        &mut txn,
                    )
                    .await
                    .unwrap();
                    txn.commit().await.unwrap();
                }
                Change::ReplaceDelegation => set_delegation(&pool, &org_id, "replacement").await,
                Change::DeleteConfig => {
                    let mut txn = pool.begin().await.unwrap();
                    tenant_identity_config::delete(&org_id, &mut txn)
                        .await
                        .unwrap();
                    txn.commit().await.unwrap();
                }
            }
            let mut connection = pool.acquire().await.unwrap();
            let expected = tenant_identity_config::find(&org_id, &mut *connection)
                .await
                .unwrap();
            let tenant_version = db::tenant::find(name, false, &mut connection)
                .await
                .unwrap()
                .unwrap()
                .version;
            reader.resume.notify_one();
            (expected, tenant_version)
        };
        let (response, (expected, tenant_version)) =
            tokio::time::timeout(Duration::from_secs(30), async {
                tokio::join!(api.reencrypt_tenant_identity_secrets(request()), update)
            })
            .await
            .expect("re-encryption and concurrent mutation must finish");
        let response = response.unwrap().into_inner();
        assert_eq!(response.rows_failed, 1, "{name}");
        assert_eq!(response.rows_updated, 0, "{name}");
        assert_eq!(response.fields_reencrypted, 0, "{name}");
        assert_eq!(response.fields_skipped_on_target, 0, "{name}");
        assert_eq!(response.failures.len(), 1, "{name}");
        assert_eq!(response.failures[0].error, rejection.to_string(), "{name}");
        let mut connection = pool.acquire().await.unwrap();
        assert_eq!(
            db::tenant::find(name, false, &mut connection)
                .await
                .unwrap()
                .unwrap()
                .version,
            tenant_version,
            "{name}",
        );
        let persisted = tenant_identity_config::find(&org_id, &mut *connection)
            .await
            .unwrap();
        drop(connection);
        let Some(expected) = expected else {
            assert!(persisted.is_none());
            continue;
        };
        let persisted = persisted.unwrap();
        assert_eq!(
            persisted.encrypted_signing_key_1,
            expected.encrypted_signing_key_1
        );
        assert_eq!(
            persisted.encrypted_signing_key_2,
            expected.encrypted_signing_key_2
        );
        assert_eq!(
            persisted.signing_key_public_2,
            expected.signing_key_public_2
        );
        assert_eq!(
            persisted.encrypted_auth_method_config,
            expected.encrypted_auth_method_config
        );

        if matches!(change, Change::RotateSigningKey) {
            let response = api
                .reencrypt_tenant_identity_secrets(Request::new(
                    ReencryptTenantIdentitySecretsRequest {
                        organization_id: Some(name.to_string()),
                        dry_run: true,
                    },
                ))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(response.rows_updated, 1);
            assert_eq!(response.fields_reencrypted, 1);
            let mut connection = pool.acquire().await.unwrap();
            let unchanged = tenant_identity_config::find(&org_id, &mut *connection)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                unchanged.encrypted_signing_key_1,
                expected.encrypted_signing_key_1
            );
            assert_eq!(
                db::tenant::find(name, false, &mut connection)
                    .await
                    .unwrap()
                    .unwrap()
                    .version,
                tenant_version,
            );
        }

        // A new invocation prepares from the replacement, outside a transaction.
        let response = api
            .reencrypt_tenant_identity_secrets(request())
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.rows_updated, 1);
        assert_eq!(response.fields_reencrypted, 1);
        assert_eq!(response.rows_failed, 0);
        let response = api
            .reencrypt_tenant_identity_secrets(request())
            .await
            .unwrap()
            .into_inner();
        assert_eq!(response.rows_updated, 0);
        assert_eq!(response.rows_skipped_all_on_target, 1);
    }
}
