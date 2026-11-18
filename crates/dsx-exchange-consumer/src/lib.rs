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

//! DSX Exchange Consumer microservice for BMS leak detection events.
//!
//! This service consumes leak detection events from the BMS MQTT event bus
//! and updates rack-level health overrides in the Carbide API.

use std::sync::Arc;

pub mod api_client;
pub mod config;
pub mod health_updater;
pub mod messages;
pub mod metrics;
pub mod mqtt_consumer;

pub use config::Config;

use crate::api_client::{ApiClientWrapper, ConsoleRackHealthSink};
use crate::health_updater::HealthUpdater;

#[derive(thiserror::Error, Debug)]
pub enum DsxConsumerError {
    #[error("API call failed: {0}")]
    Api(#[from] tonic::Status),

    #[error("configuration invalid: {0}")]
    Config(String),

    #[error("MQTT operation failed: {0}")]
    Mqtt(String),

    #[error("metrics setup failed: {0}")]
    Metrics(String),

    #[error("secrets error: {0}")]
    Secrets(String),
}

/// Builds the MQTT credential source chain: environment, then file, then Vault.
/// With `auth_mode = none` no credential is read, so no source is constructed
/// and no Vault, file, or environment setting is required. Vault joins the
/// chain only when a Vault setting names it (`VAULT_ADDR`).
async fn build_credential_reader(
    mqtt: &config::MqttConfig,
) -> Result<Arc<dyn carbide_secrets::credentials::CredentialReader>, DsxConsumerError> {
    if mqtt.auth.auth_mode == config::MqttAuthMode::None {
        return Ok(Arc::new(carbide_secrets::ChainedCredentialReader::from(
            Vec::new(),
        )));
    }
    carbide_secrets::create_credential_reader(&carbide_secrets::CredentialConfig::default())
        .await
        .map_err(|e| DsxConsumerError::Secrets(e.to_string()))
}

pub async fn run_service(config: Config) -> Result<(), DsxConsumerError> {
    let metrics_endpoint = config.metrics_addr().map_err(DsxConsumerError::Config)?;

    // Set up OpenTelemetry + Prometheus metrics
    let metrics_setup =
        metrics_endpoint::new_metrics_setup("carbide-dsx-exchange-consumer", "carbide", true)
            .map_err(|e| DsxConsumerError::Metrics(e.to_string()))?;

    let registry = metrics_setup.registry;
    let meter = metrics_setup.meter;
    carbide_instrument::log_events::register(&meter);

    // Spawn metrics server
    let metrics_config = metrics_endpoint::MetricsEndpointConfig {
        address: metrics_endpoint,
        registry,
        health_controller: Some(metrics_setup.health_controller),
        additional_prefix: None,
    };
    let join_listener =
        tokio::spawn(async move { metrics_endpoint::run_metrics_endpoint(&metrics_config).await });

    let credential_reader = build_credential_reader(&config.mqtt).await?;

    // Connect to MQTT and get the processing channel. mqttea tracks
    // subscriptions and replays them after reconnect when the broker reports
    // that the previous session was not resumed.
    let (queue_tx, rx) = mqtt_consumer::connect(&config.mqtt, &meter, credential_reader).await?;

    // Expose the channel's pending depth so backpressure is visible before the
    // drop counter starts moving. Hand the sender over by value: the gauge keeps
    // only a weak handle, so the channel still closes when the real senders drop.
    metrics::register_queue_pending_gauge(&meter, queue_tx);

    // Set up API client and create health updater
    let join_updater = if let Some(api_config) = config.carbide_api {
        let api_client = Arc::new(ApiClientWrapper::new(
            api_config.root_ca,
            api_config.client_cert,
            api_config.client_key,
            &api_config.api_url,
        ));
        let health_updater =
            HealthUpdater::new(config.mqtt.topic_prefix, config.cache, api_client, meter);
        tokio::spawn(async move { health_updater.run(rx).await })
    } else {
        tracing::warn!("Carbide API disabled, using console sink");
        let api_client = Arc::new(ConsoleRackHealthSink);
        let health_updater =
            HealthUpdater::new(config.mqtt.topic_prefix, config.cache, api_client, meter);
        tokio::spawn(async move { health_updater.run(rx).await })
    };

    tokio::select! {
        res = join_listener => {
            match res {
                Ok(Ok(_)) => tracing::info!("Metrics listener shutdown"),
                Ok(Err(e)) => tracing::error!(error=?e, "Metrics listener failed"),
                Err(e) => tracing::error!(error=?e, "Metrics listener join error"),
            }
        }
        res = join_updater => {
            match res {
                Ok(_) => tracing::info!("Health updater shutdown"),
                Err(e) => tracing::error!(error=?e, "Health updater join error"),
            }
        }
    };

    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_secrets::credentials::{CredentialKey, MqttCredentialType};

    use super::*;

    #[tokio::test]
    async fn auth_mode_none_builds_no_credential_source() {
        // No Vault, file, or environment access happens; the chain is empty.
        let mqtt = config::MqttConfig::default();
        assert_eq!(mqtt.auth.auth_mode, config::MqttAuthMode::None);
        let reader = build_credential_reader(&mqtt).await.expect("empty chain");
        let key = CredentialKey::MqttAuth {
            credential_type: MqttCredentialType::DsxExchangeConsumer,
        };
        assert_eq!(reader.get_credentials(&key).await.expect("read"), None);
    }
}
