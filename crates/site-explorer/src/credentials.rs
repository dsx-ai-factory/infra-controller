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

use std::sync::Arc;

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialManager, CredentialType, Credentials,
    REQUIRED_SITE_DEFAULT_CREDENTIAL_KEYS,
};
use mac_address::MacAddress;
use model::site_explorer::EndpointExplorationError;

use super::metrics::SiteExplorationMetrics;

pub(super) fn get_bmc_root_credential_key(bmc_mac_address: MacAddress) -> CredentialKey {
    CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot { bmc_mac_address },
    }
}

fn get_bmc_nvos_admin_credential_key(bmc_mac_address: MacAddress) -> CredentialKey {
    CredentialKey::SwitchNvosAdmin { bmc_mac_address }
}

#[derive(Clone)]
pub(super) struct CredentialClient {
    credential_manager: Arc<dyn CredentialManager>,
}

impl CredentialClient {
    fn valid_password(credentials: &Credentials) -> bool {
        let (_, password) = match credentials {
            Credentials::UsernamePassword { username, password } => (username, password),
        };

        if password.is_empty() {
            return false;
        }

        true
    }

    // TODO (spyda): fix the credential implementation for DPU and Host UEFI so that
    // we dont have to pass a validate boolean. We shouldnt store a username field in the
    // UEFI credential entry if its not relevant.
    async fn get_credentials(
        &self,
        credential_key: &CredentialKey,
    ) -> Result<Credentials, EndpointExplorationError> {
        match self
            .credential_manager
            .get_credentials(credential_key)
            .await
        {
            Ok(Some(credentials)) => {
                if !Self::valid_password(&credentials) {
                    return Err(EndpointExplorationError::Other {
                        details: format!(
                            "vault does not have a valid password entry at {}",
                            credential_key.to_key_str()
                        ),
                    });
                }

                Ok(credentials)
            }
            Ok(None) => Err(EndpointExplorationError::MissingCredentials {
                key: credential_key.to_key_str().to_string(),
                cause: "No credentials exists".to_string(),
            }),
            Err(err) => Err(EndpointExplorationError::SecretsEngineError {
                cause: err.to_string(),
            }),
        }
    }

    async fn set_credentials(
        &self,
        credential_key: &CredentialKey,
        credentials: &Credentials,
    ) -> Result<(), EndpointExplorationError> {
        match self
            .credential_manager
            .set_credentials(credential_key, credentials)
            .await
        {
            Ok(()) => Ok(()),
            Err(err) => Err(EndpointExplorationError::SetCredentials {
                key: credential_key.to_key_str().to_string(),
                cause: err.to_string(),
            }),
        }
    }

    pub(super) fn new(credential_manager: Arc<dyn CredentialManager>) -> Self {
        Self { credential_manager }
    }

    pub(super) async fn check_preconditions(
        &self,
        _metrics: &mut SiteExplorationMetrics,
    ) -> Result<(), EndpointExplorationError> {
        // The required site-wide default credentials (site-wide BMC root, DPU
        // UEFI, host UEFI) come from the shared canonical list so this check and
        // the admin UI's "default credentials not set" warning cannot drift.
        for credential_key in REQUIRED_SITE_DEFAULT_CREDENTIAL_KEYS {
            if let Some(e) = self.get_credentials(&credential_key).await.err() {
                return Err(EndpointExplorationError::MissingCredentials {
                    key: credential_key.to_key_str().to_string(),
                    cause: e.to_string(),
                });
            }
        }

        Ok(())
    }

    /// Read the site-wide BMC root credential at `version`. The caller resolves
    /// the current version from `sitewide_credential_rotation.target_version`
    /// (see [`super::bmc_endpoint_explorer::BmcEndpointExplorer`]); version 0 is
    /// the legacy unversioned path. There is no unversioned "current" alias --
    /// the rotation table is the single source of truth for which version is
    /// live.
    pub(super) async fn get_sitewide_bmc_root_credentials(
        &self,
        version: u32,
    ) -> Result<Credentials, EndpointExplorationError> {
        let key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::site_wide_root(version),
        };
        self.get_credentials(&key).await
    }

    /// Read the site-wide DPU BMC `service` password at `version`. The caller
    /// resolves the current version from
    /// `sitewide_credential_rotation.target_version` for the `dpu_bmc_service`
    /// family (see [`super::bmc_endpoint_explorer::BmcEndpointExplorer`]); version
    /// 0 is the legacy unversioned path.
    ///
    /// Only version 0 is lazily generated on first ingestion (`create_if_missing`).
    /// A versioned secret (>= 1) is staged by `RotateCredential` before the
    /// site-wide target advances, so a missing one is a real error rather than a
    /// cue to mint a divergent password that no other DPU would share.
    pub(super) async fn get_sitewide_dpu_bmc_service_password(
        &self,
        version: u32,
        create_if_missing: bool,
    ) -> Result<String, EndpointExplorationError> {
        let key = CredentialKey::BmcCredentials {
            credential_type: BmcCredentialType::site_wide_dpu_bmc_service(version),
        };

        match self.get_credentials(&key).await {
            Ok(Credentials::UsernamePassword { password, .. }) => Ok(password),
            Err(EndpointExplorationError::MissingCredentials { .. })
                if create_if_missing && version == 0 =>
            {
                let password = Credentials::generate_password();
                self.set_credentials(
                    &key,
                    &Credentials::UsernamePassword {
                        username: "service".to_string(),
                        password: password.clone(),
                    },
                )
                .await?;
                Ok(password)
            }
            Err(err) => Err(err),
        }
    }

    /// Returns the factory-default BMC credentials for a DPU of the given model.
    ///
    /// Lookup order:
    /// 1. Model-specific vault entry (`machines/all_dpus/factory_default/bmc-metadata-items/{model}`)
    /// 2. Catch-all vault entry (`machines/all_dpus/factory_default/bmc-metadata-items/root`,
    ///    i.e. `DpuModel::Unknown`) — skipped when `model` is already `Unknown`
    /// 3. Model's publicly-documented factory default (`DpuModel::default_factory_credentials`)
    ///
    /// Never fails: vault misses are silently swallowed and the hardcoded fallback is returned.
    pub(super) async fn get_dpu_factory_default_credentials(
        &self,
        model: bmc_vendor::DpuModel,
    ) -> Credentials {
        let model_key = CredentialKey::DpuRedfish {
            credential_type: CredentialType::DpuHardwareDefault { model },
        };
        if let Ok(creds) = self.get_credentials(&model_key).await {
            return creds;
        }

        if model != bmc_vendor::DpuModel::Unknown {
            let unknown_key = CredentialKey::DpuRedfish {
                credential_type: CredentialType::DpuHardwareDefault {
                    model: bmc_vendor::DpuModel::Unknown,
                },
            };
            if let Ok(creds) = self.get_credentials(&unknown_key).await {
                return creds;
            }
        }

        let (username, password) = model.default_factory_credentials();
        Credentials::UsernamePassword {
            username: username.to_string(),
            password: password.to_string(),
        }
    }

    pub(super) async fn get_bmc_root_credentials(
        &self,
        bmc_mac_address: MacAddress,
    ) -> Result<Credentials, EndpointExplorationError> {
        let bmc_root_credential_key = get_bmc_root_credential_key(bmc_mac_address);
        self.get_credentials(&bmc_root_credential_key).await
    }

    pub(super) async fn get_switch_nvos_admin_credentials(
        &self,
        bmc_mac_address: MacAddress,
    ) -> Result<Credentials, EndpointExplorationError> {
        let switch_nvos_admin_credential_key = get_bmc_nvos_admin_credential_key(bmc_mac_address);
        self.get_credentials(&switch_nvos_admin_credential_key)
            .await
    }

    pub(super) async fn set_bmc_root_credentials(
        &self,
        bmc_mac_address: MacAddress,
        credentials: &Credentials,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_root_credential_key = get_bmc_root_credential_key(bmc_mac_address);
        self.set_credentials(&bmc_root_credential_key, credentials)
            .await
    }

    pub(super) async fn set_bmc_nvos_admin_credentials(
        &self,
        bmc_mac_address: MacAddress,
        credentials: &Credentials,
    ) -> Result<(), EndpointExplorationError> {
        let bmc_nvos_admin_credential_key = get_bmc_nvos_admin_credential_key(bmc_mac_address);
        self.set_credentials(&bmc_nvos_admin_credential_key, credentials)
            .await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use carbide_secrets::credentials::{
        BmcCredentialType, CredentialKey, CredentialWriter, Credentials,
    };
    use carbide_secrets::test_support::credentials::TestCredentialManager;
    use model::site_explorer::EndpointExplorationError;

    use super::CredentialClient;
    use crate::metrics::SiteExplorationMetrics;

    #[tokio::test]
    async fn check_preconditions_should_not_count_missing_credentials_as_endpoint_failures() {
        let credential_client = CredentialClient::new(Arc::new(TestCredentialManager::default()));
        let mut metrics = SiteExplorationMetrics::new();

        let error = credential_client
            .check_preconditions(&mut metrics)
            .await
            .expect_err("missing site credentials should fail preconditions");

        assert!(matches!(
            error,
            EndpointExplorationError::MissingCredentials { .. }
        ));
        assert_eq!(metrics.endpoint_explorations, 0);
        assert_eq!(metrics.endpoint_explorations_success, 0);
        assert!(metrics.endpoint_explorations_failures_by_type.is_empty());
    }

    #[tokio::test]
    async fn get_sitewide_dpu_bmc_service_password_returns_existing() {
        let manager = Arc::new(TestCredentialManager::default());
        manager
            .set_credentials(
                &CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::SiteWideDpuBmcService,
                },
                &Credentials::UsernamePassword {
                    username: "service".to_string(),
                    password: "stored-service-pass".to_string(),
                },
            )
            .await
            .expect("preset dpu bmc service password");

        let client = CredentialClient::new(manager);
        let password = client
            .get_sitewide_dpu_bmc_service_password(0, false)
            .await
            .expect("existing dpu bmc service password");

        assert_eq!(password, "stored-service-pass");
    }

    #[tokio::test]
    async fn get_sitewide_dpu_bmc_service_password_creates_when_missing() {
        let client = CredentialClient::new(Arc::new(TestCredentialManager::default()));
        let password = client
            .get_sitewide_dpu_bmc_service_password(0, true)
            .await
            .expect("generated dpu bmc service password");

        assert!(!password.is_empty());
    }

    #[tokio::test]
    async fn get_sitewide_dpu_bmc_service_password_errors_when_missing_and_not_create() {
        let client = CredentialClient::new(Arc::new(TestCredentialManager::default()));
        let error = client
            .get_sitewide_dpu_bmc_service_password(0, false)
            .await
            .expect_err("missing dpu bmc service password should fail");

        assert!(matches!(
            error,
            EndpointExplorationError::MissingCredentials { .. }
        ));
    }

    #[tokio::test]
    async fn get_sitewide_dpu_bmc_service_password_is_stable_once_created() {
        let client = CredentialClient::new(Arc::new(TestCredentialManager::default()));
        let first = client
            .get_sitewide_dpu_bmc_service_password(0, true)
            .await
            .expect("first read creates site-wide DPU BMC service password");
        let second = client
            .get_sitewide_dpu_bmc_service_password(0, true)
            .await
            .expect("second read returns same site-wide DPU BMC service password");

        assert_eq!(first, second);
    }

    #[tokio::test]
    async fn get_sitewide_dpu_bmc_service_password_reads_versioned_path() {
        let manager = Arc::new(TestCredentialManager::default());
        manager
            .set_credentials(
                &CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::site_wide_dpu_bmc_service(2),
                },
                &Credentials::UsernamePassword {
                    username: "service".to_string(),
                    password: "v2-service-pass".to_string(),
                },
            )
            .await
            .expect("preset versioned dpu bmc service password");

        let client = CredentialClient::new(manager);
        let password = client
            .get_sitewide_dpu_bmc_service_password(2, false)
            .await
            .expect("existing versioned dpu bmc service password");

        assert_eq!(password, "v2-service-pass");
    }

    #[tokio::test]
    async fn get_sitewide_dpu_bmc_service_password_never_creates_versioned() {
        // A missing versioned (>= 1) secret is a staging error, never lazily
        // minted -- creating one here would diverge from the fleet's copy.
        let client = CredentialClient::new(Arc::new(TestCredentialManager::default()));
        let error = client
            .get_sitewide_dpu_bmc_service_password(2, true)
            .await
            .expect_err("a missing versioned service secret must not be created");

        assert!(matches!(
            error,
            EndpointExplorationError::MissingCredentials { .. }
        ));
    }
}
