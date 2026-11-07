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

use carbide_secrets::credentials::{CredentialKey, CredentialReader, Credentials};

use crate::cfg::file::{MqttAuthConfig, MqttAuthMode};

pub(crate) async fn build_credentials_provider(
    auth_config: &MqttAuthConfig,
    credential_key: CredentialKey,
    credential_reader: Arc<dyn CredentialReader>,
) -> eyre::Result<Option<Arc<dyn mqttea::auth::CredentialsProvider>>> {
    match auth_config.auth_mode {
        MqttAuthMode::None => Ok(None),
        MqttAuthMode::BasicAuth => {
            let creds = credential_reader
                .get_credentials(&credential_key)
                .await?
                .ok_or_else(|| {
                    eyre::eyre!(
                        "missing MQTT credentials for {}",
                        credential_key.to_key_str()
                    )
                })?;
            let Credentials::UsernamePassword { username, password } = creds;
            Ok(Some(Arc::new(mqttea::auth::StaticCredentials::new(
                username, password,
            ))
            // cast not needed by rustc, but satisfies rust-analyzer
            as Arc<dyn mqttea::auth::CredentialsProvider>))
        }
        MqttAuthMode::Oauth2 => {
            let oauth2 = auth_config
                .oauth2
                .as_ref()
                .ok_or_else(|| eyre::eyre!("auth_mode is oauth2 but oauth2 config is missing"))?;
            let config = mqttea::auth::OAuth2Config::new(
                &oauth2.token_url,
                oauth2.scopes.clone(),
                oauth2.http_timeout,
            );
            let client_credentials = Arc::new(SecretBackedOAuth2Credentials {
                credential_key,
                credential_reader,
            });
            let token_provider =
                mqttea::auth::OAuth2TokenProvider::new(config, client_credentials)?;
            let provider =
                mqttea::auth::TokenCredentialsProvider::new(&oauth2.username, token_provider);
            // cast not needed by rustc, but satisfies rust-analyzer
            Ok(Some(
                Arc::new(provider) as Arc<dyn mqttea::auth::CredentialsProvider>
            ))
        }
    }
}

struct SecretBackedOAuth2Credentials {
    credential_key: CredentialKey,
    credential_reader: Arc<dyn CredentialReader>,
}

#[async_trait::async_trait]
impl mqttea::auth::ClientCredentialsProvider for SecretBackedOAuth2Credentials {
    async fn get_client_credentials(
        &self,
    ) -> Result<(mqttea::ClientId, mqttea::ClientSecret), mqttea::MqtteaClientError> {
        let creds = self
            .credential_reader
            .get_credentials(&self.credential_key)
            .await
            .map_err(|e| mqttea::MqtteaClientError::CredentialsError(e.to_string()))?
            .ok_or_else(|| {
                mqttea::MqtteaClientError::CredentialsError(format!(
                    "Missing MQTT OAuth2 credentials for {}",
                    self.credential_key.to_key_str()
                ))
            })?;
        let Credentials::UsernamePassword { username, password } = creds;
        Ok((
            mqttea::ClientId::new(username),
            mqttea::ClientSecret::new(password),
        ))
    }
}
