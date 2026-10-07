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
use figment::Figment;
use figment::providers::{Env, Serialized};
use serde::{Deserialize, Serialize};

use super::CredentialSnapshot;
use crate::SecretsError;
use crate::credentials::{CredentialKey, CredentialReader, Credentials};

const DEFAULT_ENV_PREFIX: &str = "CARBIDE_STATIC_CREDENTIAL_";

#[derive(Default, Clone, Debug, Deserialize, Serialize)]
#[serde(default)]
pub struct EnvCredentialsConfig {
    pub enabled: Option<bool>,
    pub prefix: Option<String>,
}

impl EnvCredentialsConfig {
    pub fn enabled(&self) -> bool {
        self.enabled
            .or_else(|| {
                std::env::var("CARBIDE_CREDENTIALS_ENV_ENABLED")
                    .ok()
                    .and_then(|v| v.parse().ok())
            })
            .unwrap_or(false)
    }

    pub fn prefix(&self) -> String {
        self.prefix
            .clone()
            .or_else(|| std::env::var("CARBIDE_CREDENTIALS_ENV_PREFIX").ok())
            .unwrap_or_else(|| DEFAULT_ENV_PREFIX.to_string())
    }
}

#[derive(Clone, Debug)]
pub struct EnvCredentials {
    snapshot: CredentialSnapshot,
}

impl EnvCredentials {
    pub fn new(config: EnvCredentialsConfig) -> Result<Self, SecretsError> {
        let prefix = config.prefix();
        let env_prefix = format!("{}__", prefix.trim_end_matches('_'));
        let snapshot: CredentialSnapshot = Figment::new()
            .merge(Serialized::defaults(CredentialSnapshot::default()))
            .merge(Env::prefixed(&env_prefix).split("__"))
            .extract()
            .map_err(|err| {
                SecretsError::GenericError(eyre::eyre!(
                    "invalid static credentials from env prefix {env_prefix}: {err}"
                ))
            })?;
        Ok(Self { snapshot })
    }
}

#[async_trait]
impl CredentialReader for EnvCredentials {
    async fn get_credentials(
        &self,
        key: &CredentialKey,
    ) -> Result<Option<Credentials>, SecretsError> {
        Ok(self.snapshot.get_credentials(key))
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;
    use crate::credentials::{CredentialKey, CredentialType, Credentials};

    fn env_name(parts: &[&str]) -> String {
        let mut name = "CARBIDE_STATIC_CREDENTIAL".to_string();
        for part in parts {
            name.push_str("__");
            name.push_str(part);
        }
        name
    }

    #[tokio::test]
    // Mutates process environment variables. Keep serialized to avoid cross-test interference.
    #[serial]
    async fn parses_credentials_from_env_config_style() {
        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };
        let user_env = env_name(&["DPU_UEFI_SITE_DEFAULT", "USERNAME"]);
        let pass_env = env_name(&["DPU_UEFI_SITE_DEFAULT", "PASSWORD"]);

        // SAFETY: Initial lint enablement: `#[serial]` serializes participating tests,
        // but it cannot prove Unix process-wide exclusion from unmarked environment
        // readers. This needs owner review.
        unsafe {
            std::env::set_var(&user_env, "operator");
            std::env::set_var(&pass_env, "secret");
        }
        let provider =
            EnvCredentials::new(EnvCredentialsConfig::default()).expect("create env provider");
        let credentials = provider
            .get_credentials(&key)
            .await
            .expect("parse env credentials");
        // SAFETY: Initial lint enablement: `#[serial]` serializes participating tests,
        // but it cannot prove Unix process-wide exclusion from unmarked environment
        // readers. This needs owner review.
        unsafe {
            std::env::remove_var(&user_env);
            std::env::remove_var(&pass_env);
        }

        assert_eq!(
            credentials,
            Some(Credentials::UsernamePassword {
                username: "operator".to_string(),
                password: "secret".to_string(),
            })
        );
    }

    #[tokio::test]
    // Mutates process environment variables. Keep serialized to avoid cross-test interference.
    #[serial]
    async fn parses_ufm_credentials_from_documented_environment_names() {
        let key = CredentialKey::UfmAuth {
            fabric: "default".to_string(),
        };
        let user_env = env_name(&["UFM_AUTH_BY_FABRIC", "DEFAULT", "USERNAME"]);
        let pass_env = env_name(&["UFM_AUTH_BY_FABRIC", "DEFAULT", "PASSWORD"]);

        // SAFETY: Initial lint enablement: `#[serial]` serializes participating tests,
        // but it cannot prove Unix process-wide exclusion from unmarked environment
        // readers. This needs owner review.
        unsafe {
            std::env::set_var(&user_env, "operator");
            std::env::set_var(&pass_env, "token");
        }
        let provider =
            EnvCredentials::new(EnvCredentialsConfig::default()).expect("create env provider");
        let credentials = provider
            .get_credentials(&key)
            .await
            .expect("parse UFM env credentials");
        // SAFETY: Initial lint enablement: `#[serial]` serializes participating tests,
        // but it cannot prove Unix process-wide exclusion from unmarked environment
        // readers. This needs owner review.
        unsafe {
            std::env::remove_var(&user_env);
            std::env::remove_var(&pass_env);
        }

        assert_eq!(
            credentials,
            Some(Credentials::UsernamePassword {
                username: "operator".to_string(),
                password: "token".to_string(),
            })
        );
    }

    #[tokio::test]
    // Mutates process environment variables. Keep serialized to avoid cross-test interference.
    #[serial]
    async fn snapshots_env_at_startup() {
        let key = CredentialKey::DpuUefi {
            credential_type: CredentialType::SiteDefault,
        };
        let user_env = env_name(&["DPU_UEFI_SITE_DEFAULT", "USERNAME"]);
        let pass_env = env_name(&["DPU_UEFI_SITE_DEFAULT", "PASSWORD"]);

        // SAFETY: Initial lint enablement: `#[serial]` serializes participating tests,
        // but it cannot prove Unix process-wide exclusion from unmarked environment
        // readers. This needs owner review.
        unsafe {
            std::env::set_var(&user_env, "operator");
            std::env::set_var(&pass_env, "initial");
        }

        let provider =
            EnvCredentials::new(EnvCredentialsConfig::default()).expect("create env provider");

        // SAFETY: Initial lint enablement: `#[serial]` serializes participating tests,
        // but it cannot prove Unix process-wide exclusion from unmarked environment
        // readers. This needs owner review.
        unsafe {
            std::env::set_var(&pass_env, "updated");
        }

        let credentials = provider
            .get_credentials(&key)
            .await
            .expect("read startup snapshot");

        // SAFETY: Initial lint enablement: `#[serial]` serializes participating tests,
        // but it cannot prove Unix process-wide exclusion from unmarked environment
        // readers. This needs owner review.
        unsafe {
            std::env::remove_var(&user_env);
            std::env::remove_var(&pass_env);
        }

        assert_eq!(
            credentials,
            Some(Credentials::UsernamePassword {
                username: "operator".to_string(),
                password: "initial".to_string(),
            })
        );
    }
}
