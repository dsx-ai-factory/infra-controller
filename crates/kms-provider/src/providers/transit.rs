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

//! TransitKmsProvider implements KmsBackend using the Transit secrets
//! engine API. Compatible with both HashiCorp Vault and OpenBao. The kek_id
//! maps to a named Transit key, and the server handles all crypto; the
//! key material (KEK) never leaves it.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use carbide_instrument::{Event, LabelValue, emit, red};
use vaultrs::api::transit::requests::DataKeyType;
use vaultrs::client::VaultClient;
use vaultrs::transit;
use zeroize::{Zeroize, Zeroizing};

use crate::{EncryptedDek, KmsBackend, KmsError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum TokenMaintenanceStage {
    Lookup,
    Renew,
}

/// Transit KMS token maintenance failed. Each variant is the stage that broke.
#[derive(Event)]
#[event(
    event_name = "kms_token_maintenance_failed",
    metric_name = "carbide_kms_token_maintenance_failures_total",
    component = "carbide-kms-provider",
    metric = counter,
    describe = "Number of Transit KMS token maintenance failures, by maintenance stage",
    labels(stage: TokenMaintenanceStage),
)]
enum TokenMaintenanceFailed {
    #[event(
        labels(stage = TokenMaintenanceStage::Lookup),
        log = warn,
        message = "failed to look up Transit KMS token; retrying"
    )]
    Lookup {
        #[context]
        error: String,
    },

    #[event(
        labels(stage = TokenMaintenanceStage::Renew),
        log = warn,
        message = "failed to renew Transit KMS vault token"
    )]
    Renew {
        #[context]
        error: String,
    },
}
/// DEFAULT_TRANSIT_MOUNT is the default Transit
/// secrets engine mount path.
pub const DEFAULT_TRANSIT_MOUNT: &str = "transit";

/// TransitKmsProvider wraps the Transit secrets
/// engine API to provide envelope encryption.
/// Compatible with both HashiCorp Vault and OpenBao.
/// Each kek_id corresponds to a named Transit key
/// on the server.
pub struct TransitKmsProvider {
    client: Arc<VaultClient>,
    transit_mount: String,
    known_keys: Vec<String>,
}

impl TransitKmsProvider {
    /// TransitKmsProvider::new creates a provider from
    /// an authenticated VaultClient, mount path, and
    /// list of known Transit key names. The known_keys
    /// list is used by can_decrypt_kek to determine
    /// ownership without a network call.
    pub fn new(client: Arc<VaultClient>, transit_mount: String, known_keys: Vec<String>) -> Self {
        tracing::info!(
            transit_mount = %transit_mount,
            known_keys = ?known_keys,
            "initialized Transit KMS provider"
        );
        Self {
            client,
            transit_mount,
            known_keys,
        }
    }

    /// run_token_renewal returns a future that renews
    /// the Vault token at 90% (0.9) of each lease
    /// duration until `cancel` fires. The caller spawns
    /// it -- typically onto the process JoinSet, so
    /// shutdown actually stops it.
    pub fn run_token_renewal(
        &self,
        cancel: tokio_util::sync::CancellationToken,
    ) -> impl Future<Output = ()> + Send + 'static {
        let client = self.client.clone();
        async move {
            // Look up the token's TTL and renewability, retrying until it
            // answers -- a vault blip while carbide-api boots must not
            // disable renewal for the life of the process.
            let info = loop {
                match vaultrs::token::lookup_self(client.as_ref()).await {
                    Ok(info) => break info,
                    Err(e) => {
                        emit(TokenMaintenanceFailed::Lookup {
                            error: e.to_string(),
                        });
                        tokio::select! {
                            _ = cancel.cancelled() => return,
                            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
                        }
                    }
                }
            };

            if info.renewable.is_none_or(|renewable| !renewable) {
                tracing::info!("Transit KMS token is not renewable, skipping renewal loop");
                return;
            }

            let mut next_renewal = Duration::from_secs((info.ttl as f64 * 0.9).max(30.0) as u64);
            loop {
                tracing::debug!(
                    sleep_seconds = next_renewal.as_secs(),
                    "scheduling Transit KMS token renewal"
                );
                tokio::select! {
                    _ = cancel.cancelled() => return,
                    _ = tokio::time::sleep(next_renewal) => {}
                }

                match vaultrs::token::renew_self(client.as_ref(), None).await {
                    Ok(renewed) => {
                        next_renewal = Duration::from_secs(
                            (renewed.lease_duration as f64 * 0.9).max(30.0) as u64,
                        );
                        tracing::info!(
                            new_lease_duration_seconds = renewed.lease_duration,
                            "renewed Transit KMS vault token"
                        );
                    }
                    Err(e) => {
                        emit(TokenMaintenanceFailed::Renew {
                            error: e.to_string(),
                        });
                        next_renewal = Duration::from_secs(30);
                    }
                }
            }
        }
    }
}

#[async_trait]
impl KmsBackend for TransitKmsProvider {
    async fn encrypt_dek(&self, kek_id: &str, dek: &[u8; 32]) -> Result<EncryptedDek, KmsError> {
        // The base64 string is another copy of the DEK; zeroize it like the
        // decoded buffers.
        let plaintext_b64 = Zeroizing::new(BASE64.encode(dek));
        let response = red::instrumented(
            "vault_transit",
            "encrypt_dek",
            transit::data::encrypt(
                self.client.as_ref(),
                &self.transit_mount,
                kek_id,
                &plaintext_b64,
                None,
            ),
        )
        .await
        .map_err(|e| KmsError::EncryptionFailed(format!("vault transit encrypt: {e}")))?;

        // Vault Transit returns ciphertext as a string like
        // "vault:v1:<base64>". Store the entire string as bytes and pass it
        // back verbatim on decrypt.
        Ok(EncryptedDek {
            ciphertext: response.ciphertext.into_bytes(),
            nonce: vec![], // Transit manages nonces internally.
        })
    }

    async fn decrypt_dek(
        &self,
        kek_id: &str,
        encrypted: &EncryptedDek,
    ) -> Result<Zeroizing<[u8; 32]>, KmsError> {
        let ciphertext_str = std::str::from_utf8(&encrypted.ciphertext)
            .map_err(|_| KmsError::DecryptionFailed("invalid ciphertext encoding".to_string()))?;

        let response = red::instrumented(
            "vault_transit",
            "decrypt_dek",
            transit::data::decrypt(
                self.client.as_ref(),
                &self.transit_mount,
                kek_id,
                ciphertext_str,
                None,
            ),
        )
        .await
        .map_err(|e| KmsError::DecryptionFailed(format!("vault transit decrypt: {e}")))?;

        // Vault returns base64-encoded plaintext: one more copy of the DEK,
        // zeroized along with the decoded buffer.
        let plaintext_b64 = Zeroizing::new(response.plaintext);
        let mut decoded = BASE64
            .decode(plaintext_b64.as_bytes())
            .map_err(|e| KmsError::DecryptionFailed(format!("invalid base64 from vault: {e}")))?;
        let len = decoded.len();
        let dek: [u8; 32] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| KmsError::DecryptionFailed(format!("DEK has wrong length: {len}")))?;
        decoded.zeroize();
        Ok(Zeroizing::new(dek))
    }

    fn can_decrypt_kek(&self, kek_id: &str) -> bool {
        self.known_keys.iter().any(|k| k == kek_id)
    }

    fn kek_ids(&self) -> Vec<String> {
        self.known_keys.clone()
    }

    async fn generate_and_wrap_dek(
        &self,
        kek_id: &str,
    ) -> Result<(Zeroizing<[u8; 32]>, EncryptedDek), KmsError> {
        let response = red::instrumented(
            "vault_transit",
            "generate_data_key",
            transit::generate::data_key(
                self.client.as_ref(),
                &self.transit_mount,
                kek_id,
                DataKeyType::Plaintext,
                None,
            ),
        )
        .await
        .map_err(|e| KmsError::EncryptionFailed(format!("vault transit generate data key: {e}")))?;

        let plaintext_b64 = Zeroizing::new(response.plaintext.ok_or_else(|| {
            KmsError::Other("vault returned no plaintext for data key".to_string())
        })?);

        let mut decoded = BASE64
            .decode(plaintext_b64.as_bytes())
            .map_err(|e| KmsError::Other(format!("invalid base64 from vault: {e}")))?;
        let len = decoded.len();
        let dek: [u8; 32] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| KmsError::Other(format!("DEK has wrong length: {len}")))?;
        decoded.zeroize();

        let wrapped = EncryptedDek {
            ciphertext: response.ciphertext.into_bytes(),
            nonce: vec![], // Transit manages nonces internally.
        };

        Ok((Zeroizing::new(dek), wrapped))
    }
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::process::Stdio;

    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};
    use serial_test::serial;
    use tokio::io::AsyncBufReadExt;
    use tokio::process;

    use super::*;

    const TOKEN_MAINTENANCE_FAILURE_METRIC: &str = "carbide_kms_token_maintenance_failures_total";

    #[derive(Debug, PartialEq)]
    struct TokenMaintenanceFailureObservation {
        log_count: usize,
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        stage: Option<String>,
        error: Option<String>,
        exposes_sensitive_identity: bool,
        counter_delta: f64,
    }

    #[test]
    fn token_maintenance_failures_emit_metrics_and_preserve_warning_logs() {
        check_values(
            [
                Check {
                    scenario: "token lookup failure",
                    input: TokenMaintenanceStage::Lookup,
                    expect: TokenMaintenanceFailureObservation {
                        log_count: 1,
                        level: tracing::Level::WARN,
                        metadata_name: "kms_token_maintenance_failed".to_string(),
                        message: "failed to look up Transit KMS token; retrying".to_string(),
                        event_name: Some("kms_token_maintenance_failed".to_string()),
                        metric_name: Some(TOKEN_MAINTENANCE_FAILURE_METRIC.to_string()),
                        stage: Some("lookup".to_string()),
                        error: Some("vault unavailable".to_string()),
                        exposes_sensitive_identity: false,
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "token renewal failure",
                    input: TokenMaintenanceStage::Renew,
                    expect: TokenMaintenanceFailureObservation {
                        log_count: 1,
                        level: tracing::Level::WARN,
                        metadata_name: "kms_token_maintenance_failed".to_string(),
                        message: "failed to renew Transit KMS vault token".to_string(),
                        event_name: Some("kms_token_maintenance_failed".to_string()),
                        metric_name: Some(TOKEN_MAINTENANCE_FAILURE_METRIC.to_string()),
                        stage: Some("renew".to_string()),
                        error: Some("vault unavailable".to_string()),
                        exposes_sensitive_identity: false,
                        counter_delta: 1.0,
                    },
                },
            ],
            |stage| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    let error = "vault unavailable".to_string();
                    emit(match stage {
                        TokenMaintenanceStage::Lookup => TokenMaintenanceFailed::Lookup { error },
                        TokenMaintenanceStage::Renew => TokenMaintenanceFailed::Renew { error },
                    });
                });
                let log = logs.first().expect("failure Event should log once");

                TokenMaintenanceFailureObservation {
                    log_count: logs.len(),
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    stage: log.field("stage").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                    exposes_sensitive_identity: ["token", "transit_mount", "kek_id", "key"]
                        .into_iter()
                        .any(|field| log.field(field).is_some()),
                    counter_delta: metrics.counter_delta(
                        TOKEN_MAINTENANCE_FAILURE_METRIC,
                        &[(
                            "stage",
                            match stage {
                                TokenMaintenanceStage::Lookup => "lookup",
                                TokenMaintenanceStage::Renew => "renew",
                            },
                        )],
                    ),
                }
            },
        );
    }

    /// VaultDev holds a running Vault dev server
    /// for testing.
    struct VaultDev {
        _process: process::Child,
        client: Arc<VaultClient>,
    }

    /// start_vault_dev starts a Vault dev server
    /// with Transit enabled.
    async fn start_vault_dev() -> Option<VaultDev> {
        // Check if vault binary is available.
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
            .filter_map(|dir| {
                let candidate = dir.join("vault");
                candidate.is_file().then_some(candidate)
            })
            .next()?;

        let addr = {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
            listener.local_addr().expect("local addr")
        };

        let mut proc = process::Command::new("vault")
            .arg("server")
            .arg("-dev")
            .arg(format!("-dev-listen-address={addr}"))
            .env_remove("VAULT_ADDR")
            .env_remove("VAULT_CLIENT_KEY")
            .env_remove("VAULT_CLIENT_CERT")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .expect("start vault");

        // Parse root token from stdout.
        let stdout = tokio::io::BufReader::new(proc.stdout.take().unwrap());
        let stderr = proc.stderr.take().unwrap();
        tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr).lines();
            while let Some(line) = lines.next_line().await.ok().flatten() {
                eprintln!("[vault-stderr] {line}");
            }
        });

        let (token_tx, token_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let mut lines = stdout.lines();
            let mut sender = Some(token_tx);
            while let Some(line) = lines.next_line().await.ok().flatten() {
                if let Some(rest) = line.trim().strip_prefix("Root Token:")
                    && let Some(s) = sender.take()
                {
                    s.send(rest.trim().to_string()).ok();
                }
            }
        });

        let token = token_rx.await.expect("vault token");

        // Create VaultClient.
        let settings = vaultrs::client::VaultClientSettingsBuilder::default()
            .token(token)
            .address(format!("http://{addr}"))
            .build()
            .expect("vault settings");
        let client = Arc::new(VaultClient::new(settings).expect("vault client"));

        // Enable Transit secrets engine.
        vaultrs::sys::mount::enable(client.as_ref(), "transit", "transit", None)
            .await
            .expect("enable transit");

        // Create a test key.
        transit::key::create(client.as_ref(), "transit", "test-key", None)
            .await
            .expect("create transit key");

        Some(VaultDev {
            _process: proc,
            client,
        })
    }

    // Verifies that encrypt_dek + decrypt_dek
    // round-trips through Transit.
    #[tokio::test]
    #[serial]
    async fn transit_encrypt_decrypt_round_trip() {
        let vault = match start_vault_dev().await {
            Some(v) => v,
            None => {
                eprintln!("vault not available, skipping test");
                return;
            }
        };

        let provider = TransitKmsProvider::new(
            vault.client.clone(),
            "transit".to_string(),
            vec!["test-key".to_string()],
        );

        let dek: [u8; 32] = rand::random();
        let encrypted = provider
            .encrypt_dek("test-key", &dek)
            .await
            .expect("encrypt");
        let decrypted = provider
            .decrypt_dek("test-key", &encrypted)
            .await
            .expect("decrypt");

        assert_eq!(*decrypted, dek);
    }

    // Verifies that generate_and_wrap_dek produces
    // a DEK that can be unwrapped.
    #[tokio::test]
    #[serial]
    async fn transit_generate_and_wrap_round_trip() {
        let vault = match start_vault_dev().await {
            Some(v) => v,
            None => {
                eprintln!("vault not available, skipping test");
                return;
            }
        };

        let provider = TransitKmsProvider::new(
            vault.client.clone(),
            "transit".to_string(),
            vec!["test-key".to_string()],
        );

        let (dek, wrapped) = provider
            .generate_and_wrap_dek("test-key")
            .await
            .expect("generate");
        let unwrapped = provider
            .decrypt_dek("test-key", &wrapped)
            .await
            .expect("unwrap");

        assert_eq!(*dek, *unwrapped);
    }

    // Verifies that decrypting with a nonexistent
    // key returns an error.
    #[tokio::test]
    #[serial]
    async fn transit_decrypt_nonexistent_key_errors() {
        let vault = match start_vault_dev().await {
            Some(v) => v,
            None => {
                eprintln!("vault not available, skipping test");
                return;
            }
        };

        let provider = TransitKmsProvider::new(
            vault.client.clone(),
            "transit".to_string(),
            vec!["test-key".to_string()],
        );

        let dek: [u8; 32] = rand::random();
        let encrypted = provider
            .encrypt_dek("test-key", &dek)
            .await
            .expect("encrypt");

        // Try decrypting with a different key name.
        let result = provider.decrypt_dek("nonexistent-key", &encrypted).await;
        assert!(result.is_err());
    }
}
