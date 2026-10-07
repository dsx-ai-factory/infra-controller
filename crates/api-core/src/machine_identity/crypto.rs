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

//! Machine-identity encryption: Vault-backed AES keys for `tenant_identity_config` ciphertext
//! (signing private key + token delegation auth JSON), and parsing of stored delegation
//! `client_secret_basic` JSON for outbound token exchange.
//!
//! Decrypt uses the `key_id` embedded in each ciphertext envelope. Encrypt uses site
//! `[machine_identity].current_encryption_key_id`.

use carbide_instrument::{Event, LabelValue, emit};
use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
use carbide_secrets::key_encryption;
use model::tenant::{
    ClientSecretBasic, EncryptedTokenDelegationAuthConfig, EncryptionKeyId,
    TokenDelegationAuthMethod,
};
use tonic::Status;

use crate::CarbideError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum StoredMachineIdentitySecretKind {
    SigningKey,
    TokenDelegationAuth,
}

/// A tenant's stored machine-identity secret could not be decrypted. Each
/// variant is one kind of secret, and picks the wording that path logged.
#[derive(Event)]
#[event(
    event_name = "machine_identity_stored_secret_decryption_failed",
    metric_name = "carbide_machine_identity_stored_secret_decryption_failures_total",
    component = "nico-api",
    metric = counter,
    log = error,
    describe = "Number of stored machine identity secret decryption failures, by secret kind.",
    labels(secret_kind: StoredMachineIdentitySecretKind),
)]
pub(crate) enum MachineIdentityStoredSecretDecryptionFailed {
    #[event(labels(secret_kind = SigningKey), message = "tenant signing key decrypt failed")]
    SigningKey {
        #[context]
        organization_id: String,
        #[context]
        error: String,
    },

    #[event(
        labels(secret_kind = TokenDelegationAuth),
        message = "token delegation auth config decrypt failed"
    )]
    TokenDelegationAuth {
        #[context]
        organization_id: String,
        #[context]
        error: String,
    },
}

fn emit_stored_secret_decryption_failed(
    secret_kind: StoredMachineIdentitySecretKind,
    organization_id: &str,
    error: &Status,
) {
    let organization_id = organization_id.to_string();
    let error = error.message().to_string();
    emit(match secret_kind {
        StoredMachineIdentitySecretKind::SigningKey => {
            MachineIdentityStoredSecretDecryptionFailed::SigningKey {
                organization_id,
                error,
            }
        }
        StoredMachineIdentitySecretKind::TokenDelegationAuth => {
            MachineIdentityStoredSecretDecryptionFailed::TokenDelegationAuth {
                organization_id,
                error,
            }
        }
    });
}

pub(crate) async fn machine_identity_encryption_secret(
    credentials: &dyn CredentialReader,
    encryption_key_id: &EncryptionKeyId,
) -> Result<key_encryption::Aes256Key, Status> {
    machine_identity_encryption_secret_for_key_id(credentials, encryption_key_id.as_str()).await
}

async fn machine_identity_encryption_secret_for_key_id(
    credentials: &dyn CredentialReader,
    encryption_key_id: &str,
) -> Result<key_encryption::Aes256Key, Status> {
    let cred_key = CredentialKey::MachineIdentityEncryptionKey {
        key_id: encryption_key_id.to_string(),
    };
    let creds = credentials
        .get_credentials(&cred_key)
        .await
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()))?
        .ok_or_else(|| {
            CarbideError::InvalidArgument(format!(
                "encryption key '{encryption_key_id}' not found in secrets (machine_identity.encryption_keys)"
            ))
        })?;
    let stored = match &creds {
        Credentials::UsernamePassword { password, .. } => password.as_str(),
    };
    key_encryption::aes256_key_from_stored_secret(stored)
        .map_err(|e| CarbideError::InvalidArgument(e.to_string()).into())
}

/// Decrypts a machine-identity envelope using the `key_id` embedded in the blob.
pub(crate) async fn decrypt_machine_identity_ciphertext(
    credentials: &dyn CredentialReader,
    encrypted_base64: &str,
) -> Result<Vec<u8>, Status> {
    let envelope_key_id = key_encryption::envelope_key_id(encrypted_base64).map_err(|e| {
        CarbideError::internal(format!("stored ciphertext envelope is invalid: {e}"))
    })?;
    let aes = machine_identity_encryption_secret_for_key_id(credentials, &envelope_key_id).await?;
    key_encryption::decrypt(encrypted_base64, &aes)
        .map_err(|e| {
            CarbideError::internal(format!("stored ciphertext could not be decrypted: {e}"))
        })
        .map_err(Into::into)
}

/// Outcome of evaluating one encrypted blob for master-key re-wrap.
pub(crate) enum ReencryptBlobOutcome {
    SkippedOnTarget,
    DryRunWouldReencrypt,
    Reencrypted(String),
}

/// Re-wraps `ciphertext` when envelope `key_id` differs from `target_key_id`.
pub(crate) async fn reencrypt_ciphertext_if_needed(
    credentials: &dyn CredentialReader,
    ciphertext: &str,
    secret_kind: StoredMachineIdentitySecretKind,
    organization_id: &str,
    target_key_id: &EncryptionKeyId,
    target_aes: &key_encryption::Aes256Key,
    dry_run: bool,
) -> Result<ReencryptBlobOutcome, Status> {
    let current_key_id = key_encryption::envelope_key_id(ciphertext)
        .map_err(|e| -> Status {
            CarbideError::internal(format!("stored ciphertext envelope is invalid: {e}")).into()
        })
        .inspect_err(|error| {
            emit_stored_secret_decryption_failed(secret_kind, organization_id, error);
        })?;
    if current_key_id == target_key_id.as_str() {
        return Ok(ReencryptBlobOutcome::SkippedOnTarget);
    }
    let plaintext = decrypt_machine_identity_ciphertext(credentials, ciphertext)
        .await
        .inspect_err(|error| {
            emit_stored_secret_decryption_failed(secret_kind, organization_id, error);
        })?;
    if dry_run {
        return Ok(ReencryptBlobOutcome::DryRunWouldReencrypt);
    }
    let reencrypted = key_encryption::encrypt(&plaintext, target_aes, target_key_id.as_str())
        .map_err(|e| CarbideError::internal(format!("failed to reencrypt ciphertext: {e}")))?;
    Ok(ReencryptBlobOutcome::Reencrypted(reencrypted))
}

/// Decrypts `encrypted_auth_method_config` when set, otherwise `None`.
pub(crate) async fn decrypt_token_delegation_encrypted_blob(
    credentials: &dyn CredentialReader,
    encrypted_auth_method_config: Option<&EncryptedTokenDelegationAuthConfig>,
) -> Result<Option<String>, Status> {
    let Some(enc) = encrypted_auth_method_config else {
        return Ok(None);
    };
    if enc.as_str().is_empty() {
        return Ok(None);
    }
    let plain = decrypt_machine_identity_ciphertext(credentials, enc.as_str())
        .await
        .map_err(|e| {
            CarbideError::internal(format!(
                "stored token delegation configuration could not be decrypted: {}",
                e.message()
            ))
        })?;
    let utf8 = String::from_utf8(plain).map_err(|e| {
        CarbideError::internal(format!(
            "stored token delegation configuration plaintext was not valid UTF-8: {e}"
        ))
    })?;
    Ok(Some(utf8))
}

pub(crate) fn token_delegation_credentials(
    auth_method: TokenDelegationAuthMethod,
    plaintext_json: Option<&str>,
) -> Result<Option<(String, String)>, Status> {
    match auth_method {
        TokenDelegationAuthMethod::None => Ok(None),
        TokenDelegationAuthMethod::ClientSecretBasic => {
            let s = plaintext_json.ok_or_else(|| {
                CarbideError::internal(
                    "token delegation client credentials are missing".to_string(),
                )
            })?;
            let c: ClientSecretBasic = serde_json::from_str(s).map_err(|e| {
                CarbideError::internal(format!(
                    "stored token delegation client credentials are invalid: {e}"
                ))
            })?;
            if c.client_id.is_empty() || c.client_secret.is_empty() {
                return Err(CarbideError::internal(
                    "stored token delegation client credentials are incomplete".to_string(),
                )
                .into());
            }
            Ok(Some((c.client_id, c.client_secret)))
        }
    }
}

#[cfg(test)]
mod tests {
    use base64::Engine;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_secrets::credentials::{CredentialKey, CredentialWriter, Credentials};
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use carbide_test_support::{Check, check_values};
    use model::tenant::EncryptionKeyId;

    use super::*;

    const STORED_SECRET_DECRYPTION_FAILURE_METRIC: &str =
        "carbide_machine_identity_stored_secret_decryption_failures_total";

    #[derive(Debug, PartialEq)]
    struct StoredSecretDecryptionFailureObservation {
        counter_deltas: [f64; 2],
        log_count: usize,
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        secret_kind: Option<String>,
        organization_id: Option<String>,
        error_present: bool,
        exposes_sensitive_fields: bool,
    }

    #[test]
    fn stored_secret_decryption_failures_share_one_bounded_metric() {
        check_values(
            [
                Check {
                    scenario: "stored signing key",
                    input: StoredMachineIdentitySecretKind::SigningKey,
                    expect: StoredSecretDecryptionFailureObservation {
                        counter_deltas: [1.0, 0.0],
                        log_count: 1,
                        level: tracing::Level::ERROR,
                        metadata_name: "machine_identity_stored_secret_decryption_failed"
                            .to_string(),
                        message: "tenant signing key decrypt failed".to_string(),
                        event_name: Some(
                            "machine_identity_stored_secret_decryption_failed".to_string(),
                        ),
                        metric_name: Some(STORED_SECRET_DECRYPTION_FAILURE_METRIC.to_string()),
                        secret_kind: Some("signing_key".to_string()),
                        organization_id: Some("tenant-org".to_string()),
                        error_present: true,
                        exposes_sensitive_fields: false,
                    },
                },
                Check {
                    scenario: "token delegation authentication",
                    input: StoredMachineIdentitySecretKind::TokenDelegationAuth,
                    expect: StoredSecretDecryptionFailureObservation {
                        counter_deltas: [0.0, 1.0],
                        log_count: 1,
                        level: tracing::Level::ERROR,
                        metadata_name: "machine_identity_stored_secret_decryption_failed"
                            .to_string(),
                        message: "token delegation auth config decrypt failed".to_string(),
                        event_name: Some(
                            "machine_identity_stored_secret_decryption_failed".to_string(),
                        ),
                        metric_name: Some(STORED_SECRET_DECRYPTION_FAILURE_METRIC.to_string()),
                        secret_kind: Some("token_delegation_auth".to_string()),
                        organization_id: Some("tenant-org".to_string()),
                        error_present: true,
                        exposes_sensitive_fields: false,
                    },
                },
            ],
            |secret_kind| {
                let metrics = MetricsCapture::start();
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let source_key_id = "source-key";
                let target_key_id: EncryptionKeyId = "target-key".parse().unwrap();
                let source_aes =
                    key_encryption::aes256_key_from_stored_secret(&stored_secret(1)).unwrap();
                let target_aes =
                    key_encryption::aes256_key_from_stored_secret(&stored_secret(2)).unwrap();
                let ciphertext =
                    key_encryption::encrypt(b"stored secret", &source_aes, source_key_id).unwrap();
                let credentials = TestCredentialManager::default();
                runtime
                    .block_on(credentials.set_credentials(
                        &CredentialKey::MachineIdentityEncryptionKey {
                            key_id: source_key_id.to_string(),
                        },
                        &Credentials::UsernamePassword {
                            username: String::new(),
                            password: stored_secret(3),
                        },
                    ))
                    .unwrap();
                let mut rewrap_result = None;
                let logs = capture_logs(|| {
                    rewrap_result = Some(runtime.block_on(reencrypt_ciphertext_if_needed(
                        &credentials,
                        &ciphertext,
                        secret_kind,
                        "tenant-org",
                        &target_key_id,
                        &target_aes,
                        false,
                    )));
                });
                assert!(rewrap_result.expect("rewrap ran").is_err());
                let event_logs = logs
                    .iter()
                    .filter(|log| {
                        log.field("metric_name") == Some(STORED_SECRET_DECRYPTION_FAILURE_METRIC)
                    })
                    .collect::<Vec<_>>();
                let log = event_logs
                    .first()
                    .expect("decryption failure should emit one Event log");

                StoredSecretDecryptionFailureObservation {
                    counter_deltas: [
                        metrics.counter_delta(
                            STORED_SECRET_DECRYPTION_FAILURE_METRIC,
                            &[("secret_kind", "signing_key")],
                        ),
                        metrics.counter_delta(
                            STORED_SECRET_DECRYPTION_FAILURE_METRIC,
                            &[("secret_kind", "token_delegation_auth")],
                        ),
                    ],
                    log_count: event_logs.len(),
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    secret_kind: log.field("secret_kind").map(str::to_string),
                    organization_id: log.field("organization_id").map(str::to_string),
                    error_present: log.field("error").is_some_and(|error| !error.is_empty()),
                    exposes_sensitive_fields: [
                        "ciphertext",
                        "private_key",
                        "client_id",
                        "client_secret",
                        "plaintext",
                        "token",
                    ]
                    .into_iter()
                    .any(|field| log.field(field).is_some()),
                }
            },
        );
    }

    fn stored_secret(key_byte: u8) -> String {
        base64::engine::general_purpose::STANDARD.encode([key_byte; 32])
    }

    async fn test_credentials_with_keys(key_v1: &str, key_v2: &str) -> TestCredentialManager {
        let credentials = TestCredentialManager::default();
        for (key_id, byte) in [(key_v1, 1u8), (key_v2, 2u8)] {
            credentials
                .set_credentials(
                    &CredentialKey::MachineIdentityEncryptionKey {
                        key_id: key_id.to_string(),
                    },
                    &Credentials::UsernamePassword {
                        username: String::new(),
                        password: stored_secret(byte),
                    },
                )
                .await
                .unwrap();
        }
        credentials
    }

    #[tokio::test]
    async fn reencrypt_ciphertext_if_needed_skips_on_target_and_rewraps() {
        let key_v1 = "key-v1";
        let key_v2 = "key-v2";
        let credentials = test_credentials_with_keys(key_v1, key_v2).await;
        let aes_v1 = key_encryption::aes256_key_from_stored_secret(&stored_secret(1)).unwrap();
        let aes_v2 = key_encryption::aes256_key_from_stored_secret(&stored_secret(2)).unwrap();
        let plaintext = b"tenant signing key material";
        let ciphertext =
            key_encryption::encrypt(plaintext, &aes_v1, key_v1).expect("encrypt test blob");

        let target_v1: EncryptionKeyId = key_v1.parse().unwrap();
        let target_v2: EncryptionKeyId = key_v2.parse().unwrap();

        assert!(matches!(
            reencrypt_ciphertext_if_needed(
                &credentials,
                &ciphertext,
                StoredMachineIdentitySecretKind::SigningKey,
                "tenant-org",
                &target_v1,
                &aes_v1,
                false,
            )
            .await
            .unwrap(),
            ReencryptBlobOutcome::SkippedOnTarget
        ));

        assert!(matches!(
            reencrypt_ciphertext_if_needed(
                &credentials,
                &ciphertext,
                StoredMachineIdentitySecretKind::SigningKey,
                "tenant-org",
                &target_v2,
                &aes_v2,
                true,
            )
            .await
            .unwrap(),
            ReencryptBlobOutcome::DryRunWouldReencrypt
        ));

        let ReencryptBlobOutcome::Reencrypted(reencrypted) = reencrypt_ciphertext_if_needed(
            &credentials,
            &ciphertext,
            StoredMachineIdentitySecretKind::SigningKey,
            "tenant-org",
            &target_v2,
            &aes_v2,
            false,
        )
        .await
        .unwrap() else {
            panic!("expected Reencrypted outcome");
        };
        assert_eq!(
            key_encryption::envelope_key_id(&reencrypted).unwrap(),
            key_v2
        );
        let decrypted = decrypt_machine_identity_ciphertext(&credentials, &reencrypted)
            .await
            .unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn token_delegation_credentials_none_and_client_secret_basic() {
        assert!(
            token_delegation_credentials(TokenDelegationAuthMethod::None, None)
                .unwrap()
                .is_none()
        );
        let j = r#"{"client_id":"cid","client_secret":"csec"}"#;
        let got =
            token_delegation_credentials(TokenDelegationAuthMethod::ClientSecretBasic, Some(j))
                .unwrap()
                .unwrap();
        assert_eq!(got.0, "cid");
        assert_eq!(got.1, "csec");
        assert!(
            token_delegation_credentials(TokenDelegationAuthMethod::ClientSecretBasic, None)
                .is_err()
        );
    }
}
