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

//! Authentication module for mqttea.
//!
//! This module provides pluggable authentication for MQTT connections:
//!
//! - [`CredentialsProvider`]: Trait for providers that supply username + password
//! - [`TokenProvider`]: Trait for providers that supply only a token (e.g., OAuth2 access token)
//! - [`TokenCredentialsProvider`]: Combines a [`TokenProvider`] with a fixed username
//! - [`StaticCredentials`]: Simple static username/password credentials
//! - [`OAuth2TokenProvider`]: OAuth2 client credentials flow (requires `oauth2` feature)
//!
//! # Example with OAuth2
//!
//! ```rust,ignore
//! use std::sync::Arc;
//! use std::time::Duration;
//! use mqttea::auth::{
//!     OAuth2Config, OAuth2TokenProvider, ClientCredentialsProvider,
//!     ClientId, ClientSecret, TokenCredentialsProvider,
//! };
//! use mqttea::{MqtteaClient, MqtteaClientError, ClientOptions};
//!
//! struct MyCredentials { /* secrets reader, credential key, etc. */ }
//!
//! #[async_trait::async_trait]
//! impl ClientCredentialsProvider for MyCredentials {
//!     async fn get_client_credentials(&self) -> Result<(ClientId, ClientSecret), MqtteaClientError> {
//!         Ok((ClientId::new("my-client-id"), ClientSecret::new("my-client-secret")))
//!     }
//! }
//!
//! let oauth2_config = OAuth2Config::new(
//!     "https://auth.example.com/oauth/token",
//!     vec!["mqtt:publish".into()],
//!     Duration::from_secs(30),
//! );
//!
//! let token_provider = OAuth2TokenProvider::new(
//!     oauth2_config,
//!     Arc::new(MyCredentials { }),
//! )?;
//!
//! // Combine with MQTT username
//! let credentials_provider = TokenCredentialsProvider::new(token_provider, "oauth2token");
//!
//! let options = ClientOptions::default()
//!     .with_credentials_provider(Arc::new(credentials_provider));
//!
//! let client = MqtteaClient::new("broker.example.com", 8883, "my-client", Some(options)).await?;
//! ```

mod oauth2_provider;
mod traits;

pub use oauth2::{ClientId, ClientSecret};
pub use oauth2_provider::{ClientCredentialsProvider, OAuth2Config, OAuth2TokenProvider};
pub use traits::{CredentialsProvider, StaticCredentials, TokenCredentialsProvider, TokenProvider};
