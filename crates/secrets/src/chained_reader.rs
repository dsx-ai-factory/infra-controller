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
use async_trait::async_trait;

use crate::SecretsError;
use crate::credentials::{BmcCredentialType, CredentialKey, CredentialReader, Credentials};

pub struct ChainedCredentialReader(Vec<Box<dyn CredentialReader>>);

/// Operator action when a local UFM credential is active or required.
pub const UFM_LOCAL_CREDENTIAL_REMEDIATION: &str =
    // xtask:allow-error-case: UFM and NICo are product names
    "when environment credentials supply the UFM fabric, update them and restart NICo; otherwise \
     update the watched credential file (environment credentials take precedence over the file)";

/// Operator action for switching the entire site back to backend-managed UFM
/// credentials.
pub const UFM_BACKEND_SOURCE_REMEDIATION: &str = "to restore persistent backend ownership for all UFM fabrics, set \
     credentials.ufm_source = \"backend\" and restart NICo";

/// Operator action when local sources own version 0 of the site-wide BMC root.
pub const BMC_SITE_WIDE_ROOT_V0_LOCAL_CREDENTIAL_REMEDIATION: &str =
    // xtask:allow-error-case: NICo is a product name
    "before any managed device uses version 0, add or correct bmc_site_wide_root in the \
     configured environment or watched credential file (environment changes require a NICo \
     restart); after ingestion, keep version 0 unchanged; coordinated BMC credential rotation is \
     not safe while DPF manages DPUs with one shared BMC Secret; see \
     https://github.com/NVIDIA/infra-controller/issues/6147";

/// Operator action for returning version 0 of the site-wide BMC root to the
/// persistent backend.
pub const BMC_SITE_WIDE_ROOT_V0_BACKEND_SOURCE_REMEDIATION: &str =
    // xtask:allow-error-case: NICo is a product name
    "to restore persistent backend ownership for version 0, set \
     credentials.bmc_site_wide_root_source = \"backend\" and restart NICo";

/// Stops UFM credential lookup before the persistent backend portion of a
/// reader chain when local sources own UFM credentials. Place this after local
/// environment and file readers.
#[derive(Debug)]
pub struct UfmBackendCredentialBlocker;

/// Stops version 0 site-wide BMC root lookup before the persistent backend
/// when local sources own it. Versioned roots and all other BMC credentials
/// continue through the chain.
#[derive(Debug)]
pub struct BmcSiteWideRootV0BackendCredentialBlocker;

/// Delegates every non-UFM lookup while making local UFM entries invisible.
/// Place this around the environment/file chain when the persistent backend
/// owns UFM credentials.
pub struct NonUfmCredentialReader<R>(R);

/// Delegates every lookup except the version 0 site-wide BMC root, making that
/// local entry invisible when the persistent backend owns it.
pub struct NonBmcSiteWideRootV0CredentialReader<R>(R);

impl<R> NonUfmCredentialReader<R> {
    /// Wraps a reader and suppresses its `CredentialKey::UfmAuth` lookups.
    pub fn new(reader: R) -> Self {
        Self(reader)
    }
}

impl<R> NonBmcSiteWideRootV0CredentialReader<R> {
    /// Wraps a reader and suppresses only the unversioned site-wide BMC root.
    pub fn new(reader: R) -> Self {
        Self(reader)
    }
}

#[async_trait]
impl<R: CredentialReader> CredentialReader for NonUfmCredentialReader<R> {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        if matches!(key, CredentialKey::UfmAuth { .. }) {
            return Ok(None);
        }
        self.0.get_credentials(key).await
    }
}

#[async_trait]
impl<R: CredentialReader> CredentialReader for NonBmcSiteWideRootV0CredentialReader<R> {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        if matches!(
            key,
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::SiteWideRoot,
            }
        ) {
            return Ok(None);
        }
        self.0.get_credentials(key).await
    }
}

#[async_trait]
impl CredentialReader for UfmBackendCredentialBlocker {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        let CredentialKey::UfmAuth { fabric } = key else {
            return Ok(None);
        };

        tracing::error!(%fabric, "local UFM credential is missing");
        Err(SecretsError::UfmCredentialReadBlocked {
            fabric: fabric.clone(),
        })
    }
}

#[async_trait]
impl CredentialReader for BmcSiteWideRootV0BackendCredentialBlocker {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        if !matches!(
            key,
            CredentialKey::BmcCredentials {
                credential_type: BmcCredentialType::SiteWideRoot,
            }
        ) {
            return Ok(None);
        }

        // A local-only root may intentionally be supplied after startup. The
        // callers that require it decide whether and how to report the missing
        // credential, so keep this policy-chain signal below alerting levels.
        tracing::debug!("local site-wide BMC root version 0 credential is missing");
        Err(SecretsError::BmcSiteWideRootV0CredentialReadBlocked)
    }
}

impl From<Vec<Box<dyn CredentialReader>>> for ChainedCredentialReader {
    fn from(providers: Vec<Box<dyn CredentialReader>>) -> Self {
        Self(providers)
    }
}

#[async_trait]
impl CredentialReader for ChainedCredentialReader {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        for provider in &self.0 {
            if let Some(credentials) = provider.get_credentials(key).await? {
                return Ok(Some(credentials));
            }
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use serial_test::serial;

    use super::*;
    use crate::credentials::{BmcCredentialType, CredentialType};
    use crate::local_credentials::{
        EnvCredentials, EnvCredentialsConfig, FileCredentialsConfig, FileCredentialsWatcher,
    };
    use crate::test_support::credentials::TestCredentialManager;

    #[tokio::test]
    async fn empty_chain_returns_none() {
        let chain: ChainedCredentialReader = vec![].into();
        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };
        let value = chain.get_credentials(&key).await.expect("empty chain");
        assert_eq!(value, None);
    }

    #[tokio::test]
    async fn chained_reader_falls_through_to_last_provider() {
        let vault = Arc::new(TestCredentialManager::new(Credentials::UsernamePassword {
            username: "vault-user".to_string(),
            password: "vault-pass".to_string(),
        }));

        let chain: ChainedCredentialReader = vec![
            Box::new(
                EnvCredentials::new(EnvCredentialsConfig {
                    prefix: Some("CARBIDE_TEST_FALLTHRU_".to_string()),
                    ..Default::default()
                })
                .expect("create env provider"),
            ) as Box<dyn CredentialReader>,
            Box::new(vault),
        ]
        .into();

        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };
        let value = chain.get_credentials(&key).await.expect("get credentials");

        assert_eq!(
            value,
            Some(Credentials::UsernamePassword {
                username: "vault-user".to_string(),
                password: "vault-pass".to_string(),
            })
        );
    }

    #[tokio::test]
    async fn ufm_backend_blocker_stops_lookup_before_backend() {
        let backend = Arc::new(TestCredentialManager::new(Credentials::UsernamePassword {
            username: "vault-user".to_string(),
            password: "vault-pass".to_string(),
        }));
        let chain: ChainedCredentialReader = vec![
            Box::new(UfmBackendCredentialBlocker) as Box<dyn CredentialReader>,
            Box::new(backend),
        ]
        .into();
        let key = CredentialKey::UfmAuth {
            fabric: "fabric-a".to_string(),
        };

        let error = chain
            .get_credentials(&key)
            .await
            .expect_err("UFM lookup must stop before the persistent backend");

        let message = error.to_string();
        assert!(matches!(
            error,
            SecretsError::UfmCredentialReadBlocked { ref fabric } if fabric == "fabric-a"
        ));
        assert!(message.contains("fabric-a"));
        assert!(message.contains(UFM_LOCAL_CREDENTIAL_REMEDIATION));
    }

    #[tokio::test]
    async fn ufm_backend_blocker_allows_other_credentials_to_continue() {
        let expected = Credentials::UsernamePassword {
            username: "vault-user".to_string(),
            password: "vault-pass".to_string(),
        };
        let backend = Arc::new(TestCredentialManager::new(expected.clone()));
        let chain: ChainedCredentialReader = vec![
            Box::new(UfmBackendCredentialBlocker) as Box<dyn CredentialReader>,
            Box::new(backend),
        ]
        .into();
        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };

        let value = chain
            .get_credentials(&key)
            .await
            .expect("non-UFM lookup must continue to the backend");

        assert_eq!(value, Some(expected));
    }

    #[tokio::test]
    async fn non_ufm_reader_hides_only_ufm_credentials() {
        let expected = Credentials::UsernamePassword {
            username: "local-user".to_string(),
            password: "local-password".to_string(),
        };
        let reader = NonUfmCredentialReader::new(TestCredentialManager::new(expected.clone()));

        let ufm_value = reader
            .get_credentials(&CredentialKey::UfmAuth {
                fabric: "fabric-a".to_string(),
            })
            .await
            .expect("ignore local UFM credential");
        let non_ufm_value = reader
            .get_credentials(&CredentialKey::DpuUefi {
                credential_type: CredentialType::SiteDefault,
            })
            .await
            .expect("read local non-UFM credential");

        assert_eq!(ufm_value, None);
        assert_eq!(non_ufm_value, Some(expected));
    }

    #[tokio::test]
    async fn bmc_site_wide_root_v0_backend_blocker_is_exact() {
        let expected = Credentials::new("backend-user", "backend-password");
        let backend = Arc::new(TestCredentialManager::new(expected.clone()));
        let chain: ChainedCredentialReader = vec![
            Box::new(BmcSiteWideRootV0BackendCredentialBlocker) as Box<dyn CredentialReader>,
            Box::new(backend),
        ]
        .into();
        let v0 = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRoot,
        };
        let v1 = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRootVersioned { version: 1 },
        };

        assert!(matches!(
            chain.get_credentials(&v0).await,
            Err(SecretsError::BmcSiteWideRootV0CredentialReadBlocked)
        ));
        assert_eq!(
            chain
                .get_credentials(&v1)
                .await
                .expect("versioned BMC root lookup must continue to the backend"),
            Some(expected)
        );
    }

    #[tokio::test]
    async fn non_bmc_site_wide_root_v0_reader_is_exact() {
        let expected = Credentials::new("local-user", "local-password");
        let reader =
            NonBmcSiteWideRootV0CredentialReader::new(TestCredentialManager::new(expected.clone()));
        let v0 = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRoot,
        };
        let v1 = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::SiteWideRootVersioned { version: 1 },
        };

        assert_eq!(
            reader
                .get_credentials(&v0)
                .await
                .expect("ignore local version 0"),
            None
        );
        assert_eq!(
            reader
                .get_credentials(&v1)
                .await
                .expect("read local versioned BMC root"),
            Some(expected)
        );
    }

    #[tokio::test]
    #[serial]
    async fn env_takes_precedence_over_file_and_vault() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("static-creds.yaml");
        tokio::fs::write(
            &file_path,
            r#"dpu_uefi_site_default:
  username: file-user
  password: file-password
"#,
        )
        .await
        .expect("write static credential file");

        let vault = Arc::new(TestCredentialManager::new(Credentials::UsernamePassword {
            username: "vault-user".to_string(),
            password: "vault-password".to_string(),
        }));
        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };

        let env_user = "CARBIDE_TEST_PREC__DPU_UEFI_SITE_DEFAULT__USERNAME";
        let env_password = "CARBIDE_TEST_PREC__DPU_UEFI_SITE_DEFAULT__PASSWORD";
        // SAFETY: Initial lint enablement: `#[serial]` serializes participating tests,
        // but it cannot prove Unix process-wide exclusion from unmarked environment
        // readers. This needs owner review.
        unsafe {
            std::env::set_var(env_user, "env-user");
            std::env::set_var(env_password, "env-password");
        }

        let chain: ChainedCredentialReader = vec![
            Box::new(
                EnvCredentials::new(EnvCredentialsConfig {
                    prefix: Some("CARBIDE_TEST_PREC_".to_string()),
                    ..Default::default()
                })
                .expect("create env provider"),
            ) as Box<dyn CredentialReader>,
            Box::new(
                FileCredentialsWatcher::new(FileCredentialsConfig {
                    path: Some(file_path.clone()),
                    poll_interval: Some(Duration::from_millis(250)),
                    ..Default::default()
                })
                .await
                .expect("create file provider"),
            ),
            Box::new(vault.clone()),
        ]
        .into();

        let env_value = chain.get_credentials(&key).await.expect("get env value");
        assert_eq!(
            env_value,
            Some(Credentials::UsernamePassword {
                username: "env-user".to_string(),
                password: "env-password".to_string(),
            })
        );

        // SAFETY: Initial lint enablement: `#[serial]` serializes participating tests,
        // but it cannot prove Unix process-wide exclusion from unmarked environment
        // readers. This needs owner review.
        unsafe {
            std::env::remove_var(env_user);
            std::env::remove_var(env_password);
        }
    }

    #[tokio::test]
    async fn file_takes_precedence_over_vault() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let file_path = dir.path().join("static-creds.yaml");
        tokio::fs::write(
            &file_path,
            r#"dpu_uefi_site_default:
  username: file-user
  password: file-password
"#,
        )
        .await
        .expect("write static credential file");

        let vault = Arc::new(TestCredentialManager::new(Credentials::UsernamePassword {
            username: "vault-user".to_string(),
            password: "vault-password".to_string(),
        }));
        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };

        let chain: ChainedCredentialReader = vec![
            Box::new(
                FileCredentialsWatcher::new(FileCredentialsConfig {
                    path: Some(file_path),
                    poll_interval: Some(Duration::from_millis(250)),
                    ..Default::default()
                })
                .await
                .expect("create file provider"),
            ) as Box<dyn CredentialReader>,
            Box::new(vault),
        ]
        .into();

        let file_value = chain.get_credentials(&key).await.expect("get file value");
        assert_eq!(
            file_value,
            Some(Credentials::UsernamePassword {
                username: "file-user".to_string(),
                password: "file-password".to_string(),
            })
        );
    }
}
