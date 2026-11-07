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

// tests/auth.rs
// Unit tests for authentication functionality including credentials providers,
// token providers, and OAuth2 support.

use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use mqttea::auth::{
    CredentialsProvider, StaticCredentials, TokenCredentialsProvider, TokenProvider,
};
use mqttea::errors::MqtteaClientError;

// =============================================================================
// Mock Implementations for Testing
// =============================================================================

/// A mock token provider for testing.
#[derive(Debug)]
struct MockTokenProvider {
    token: String,
    call_count: AtomicUsize,
}

impl MockTokenProvider {
    fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            call_count: AtomicUsize::new(0),
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl TokenProvider for MockTokenProvider {
    async fn get_token(&self) -> Result<String, MqtteaClientError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        Ok(self.token.clone())
    }
}

/// A mock token provider that returns errors.
#[derive(Debug)]
struct FailingTokenProvider;

#[async_trait]
impl TokenProvider for FailingTokenProvider {
    async fn get_token(&self) -> Result<String, MqtteaClientError> {
        Err(MqtteaClientError::CredentialsError(
            "Token fetch failed".to_string(),
        ))
    }
}

// =============================================================================
// StaticCredentials Tests
// =============================================================================

#[tokio::test]
async fn test_static_credentials() {
    let provider = StaticCredentials::new("user", "pass");
    let creds = provider.get_credentials().await.unwrap();

    assert_eq!(creds.username, "user");
    assert_eq!(creds.password, "pass");
}

#[tokio::test]
async fn test_static_credentials_multiple_calls() {
    let provider = StaticCredentials::new("user", "pass");

    // Multiple calls should return the same credentials
    for _ in 0..3 {
        let creds = provider.get_credentials().await.unwrap();
        assert_eq!(creds.username, "user");
        assert_eq!(creds.password, "pass");
    }
}

#[test]
fn test_static_credentials_debug() {
    let provider = StaticCredentials::new("user", "secret");
    let debug_output = format!("{:?}", provider);

    // Should be debuggable
    assert!(debug_output.contains("StaticCredentials"));
}

// =============================================================================
// TokenProvider Tests
// =============================================================================

#[tokio::test]
async fn test_mock_token_provider() {
    let provider = MockTokenProvider::new("my-access-token");

    let token = provider.get_token().await.unwrap();
    assert_eq!(token, "my-access-token");
    assert_eq!(provider.call_count(), 1);

    // Call again
    let token2 = provider.get_token().await.unwrap();
    assert_eq!(token2, "my-access-token");
    assert_eq!(provider.call_count(), 2);
}

// =============================================================================
// TokenCredentialsProvider Tests
// =============================================================================

#[tokio::test]
async fn test_token_credentials_provider() {
    let token_provider = MockTokenProvider::new("oauth2-access-token");
    let provider = TokenCredentialsProvider::new("oauth2token", token_provider);

    let creds = provider.get_credentials().await.unwrap();

    assert_eq!(creds.username, "oauth2token");
    assert_eq!(creds.password, "oauth2-access-token");
}

#[tokio::test]
async fn test_token_credentials_provider_custom_username() {
    let token_provider = MockTokenProvider::new("token123");
    let provider = TokenCredentialsProvider::new("custom_mqtt_user", token_provider);

    let creds = provider.get_credentials().await.unwrap();

    assert_eq!(creds.username, "custom_mqtt_user");
    assert_eq!(creds.password, "token123");
}

#[tokio::test]
async fn test_token_credentials_provider_propagates_errors() {
    let token_provider = FailingTokenProvider;
    let provider = TokenCredentialsProvider::new("user", token_provider);

    let result = provider.get_credentials().await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, MqtteaClientError::CredentialsError(_)));
}

#[test]
fn test_token_credentials_provider_debug() {
    let token_provider = MockTokenProvider::new("token");
    let provider = TokenCredentialsProvider::new("mqtt_user", token_provider);

    let debug_output = format!("{:?}", provider);

    assert!(debug_output.contains("TokenCredentialsProvider"));
    assert!(debug_output.contains("mqtt_user"));
}

// =============================================================================
// OAuth2 Tests
// =============================================================================

mod oauth2_tests {
    use std::sync::Arc;
    use std::time::Duration;

    use mqttea::MqtteaClientError;
    use mqttea::auth::{
        ClientCredentialsProvider, ClientId, ClientSecret, OAuth2Config, OAuth2TokenProvider,
    };

    struct StaticOAuth2Creds {
        client_id: String,
        client_secret: String,
    }

    #[async_trait::async_trait]
    impl ClientCredentialsProvider for StaticOAuth2Creds {
        async fn get_client_credentials(
            &self,
        ) -> Result<(ClientId, ClientSecret), MqtteaClientError> {
            Ok((
                ClientId::new(self.client_id.clone()),
                ClientSecret::new(self.client_secret.clone()),
            ))
        }
    }

    fn test_creds() -> Arc<dyn ClientCredentialsProvider> {
        Arc::new(StaticOAuth2Creds {
            client_id: "client123".into(),
            client_secret: "secret456".into(),
        })
    }

    #[test]
    fn test_oauth2_config_creation() {
        let config = OAuth2Config::new(
            "https://auth.example.com/token",
            vec!["mqtt:publish".into(), "mqtt:subscribe".into()],
            Duration::from_secs(60),
        );

        assert_eq!(config.token_url, "https://auth.example.com/token");
        assert_eq!(config.scopes, vec!["mqtt:publish", "mqtt:subscribe"]);
        assert_eq!(config.http_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_oauth2_config_empty_scopes() {
        let config = OAuth2Config::new(
            "https://auth.example.com/token",
            vec![],
            Duration::from_secs(30),
        );

        assert!(config.scopes.is_empty());
    }

    #[test]
    fn test_oauth2_provider_creation() {
        let config = OAuth2Config::new(
            "https://auth.example.com/token",
            vec![],
            Duration::from_secs(30),
        );

        let provider = OAuth2TokenProvider::new(config, test_creds());
        assert!(provider.is_ok());
    }

    #[test]
    fn test_oauth2_provider_with_scopes() {
        let config = OAuth2Config::new(
            "https://auth.example.com/token",
            vec!["scope1".into(), "scope2".into()],
            Duration::from_secs(30),
        );

        let provider = OAuth2TokenProvider::new(config, test_creds());
        assert!(provider.is_ok());
    }

    #[test]
    fn test_oauth2_provider_invalid_url() {
        let config = OAuth2Config::new("not-a-valid-url", vec![], Duration::from_secs(30));

        let provider = OAuth2TokenProvider::new(config, test_creds());
        assert!(provider.is_err());
    }

    #[test]
    fn test_oauth2_provider_debug() {
        let config = OAuth2Config::new(
            "https://auth.example.com/token",
            vec!["mqtt:publish".into()],
            Duration::from_secs(30),
        );

        let provider = OAuth2TokenProvider::new(config, test_creds()).unwrap();
        let debug_output = format!("{:?}", provider);

        assert!(debug_output.contains("OAuth2TokenProvider"));
        assert!(debug_output.contains("auth.example.com"));
    }
}
