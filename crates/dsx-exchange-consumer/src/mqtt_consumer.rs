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

//! MQTT consumer that receives messages and writes them to a channel.

use std::sync::Arc;

use carbide_instrument::emit;
use carbide_secrets::credentials::CredentialReader;
use mqttea::QoS;
use mqttea::client::{ClientOptions, MqtteaClient};
use mqttea::registry::JsonRegistration;
use tokio::sync::mpsc;

use crate::DsxConsumerError;
use crate::config::{MqttAuthMode, MqttConfig};
use crate::messages::{LeakMetadata, ValueMessage};
use crate::metrics::{DroppedMessageType, MessageDropped, MessageReceived};

/// Topics of the BMS-published rack leak points. The patterns contain regex
/// metacharacters, so mqttea uses them as written instead of as topic suffixes.
const METADATA_TOPIC_PATTERN: &str = "/PUB/Metadata/Rack/(RackLeakDetect|RackLeakSensorFault)/";
const VALUE_TOPIC_PATTERN: &str = "/PUB/Value/Rack/(RackLeakDetect|RackLeakSensorFault)/";

/// Message types received from MQTT.
#[derive(Debug, Clone)]
pub enum MqttMessage {
    Metadata {
        topic: String,
        metadata: LeakMetadata,
    },
    Value {
        topic: String,
        value: ValueMessage,
    },
}

/// Connect to MQTT and return the processing channel's sender and receiver.
///
/// Sets up the MQTT client, registers message handlers and the client's
/// queue/publish/connection metrics on the meter, subscribes to topics,
/// and connects. Returns a sender handle (for the caller to observe the
/// channel's pending depth) alongside the receiver that yields messages with
/// drop-on-overflow.
pub async fn connect(
    config: &MqttConfig,
    meter: &opentelemetry::metrics::Meter,
    credential_reader: Arc<dyn CredentialReader>,
) -> Result<(mpsc::Sender<MqttMessage>, mpsc::Receiver<MqttMessage>), DsxConsumerError> {
    let (tx, rx) = mpsc::channel(config.queue_capacity);

    // QoS 0 is the recommended setting for DSX Exchange integrations.
    // BMS will republish all messages periodically to handle missed messages.
    let options = {
        let defaults = ClientOptions::default().with_qos(QoS::AtMostOnce);
        if let Some(provider) =
            build_credentials_provider(config, credential_reader.clone()).await?
        {
            defaults.with_credentials_provider(provider)
        } else {
            defaults
        }
    };

    // Suffix the broker-level client identifier so multiple replicas (or a new
    // pod coming up while the old one is still terminating) do not race for
    // the same MQTT session and ping-pong each other off the broker.
    let client_id = mqttea::unique_client_id(&config.client_id);
    let client = MqtteaClient::new(&config.endpoint, config.port, &client_id, Some(options))
        .await
        .map_err(|e| DsxConsumerError::Mqtt(e.to_string()))?;
    client.register_metrics(meter, "dsx_exchange_consumer");

    // Register message types with distinct topic patterns.
    client
        .register_json_message::<LeakMetadata>(METADATA_TOPIC_PATTERN.to_string())
        .await
        .map_err(|e| DsxConsumerError::Mqtt(e.to_string()))?;

    client
        .register_json_message::<ValueMessage>(VALUE_TOPIC_PATTERN.to_string())
        .await
        .map_err(|e| DsxConsumerError::Mqtt(e.to_string()))?;

    // Register handler for metadata messages
    client
        .on_message::<LeakMetadata, _, _>({
            let tx = tx.clone();
            move |_client, metadata, topic| {
                emit(MessageReceived);
                let msg = MqttMessage::Metadata { topic, metadata };
                if tx.try_send(msg).is_err() {
                    emit(MessageDropped {
                        message_type: DroppedMessageType::Metadata,
                    });
                }
                std::future::ready(())
            }
        })
        .await;

    // Register handler for value messages
    client
        .on_message::<ValueMessage, _, _>({
            let tx = tx.clone();
            move |_client, value, topic| {
                emit(MessageReceived);
                let msg = MqttMessage::Value { topic, value };
                if tx.try_send(msg).is_err() {
                    emit(MessageDropped {
                        message_type: DroppedMessageType::Value,
                    });
                }
                std::future::ready(())
            }
        })
        .await;

    // Subscribe to all topics under the prefix
    let subscribe_pattern = format!("{}/#", config.topic_prefix);
    client
        .subscribe(&subscribe_pattern, QoS::AtMostOnce)
        .await
        .map_err(|e| DsxConsumerError::Mqtt(e.to_string()))?;

    tracing::info!(topic = %subscribe_pattern, "Subscribed to MQTT topics");

    // Connect
    client
        .connect()
        .await
        .map_err(|e| DsxConsumerError::Mqtt(e.to_string()))?;

    tracing::info!("MQTT consumer connected");

    Ok((tx, rx))
}

async fn build_credentials_provider(
    config: &MqttConfig,
    credential_reader: Arc<dyn CredentialReader>,
) -> Result<Option<Arc<dyn mqttea::auth::CredentialsProvider>>, DsxConsumerError> {
    let credential_key = carbide_secrets::credentials::CredentialKey::MqttAuth {
        credential_type: carbide_secrets::credentials::MqttCredentialType::DsxExchangeConsumer,
    };

    match config.auth.auth_mode {
        MqttAuthMode::None => Ok(None),
        MqttAuthMode::BasicAuth => {
            let creds = credential_reader
                .get_credentials(&credential_key)
                .await
                .map_err(|e| DsxConsumerError::Secrets(e.to_string()))?
                .ok_or_else(|| {
                    DsxConsumerError::Secrets(format!(
                        "Missing MQTT credentials for {}",
                        credential_key.to_key_str()
                    ))
                })?;
            let carbide_secrets::credentials::Credentials::UsernamePassword { username, password } =
                creds;
            Ok(Some(Arc::new(mqttea::auth::StaticCredentials::new(
                username, password,
            ))
            // cast not needed by rustc, but satisfies rust-analyzer
            as Arc<dyn mqttea::auth::CredentialsProvider>))
        }
        MqttAuthMode::Oauth2 => {
            let oauth2 = config.auth.oauth2.as_ref().ok_or_else(|| {
                DsxConsumerError::Config(
                    "auth_mode is oauth2 but oauth2 config is missing".to_string(),
                )
            })?;
            let config = mqttea::auth::OAuth2Config::new(
                &oauth2.token_url,
                oauth2.scopes.clone(),
                oauth2.http_timeout,
            );
            let client_credentials = Arc::new(SecretBackedOAuth2Credentials {
                credential_key,
                credential_reader,
            });
            let token_provider = mqttea::auth::OAuth2TokenProvider::new(config, client_credentials)
                .map_err(|e| DsxConsumerError::Mqtt(e.to_string()))?;
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
    credential_key: carbide_secrets::credentials::CredentialKey,
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
        let carbide_secrets::credentials::Credentials::UsernamePassword { username, password } =
            creds;
        Ok((
            mqttea::ClientId::new(username),
            mqttea::ClientSecret::new(password),
        ))
    }
}

#[cfg(test)]
mod tests {
    use mqttea::MqttRegistry;

    use super::*;

    #[test]
    fn leak_topics_select_their_message_type() {
        use carbide_test_support::value_scenarios;

        let mut registry = MqttRegistry::new();
        registry
            .register_json_message::<LeakMetadata>(vec![METADATA_TOPIC_PATTERN.to_string()], None)
            .unwrap();
        registry
            .register_json_message::<ValueMessage>(vec![VALUE_TOPIC_PATTERN.to_string()], None)
            .unwrap();

        value_scenarios!(
            run = |topic: &str| {
                registry
                    .find_matching_type_for_topic(topic)
                    .map(|info| info.type_name.rsplit("::").next().unwrap().to_string())
            };
            "BMS-published rack leak points are routed by their topic" {
                "BMS/v1/PUB/Metadata/Rack/RackLeakDetect/site/rack02/leak"
                    => Some("LeakMetadata".to_string()),
                "BMS/v1/PUB/Metadata/Rack/RackLeakSensorFault/site/rack02/fault"
                    => Some("LeakMetadata".to_string()),
                "BMS/v1/PUB/Value/Rack/RackLeakDetect/site/rack02/leak"
                    => Some("ValueMessage".to_string()),
                "BMS/v1/PUB/Value/Rack/RackLeakSensorFault/site/rack02/fault"
                    => Some("ValueMessage".to_string()),
            }
            "other points and integration-published values are ignored" {
                "BMS/v1/PUB/Value/Rack/RackPower/site/rack02/power" => None,
                "BMS/v1/PUB/Metadata/Rack/RackLeakDetectTray/site/rack02/tray" => None,
                "BMS/v1/NICO/Value/Rack/RackLeakDetectTray/site/rack02/tray" => None,
                "BMS/v1/PUB/Value/Sensor/LeakDetect/site/pod/leak" => None,
            }
        );
    }
}
