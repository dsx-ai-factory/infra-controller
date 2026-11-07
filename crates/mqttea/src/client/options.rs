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

// src/client/options.rs
// Configuration options for the Mqttea client.
use std::sync::Arc;

use rumqttc::QoS;
use tokio::time::Duration;

use crate::auth::{CredentialsProvider, StaticCredentials};
use crate::registry::types::PublishOptions;

// ClientOptions are optional parameters that can be
// passed to the client, all of which are supposed
// to have default fallbacks.
//
// TODO(chet): This might be worthy of a ::new()
// or ::default() or something, even though
// passing None to the client just gets us
// const defaults anyway. Just trying to be flexible!
#[derive(Clone, Debug, Default)]
pub struct ClientOptions {
    // keep_alive sets the keepalive to use for MQTT broker connections.
    // Defaults to DEFAULT_KEEP_ALIVE.
    pub keep_alive: Option<std::time::Duration>,
    // message_channel_capacity is the number of *messages* the underlying
    // async client queue should buffer before no longer reading additional
    // bytes from the wire.
    // Defaults to DEFAULT_MESSAGE_CHANNEL_CAPACITY.
    pub message_channel_capacity: Option<usize>,
    // publish_options is used when no explicit PublishOptions are provided
    // for a given message type or topic pattern. If this is None, then
    // the default consts are used as fallback.
    pub publish_options: Option<PublishOptions>,
    // client_queue_size sets a limit to the number of messages that
    // can be buffered in our local client queue (between our event
    // loop and message processing tasks) before dropping.
    // Defaults to DEFAULT_CLIENT_QUEUE_SIZE.
    pub client_queue_size: Option<usize>,
    // warn_on_unmatched_topic will tell the client to log warnings
    // any time it encounters a topic pattern without a handler match.
    // Defaults to TRUE.
    pub warn_on_unmatched_topic: Option<bool>,
    // credentials_provider is an optional pluggable credentials provider
    // that can dynamically fetch credentials (e.g., OAuth2 tokens).
    pub credentials_provider: Option<Arc<dyn CredentialsProvider>>,
    // tls_config is an optional ClientTlsConfig to provide
    // for using TLS, and optionally, mTLS. This can be used
    // with or without credentials.
    pub tls_config: Option<ClientTlsConfig>,
    // max_concurrency is the maximum number of messages that can be
    // processed concurrently. If unset, defaults to 1, which is
    // effectively sequential processing.
    pub max_concurrency: Option<usize>,
}

impl ClientOptions {
    // Builder methods that consume and return Self
    pub fn with_keep_alive(mut self, keep_alive: Duration) -> Self {
        self.keep_alive = Some(keep_alive);
        self
    }

    pub fn with_message_channel_capacity(mut self, capacity: usize) -> Self {
        self.message_channel_capacity = Some(capacity);
        self
    }

    pub fn with_qos(mut self, qos: QoS) -> Self {
        // Initialize publish_options if None, then set qos
        let mut pub_opts = self.publish_options.unwrap_or_default();
        pub_opts.qos = Some(qos);
        self.publish_options = Some(pub_opts);
        self
    }

    pub fn with_retain(mut self, retain: bool) -> Self {
        let mut pub_opts = self.publish_options.unwrap_or_default();
        pub_opts.retain = Some(retain);
        self.publish_options = Some(pub_opts);
        self
    }

    pub fn with_publish_options(mut self, publish_options: PublishOptions) -> Self {
        self.publish_options = Some(publish_options);
        self
    }

    pub fn with_max_concurrency(mut self, max_concurrency: usize) -> Self {
        self.max_concurrency = Some(max_concurrency);
        self
    }

    /// Set a credentials provider for dynamic credential fetching.
    ///
    /// Use this for OAuth2 or other token-based authentication where
    /// credentials need to be refreshed.
    pub fn with_credentials_provider(mut self, provider: Arc<dyn CredentialsProvider>) -> Self {
        self.credentials_provider = Some(provider);
        self
    }

    /// Set static credentials for authentication.
    ///
    /// Creates a provider from the given credentials and sets it as the credentials provider.
    /// The provider will be used on reconnection as well.
    pub fn with_credentials(mut self, credentials: ClientCredentials) -> Self {
        // CredentialsProvider trait is implemented by StaticCredentials.
        let provider =
            StaticCredentials::new(credentials.username.clone(), credentials.password.clone());
        self.credentials_provider = Some(Arc::new(provider));
        self
    }
}

// ClientCredentials are used for providing a username
// and password to the MQTT server.
#[derive(Clone, Debug)]
pub struct ClientCredentials {
    pub username: String,
    pub password: String,
}

// ClientTlsConfig is config for using TLS (and optionally
// mTLS) with the MQTT server.
#[derive(Clone, Debug)]
pub struct ClientTlsConfig {
    // ca_certificate is PEM bytes for a CA certificate (or
    // CA certificate bundle); it is intended these were
    // probably loaded from a file, but could have also
    // been provided over the wire.
    pub ca_certificate: Vec<u8>,
    // client_identity is an optional client certificate
    // and private key to do mTLS with the MQTT server.
    pub client_identity: Option<ClientTlsIdentity>,
}

// ClientTlsIdentity is config to negotiate an mTLS
// handshake with the MQTT server.
#[derive(Clone, Debug)]
pub struct ClientTlsIdentity {
    // certificate is PEM bytes for a client certificate.
    // It is intended these were probably loaded from a
    // file, but could have also been provided over the
    // wire or generated ephemerally.
    pub certificate: Vec<u8>,
    // private_key is PEM bytes for the matching key.
    // It is intended these were probably loaded from a
    // file, but could have also been provided over the
    // wire or generated ephemerally.
    pub private_key: Vec<u8>,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[derive(Clone, Copy)]
    enum OptionsBuild {
        Default,
        KeepAlive,
        MessageCapacity,
        Qos,
        Retain,
        PublishOptions,
        MaxConcurrency,
        Credentials,
        CombinedPublishOptions,
    }

    #[derive(Debug, PartialEq)]
    struct OptionsSummary {
        keep_alive_secs: Option<u64>,
        message_channel_capacity: Option<usize>,
        qos: Option<QoS>,
        retain: Option<bool>,
        max_concurrency: Option<usize>,
        has_credentials_provider: bool,
    }

    fn build_options(build: OptionsBuild) -> ClientOptions {
        match build {
            OptionsBuild::Default => ClientOptions::default(),
            OptionsBuild::KeepAlive => {
                ClientOptions::default().with_keep_alive(Duration::from_secs(7))
            }
            OptionsBuild::MessageCapacity => {
                ClientOptions::default().with_message_channel_capacity(512)
            }
            OptionsBuild::Qos => ClientOptions::default().with_qos(QoS::AtLeastOnce),
            OptionsBuild::Retain => ClientOptions::default().with_retain(true),
            OptionsBuild::PublishOptions => ClientOptions::default().with_publish_options(
                PublishOptions::default()
                    .with_qos(QoS::ExactlyOnce)
                    .with_retain(false),
            ),
            OptionsBuild::MaxConcurrency => ClientOptions::default().with_max_concurrency(16),
            OptionsBuild::Credentials => {
                ClientOptions::default().with_credentials(ClientCredentials {
                    username: "user".to_string(),
                    password: "pass".to_string(),
                })
            }
            OptionsBuild::CombinedPublishOptions => ClientOptions::default()
                .with_qos(QoS::AtMostOnce)
                .with_retain(true),
        }
    }

    fn summarize(options: ClientOptions) -> OptionsSummary {
        let publish_options = options.publish_options.as_ref();

        OptionsSummary {
            keep_alive_secs: options.keep_alive.map(|duration| duration.as_secs()),
            message_channel_capacity: options.message_channel_capacity,
            qos: publish_options.and_then(|opts| opts.qos),
            retain: publish_options.and_then(|opts| opts.retain),
            max_concurrency: options.max_concurrency,
            has_credentials_provider: options.credentials_provider.is_some(),
        }
    }

    #[test]
    fn test_client_options_builders() {
        value_scenarios!(
            run = |build| summarize(build_options(build));
            "default" {
                OptionsBuild::Default => OptionsSummary {
                    keep_alive_secs: None,
                    message_channel_capacity: None,
                    qos: None,
                    retain: None,
                    max_concurrency: None,
                    has_credentials_provider: false,
                },
            }

            "keep alive" {
                OptionsBuild::KeepAlive => OptionsSummary {
                    keep_alive_secs: Some(7),
                    message_channel_capacity: None,
                    qos: None,
                    retain: None,
                    max_concurrency: None,
                    has_credentials_provider: false,
                },
            }

            "message capacity" {
                OptionsBuild::MessageCapacity => OptionsSummary {
                    keep_alive_secs: None,
                    message_channel_capacity: Some(512),
                    qos: None,
                    retain: None,
                    max_concurrency: None,
                    has_credentials_provider: false,
                },
            }

            "qos" {
                OptionsBuild::Qos => OptionsSummary {
                    keep_alive_secs: None,
                    message_channel_capacity: None,
                    qos: Some(QoS::AtLeastOnce),
                    retain: None,
                    max_concurrency: None,
                    has_credentials_provider: false,
                },
            }

            "retain" {
                OptionsBuild::Retain => OptionsSummary {
                    keep_alive_secs: None,
                    message_channel_capacity: None,
                    qos: None,
                    retain: Some(true),
                    max_concurrency: None,
                    has_credentials_provider: false,
                },
            }

            "publish options" {
                OptionsBuild::PublishOptions => OptionsSummary {
                    keep_alive_secs: None,
                    message_channel_capacity: None,
                    qos: Some(QoS::ExactlyOnce),
                    retain: Some(false),
                    max_concurrency: None,
                    has_credentials_provider: false,
                },
            }

            "max concurrency" {
                OptionsBuild::MaxConcurrency => OptionsSummary {
                    keep_alive_secs: None,
                    message_channel_capacity: None,
                    qos: None,
                    retain: None,
                    max_concurrency: Some(16),
                    has_credentials_provider: false,
                },
            }

            "credentials" {
                OptionsBuild::Credentials => OptionsSummary {
                    keep_alive_secs: None,
                    message_channel_capacity: None,
                    qos: None,
                    retain: None,
                    max_concurrency: None,
                    has_credentials_provider: true,
                },
            }

            "combined publish options" {
                OptionsBuild::CombinedPublishOptions => OptionsSummary {
                    keep_alive_secs: None,
                    message_channel_capacity: None,
                    qos: Some(QoS::AtMostOnce),
                    retain: Some(true),
                    max_concurrency: None,
                    has_credentials_provider: false,
                },
            }
        );
    }
}
