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

//! Traits for pluggable credential providers.

use async_trait::async_trait;

use crate::client::ClientCredentials;
use crate::errors::MqtteaClientError;

/// A provider that can supply MQTT credentials (username + password).
///
/// This trait allows for pluggable authentication mechanisms. Implementations
/// can fetch credentials from various sources (static, OAuth2, Vault, etc.)
/// and handle token refresh automatically.
///
/// # Example
///
/// ```rust,ignore
/// use mqttea::auth::CredentialsProvider;
/// use mqttea::client::ClientCredentials;
///
/// struct MyOAuth2Provider {
///     // ... your OAuth2 client fields
/// }
///
/// #[async_trait::async_trait]
/// impl CredentialsProvider for MyOAuth2Provider {
///     async fn get_credentials(&self) -> Result<ClientCredentials, MqtteaClientError> {
///         let token = self.fetch_access_token().await?;
///         Ok(ClientCredentials {
///             username: "oauth2token".to_string(),
///             password: token,
///         })
///     }
/// }
/// ```
#[async_trait]
pub trait CredentialsProvider: Send + Sync + std::fmt::Debug {
    /// Get the current credentials for MQTT authentication.
    ///
    /// This method may perform network calls (e.g., to fetch OAuth2 tokens)
    /// and should handle caching and refresh internally.
    async fn get_credentials(&self) -> Result<ClientCredentials, MqtteaClientError>;
}

/// A provider that supplies only a token/password.
///
/// This trait is for authentication mechanisms that provide a token (like OAuth2)
/// which will be used as the MQTT password. The MQTT username is configured
/// separately when composing this with [`TokenCredentialsProvider`].
///
/// # Example
///
/// ```rust,ignore
/// use mqttea::auth::{TokenProvider, TokenCredentialsProvider};
///
/// struct MyTokenFetcher { /* ... */ }
///
/// #[async_trait::async_trait]
/// impl TokenProvider for MyTokenFetcher {
///     async fn get_token(&self) -> Result<String, MqtteaClientError> {
///         // Fetch token from some source
///         Ok("my-access-token".to_string())
///     }
/// }
///
/// // Compose with a username for MQTT
/// let provider = TokenCredentialsProvider::new(
///     Arc::new(MyTokenFetcher {}),
///     "oauth2token",
/// );
/// ```
#[async_trait]
pub trait TokenProvider: Send + Sync + std::fmt::Debug {
    /// Get the current token/password.
    ///
    /// This method may perform network calls (e.g., to fetch OAuth2 tokens)
    /// and should handle caching and refresh internally.
    async fn get_token(&self) -> Result<String, MqtteaClientError>;
}

/// A static credentials provider that never changes.
///
/// Use this for simple username/password authentication without token refresh.
#[derive(Debug, Clone)]
pub struct StaticCredentials {
    credentials: ClientCredentials,
}

impl StaticCredentials {
    pub fn new(username: impl Into<String>, password: impl Into<String>) -> Self {
        Self {
            credentials: ClientCredentials {
                username: username.into(),
                password: password.into(),
            },
        }
    }
}

#[async_trait]
impl CredentialsProvider for StaticCredentials {
    async fn get_credentials(&self) -> Result<ClientCredentials, MqtteaClientError> {
        Ok(self.credentials.clone())
    }
}

/// Combines a [`TokenProvider`] with a fixed username to create a [`CredentialsProvider`].
///
/// This is useful for OAuth2-based MQTT authentication where:
/// - The token provider fetches the OAuth2 access token (used as password)
/// - The username is a fixed value expected by the MQTT broker (e.g., "oauth2token")
///
/// # Example
///
/// ```rust,ignore
/// use std::sync::Arc;
/// use mqttea::auth::{TokenCredentialsProvider, OAuth2TokenProvider, OAuth2Config};
///
/// let oauth2_config = OAuth2Config::new(
///     "https://auth.example.com/token",
///     vec!["mqtt:publish".into()],
///     Duration::from_secs(30),
/// );
///
/// let token_provider = Arc::new(OAuth2TokenProvider::new(oauth2_config, client_credentials)?);
/// let credentials_provider = TokenCredentialsProvider::new("oauth2token", token_provider);
/// ```
#[derive(Debug)]
pub struct TokenCredentialsProvider<T: TokenProvider> {
    token_provider: T,
    username: String,
}

impl<T: TokenProvider> TokenCredentialsProvider<T> {
    /// Create a new TokenCredentialsProvider.
    ///
    /// # Arguments
    ///
    /// * `username` - The fixed username to use for MQTT authentication
    /// * `token_provider` - The provider that fetches tokens
    pub fn new(username: impl Into<String>, token_provider: T) -> Self {
        Self {
            token_provider,
            username: username.into(),
        }
    }
}

#[async_trait]
impl<T: TokenProvider> CredentialsProvider for TokenCredentialsProvider<T> {
    async fn get_credentials(&self) -> Result<ClientCredentials, MqtteaClientError> {
        let password = self.token_provider.get_token().await?;
        Ok(ClientCredentials {
            username: self.username.clone(),
            password,
        })
    }
}
