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

use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use std::time::Duration;

use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};
use tokio::fs::{self, File, OpenOptions};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

const UNIFIED_PREINGESTION_BFB_PATH: &str =
    "/forge-boot-artifacts/blobs/internal/aarch64/preingestion_unified_update.bfb";
const PREINGESTION_BFB_PATH: &str = "/forge-boot-artifacts/blobs/internal/aarch64/preingestion.bfb";

#[derive(Debug, thiserror::Error)]
pub(crate) enum BfbRshimCopyError {
    #[error("missing credential {key}: {cause}")]
    MissingCredentials { key: String, cause: String },
    #[error("secrets engine error occurred: {cause}")]
    SecretsEngineError { cause: String },
    #[error("error: {details}")]
    Other { details: String },
}

pub(crate) struct BfbRshimCopier {
    credential_reader: Option<Arc<dyn CredentialReader>>,
    bfb_file_lock: Arc<Mutex<()>>,
}

impl BfbRshimCopier {
    pub(crate) fn new(credential_reader: Option<Arc<dyn CredentialReader>>) -> Self {
        Self {
            credential_reader,
            bfb_file_lock: Arc::new(Mutex::new(())),
        }
    }

    fn valid_bmc_password(credentials: &Credentials) -> bool {
        let (_, password) = match credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        !password.is_empty()
    }

    async fn get_bmc_root_credentials(
        &self,
        credential_key: &CredentialKey,
    ) -> Result<Credentials, BfbRshimCopyError> {
        let Some(credential_reader) = &self.credential_reader else {
            return Err(BfbRshimCopyError::MissingCredentials {
                key: credential_key.to_key_str().to_string(),
                cause: "credential reader is not configured".to_string(),
            });
        };

        match credential_reader.get_credentials(credential_key).await {
            Ok(Some(credentials)) => {
                if !Self::valid_bmc_password(&credentials) {
                    return Err(BfbRshimCopyError::Other {
                        details: format!(
                            "vault does not have a valid password entry at {}",
                            credential_key.to_key_str()
                        ),
                    });
                }

                Ok(credentials)
            }
            Ok(None) => Err(BfbRshimCopyError::MissingCredentials {
                key: credential_key.to_key_str().to_string(),
                cause: "No credentials exists".to_string(),
            }),
            Err(err) => Err(BfbRshimCopyError::SecretsEngineError {
                cause: err.to_string(),
            }),
        }
    }

    async fn is_rshim_enabled(
        &self,
        bmc_ip_address: std::net::SocketAddr,
        credentials: Credentials,
    ) -> Result<bool, BfbRshimCopyError> {
        let (username, password) = match credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        forge_ssh::ssh::is_rshim_enabled(bmc_ip_address, username, password)
            .await
            .map_err(|err| BfbRshimCopyError::Other {
                details: format!("failed query RSHIM status on on {bmc_ip_address}: {err}"),
            })
    }

    async fn enable_rshim(
        &self,
        bmc_ip_address: std::net::SocketAddr,
        credentials: Credentials,
    ) -> Result<(), BfbRshimCopyError> {
        let (username, password) = match credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        forge_ssh::ssh::enable_rshim(bmc_ip_address, username, password)
            .await
            .map_err(|err| BfbRshimCopyError::Other {
                details: format!("failed enable RSHIM on {bmc_ip_address}: {err}"),
            })
    }

    async fn check_and_enable_rshim(
        &self,
        bmc_ip_address: std::net::SocketAddr,
        credentials: &Credentials,
    ) -> Result<(), BfbRshimCopyError> {
        let mut i = 0;
        while i < 3 {
            if !self
                .is_rshim_enabled(bmc_ip_address, credentials.clone())
                .await?
            {
                tracing::warn!(%bmc_ip_address, "RSHIM is not enabled");
                self.enable_rshim(bmc_ip_address, credentials.clone())
                    .await?;

                // Sleep for 10 seconds before checking again
                tokio::time::sleep(Duration::from_secs(10)).await;
                i += 1;
            } else {
                return Ok(());
            }
        }

        Err(BfbRshimCopyError::Other {
            details: format!("could not enable RSHIM on {bmc_ip_address}"),
        })
    }

    async fn create_unified_preingestion_bfb(
        &self,
        username: &str,
        password: &str,
    ) -> Result<(), BfbRshimCopyError> {
        let _lock = self.bfb_file_lock.lock().await;

        Self::write_unified_preingestion_bfb(
            PREINGESTION_BFB_PATH,
            UNIFIED_PREINGESTION_BFB_PATH,
            username,
            password,
        )
        .await
    }

    /// Assemble the unified pre-ingestion BFB by concatenating the base BFB with
    /// a `bf.cfg` blob carrying the BMC credentials.
    ///
    /// The `bf.cfg` blob embeds the BMC root password in cleartext, so the
    /// artifact must be readable only by its owner (`0o600`). `.mode(0o600)` on
    /// the `open` keeps the file from ever being group/other-readable at
    /// creation, but it is still filtered by the process umask (e.g. it resolves
    /// to `0o000` under a `0o777` umask), so the mode is then pinned exactly with
    /// `set_permissions` — which `chmod`s the fd and is not umask-dependent —
    /// before any secret bytes are written.
    async fn write_unified_preingestion_bfb(
        source_bfb_path: &str,
        unified_bfb_path: &str,
        username: &str,
        password: &str,
    ) -> Result<(), BfbRshimCopyError> {
        if fs::metadata(unified_bfb_path).await.is_err() {
            tracing::info!(path = unified_bfb_path, "Writing unified preingestion BFB");
            let bf_cfg_contents = format!(
                "BMC_USER=\"{username}\"\nBMC_PASSWORD=\"{password}\"\nBMC_REBOOT=\"yes\"\nCEC_REBOOT=\"yes\"\n"
            );

            let mut preingestion_bfb =
                File::open(source_bfb_path)
                    .await
                    .map_err(|err| BfbRshimCopyError::Other {
                        details: format!("failed to open {source_bfb_path}: {err}"),
                    })?;

            let mut unified_bfb = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(unified_bfb_path)
                .await
                .map_err(|err| BfbRshimCopyError::Other {
                    details: format!("failed to create {unified_bfb_path}: {err}"),
                })?;

            // Pin the mode to exactly 0o600 regardless of the process umask,
            // before the cleartext credentials are written to the file.
            unified_bfb
                .set_permissions(std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|err| BfbRshimCopyError::Other {
                    details: format!("failed to set permissions on {unified_bfb_path}: {err}"),
                })?;

            let mut buffer = vec![0; 1024 * 1024].into_boxed_slice(); // 1 MB buffer

            loop {
                let n = preingestion_bfb.read(&mut buffer).await.map_err(|err| {
                    BfbRshimCopyError::Other {
                        details: format!("failed to read BFB: {err}"),
                    }
                })?;

                if n == 0 {
                    break;
                }

                unified_bfb.write_all(&buffer[..n]).await.map_err(|err| {
                    BfbRshimCopyError::Other {
                        details: format!("failed to write BFB to {unified_bfb_path}: {err}"),
                    }
                })?;
            }

            unified_bfb
                .write_all(bf_cfg_contents.as_bytes())
                .await
                .map_err(|err| BfbRshimCopyError::Other {
                    details: format!("failed to write bf.cfg: {err}"),
                })?;

            unified_bfb
                .sync_all()
                .await
                .map_err(|err| BfbRshimCopyError::Other {
                    details: format!("failed to flush {unified_bfb_path}: {err}"),
                })?;
        }

        Ok(())
    }

    pub(crate) async fn copy_bfb_to_dpu_rshim(
        &self,
        bmc_ip_address: std::net::SocketAddr,
        credential_key: &CredentialKey,
    ) -> Result<(), BfbRshimCopyError> {
        let credentials = self.get_bmc_root_credentials(credential_key).await?;
        let (username, password) = match credentials.clone() {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        self.create_unified_preingestion_bfb(&username, &password)
            .await?;

        self.check_and_enable_rshim(bmc_ip_address, &credentials)
            .await?;

        forge_ssh::ssh::copy_bfb_to_bmc_rshim(
            bmc_ip_address,
            username,
            password,
            UNIFIED_PREINGESTION_BFB_PATH.to_string(),
        )
        .await
        .map_err(|err| BfbRshimCopyError::Other {
            details: format!(
                "failed to copy BFB from {UNIFIED_PREINGESTION_BFB_PATH} to BMC RSHIM on {bmc_ip_address}: {err}"
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;

    // The umask is process-global (shared across threads on Linux), so tests
    // that depend on or mutate it must not run concurrently. Serialize them
    // behind one lock, and use plain `#[test]` + `block_on` so the guard is
    // never held across an `.await` (which `clippy::await_holding_lock` forbids).
    static UMASK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Restores the process umask on drop so a failing assertion cannot leak a
    /// restrictive umask into other tests.
    struct UmaskGuard(libc::mode_t);

    impl UmaskGuard {
        fn set(mask: libc::mode_t) -> Self {
            // SAFETY: `umask` accepts every `mode_t` value and dereferences no pointers. The
            // caller holds `UMASK_TEST_LOCK`, serializing this process-global mutation.
            let previous = unsafe { libc::umask(mask) };
            Self(previous)
        }
    }

    impl Drop for UmaskGuard {
        fn drop(&mut self) {
            // SAFETY: `self.0` was returned by `umask`; every `mode_t` value is accepted, and
            // the call dereferences no pointers.
            unsafe { libc::umask(self.0) };
        }
    }

    fn run_case(umask_value: libc::mode_t) {
        // Poisoning is fine here: `UmaskGuard` still restores the umask on a
        // panicking test, so recover the guard rather than cascade failures.
        let _serial = UMASK_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build current-thread runtime")
            .block_on(assert_owner_only_artifact(umask_value));
    }

    /// Writes the unified BFB while the given umask is in effect and asserts the
    /// artifact is owner-only (`0o600`) and carries the base BFB plus the
    /// credential-bearing `bf.cfg` blob.
    async fn assert_owner_only_artifact(umask_value: libc::mode_t) {
        let dir = tempfile::tempdir().expect("create tempdir");
        let source_path = dir.path().join("preingestion.bfb");
        let unified_path = dir.path().join("preingestion_unified_update.bfb");

        let base_payload = b"base-bfb-payload";
        fs::write(&source_path, base_payload)
            .await
            .expect("write source bfb");

        // Fixtures above are created under the ambient umask so they stay
        // readable; only the write-under-test runs with the restrictive umask.
        let umask_guard = UmaskGuard::set(umask_value);

        BfbRshimCopier::write_unified_preingestion_bfb(
            source_path.to_str().expect("utf-8 source path"),
            unified_path.to_str().expect("utf-8 unified path"),
            "root",
            "s3cr3t-p@ssw0rd",
        )
        .await
        .expect("write unified bfb");

        drop(umask_guard);

        assert_bf_cfg_contents(&unified_path, base_payload).await;
    }

    async fn assert_bf_cfg_contents(unified_path: &Path, base_payload: &[u8]) {
        let mode = fs::metadata(unified_path)
            .await
            .expect("stat unified bfb")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "unified BFB must be owner-only, got {:o}",
            mode & 0o777
        );

        // The base BFB is concatenated with the bf.cfg blob carrying the creds.
        let written = fs::read(unified_path).await.expect("read unified bfb");
        assert!(
            written.starts_with(base_payload),
            "unified BFB must start with the base BFB payload"
        );
        let bf_cfg = String::from_utf8_lossy(&written[base_payload.len()..]);
        assert!(bf_cfg.contains("BMC_USER=\"root\""));
        assert!(bf_cfg.contains("BMC_PASSWORD=\"s3cr3t-p@ssw0rd\""));
    }

    /// The unified BFB embeds the BMC root password in cleartext, so the
    /// assembled artifact must be readable only by its owner (`0o600`).
    #[test]
    fn write_unified_preingestion_bfb_creates_owner_only_artifact() {
        run_case(0o022);
    }

    /// Regression for the umask-filtering gap: with only `.mode(0o600)` on the
    /// `open`, a `0o777` umask leaves the artifact `0o000`. The explicit
    /// `set_permissions` must still pin it to `0o600`.
    #[test]
    fn write_unified_preingestion_bfb_pins_mode_under_restrictive_umask() {
        run_case(0o777);
    }
}
