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
use std::time::Duration;

use bms_dsx_exchange::{BmsDsxExchangePublisher, Publication, PublisherConfig, SourceUpdate};
use carbide_instrument::{Event, LabelValue, emit};
use carbide_mqtt_common::hook::MqttPublisher;
use carbide_mqtt_common::metrics::{MqttHookMetrics, PublishComponent};
use carbide_uuid::rack::RackId;
use chrono::Utc;
use db::db_read::PgPoolReader;
use db::{ObjectColumnFilter, rack as db_rack};
use health_report::HealthReport;
use model::rack::Rack;
use mqttea::registry::RawRegistration;
use mqttea::{MqtteaClient, QoS, RawMessageType};
use opentelemetry::metrics::Meter;
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio::time::{Instant, timeout_at};
use tokio_util::sync::CancellationToken;

const METADATA_SUBSCRIPTION: &str = "BMS/v1/PUB/Metadata/#";
const METADATA_PATTERN: &str = "^BMS/v1/PUB/Metadata/.*$";
const RACK_LEAK_ALERT_ID: &str = "BmcLeakDetection";
const RACK_LEAK_OVERRIDE_SOURCE: &str = "hardware-health.rack-leak-detection";

#[derive(Clone, Debug)]
struct BmsMetadataMessage {
    payload: Vec<u8>,
}

impl RawMessageType for BmsMetadataMessage {
    fn to_bytes(&self) -> Vec<u8> {
        self.payload.clone()
    }

    fn from_bytes(bytes: Vec<u8>) -> Self {
        Self { payload: bytes }
    }
}

enum Command {
    Metadata { topic: String, payload: Vec<u8> },
    SourceUpdate(SourceUpdate),
}

impl Command {
    fn kind(&self) -> CommandKind {
        match self {
            Self::Metadata { .. } => CommandKind::Metadata,
            Self::SourceUpdate(_) => CommandKind::SourceUpdate,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
enum CommandKind {
    Metadata,
    SourceUpdate,
}

#[derive(Event)]
#[event(
    event_name = "bms_dsx_exchange_command_dropped",
    metric_name = "carbide_bms_dsx_commands_dropped_total",
    component = "carbide-rack",
    log = warn,
    metric = counter,
    message = "BMS DSX Exchange command dropped because the worker stopped",
    describe = "Number of BMS DSX Exchange commands dropped because the worker stopped, by command kind."
)]
struct BmsDsxExchangeCommandDropped {
    #[label]
    command_kind: CommandKind,
    #[context]
    error: String,
}

pub struct BmsDsxExchangeHandle {
    sender: mpsc::Sender<Command>,
}

impl BmsDsxExchangeHandle {
    pub async fn new(
        client: Arc<MqtteaClient>,
        db_pool: &sqlx::PgPool,
        join_set: &mut JoinSet<()>,
        publish_timeout: Duration,
        queue_capacity: usize,
        meter: &Meter,
        cancel_token: CancellationToken,
    ) -> Result<Arc<Self>, eyre::Error> {
        let publisher_config = PublisherConfig::default();
        let tick_interval = publisher_config
            .heartbeat_interval
            .min(publisher_config.republish_interval);
        let (sender, receiver) = mpsc::channel(queue_capacity);
        let metrics = MqttHookMetrics::new(meter, sender.downgrade(), PublishComponent::Bms);

        let handle = Arc::new(BmsDsxExchangeHandle { sender });

        join_set.spawn(run_worker(
            receiver,
            client.clone(),
            publish_timeout,
            tick_interval,
            publisher_config,
            metrics,
            cancel_token,
        ));

        client
            .register_raw_message::<BmsMetadataMessage>(METADATA_PATTERN)
            .await
            .map_err(|error| {
                eyre::eyre!("failed to register BMS metadata message type: {error}")
            })?;

        client
            .on_message::<BmsMetadataMessage, _, _>({
                let handle = handle.clone();
                move |_client, message, topic| {
                    let handle = handle.clone();
                    async move {
                        handle.handle_metadata(topic, message.payload).await;
                    }
                }
            })
            .await;

        client
            .subscribe(METADATA_SUBSCRIPTION, QoS::AtMostOnce)
            .await
            .map_err(|error| eyre::eyre!("failed to subscribe to BMS metadata topics: {error}"))?;

        seed_current_rack_state(db_pool, handle.as_ref()).await?;

        Ok(handle)
    }

    async fn send(&self, command: Command) {
        let command_kind = command.kind();
        if let Err(error) = self.sender.send(command).await {
            emit(BmsDsxExchangeCommandDropped {
                command_kind,
                error: format!("{error:?}"),
            });
        }
    }

    pub async fn update_rack_leak_state(&self, rack_id: &RackId, report: &HealthReport) {
        let Some(leaking) = explicit_rack_leak_state(report) else {
            return;
        };
        self.set_rack_leak_state(rack_id, leaking).await;
    }

    pub async fn set_rack_leak_state(&self, rack_id: &RackId, leaking: bool) {
        self.send(Command::SourceUpdate(
            SourceUpdate::liquid_isolation_request(rack_id.to_string(), leaking),
        ))
        .await;
        self.send(Command::SourceUpdate(
            SourceUpdate::electrical_isolation_request(rack_id.to_string(), leaking),
        ))
        .await;
    }

    async fn handle_metadata(&self, topic: String, payload: Vec<u8>) {
        self.send(Command::Metadata { topic, payload }).await;
    }
}

async fn run_worker<P: MqttPublisher>(
    mut receiver: mpsc::Receiver<Command>,
    publisher: P,
    publish_timeout: Duration,
    tick_interval: Duration,
    publisher_config: PublisherConfig,
    metrics: MqttHookMetrics,
    cancel_token: CancellationToken,
) {
    let mut exchange = BmsDsxExchangePublisher::new(publisher_config);
    let mut ticker = tokio::time::interval(tick_interval);

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => break,
            _ = ticker.tick() => {
                publish_all(
                    &publisher,
                    &metrics,
                    publish_timeout,
                    exchange.tick(Utc::now()),
                ).await;
            }
            command = receiver.recv() => {
                let Some(command) = command else {
                    break;
                };

                let publications = match command {
                    Command::Metadata { topic, payload } => match bms_dsx_exchange::parse_supported_metadata(&topic, &payload) {
                        Ok(Some(metadata)) => {
                            tracing::debug!(topic = %topic, point_type = metadata.point_type(), "Received supported BMS metadata");
                            exchange.upsert_metadata(metadata, Utc::now())
                        }
                        Ok(None) => Vec::new(),
                        Err(error) => {
                            metrics.record_bms_metadata_parse_error(topic, error.to_string());
                            Vec::new()
                        }
                    },
                    Command::SourceUpdate(update) => exchange.update_source(update, Utc::now()),
                };

                publish_all(&publisher, &metrics, publish_timeout, publications).await;
            }
        }
    }

    tracing::debug!("BMS DSX Exchange worker stopped");
}

async fn publish_all<P: MqttPublisher>(
    publisher: &P,
    metrics: &MqttHookMetrics,
    publish_timeout: Duration,
    publications: Vec<Publication>,
) {
    for publication in publications {
        let payload = match publication.payload_json() {
            Ok(payload) => payload,
            Err(error) => {
                metrics.record_bms_publication_serialization_error(
                    publication.topic,
                    error.to_string(),
                );
                continue;
            }
        };

        let deadline = Instant::now() + publish_timeout;
        match timeout_at(deadline, publisher.publish(&publication.topic, payload)).await {
            Ok(Ok(())) => metrics.record_bms_success(),
            Ok(Err(error)) => {
                metrics.record_bms_publish_error(publication.topic, error.to_string());
            }
            Err(_) => {
                metrics.record_bms_timeout(publication.topic);
            }
        }
    }
}

fn explicit_rack_leak_state(report: &HealthReport) -> Option<bool> {
    (report.source == RACK_LEAK_OVERRIDE_SOURCE).then(|| {
        report
            .alerts
            .iter()
            .any(|alert| alert.id.as_str() == RACK_LEAK_ALERT_ID)
    })
}

fn rack_has_active_leak(rack: &Rack) -> Option<bool> {
    rack.health_reports
        .replace
        .iter()
        .chain(rack.health_reports.merges.get(RACK_LEAK_OVERRIDE_SOURCE))
        .find_map(explicit_rack_leak_state)
}

async fn seed_current_rack_state(
    db_pool: &sqlx::PgPool,
    handle: &BmsDsxExchangeHandle,
) -> eyre::Result<()> {
    let mut reader: PgPoolReader = db_pool.clone().into();
    let racks = db_rack::find_by(
        reader.as_mut(),
        ObjectColumnFilter::<db_rack::IdColumn>::All,
    )
    .await?;

    for rack in racks {
        if let Some(leaking) = rack_has_active_leak(&rack) {
            handle.set_rack_leak_state(&rack.id, leaking).await;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;
    use carbide_uuid::rack::RackId;
    use mqttea::MqtteaClientError;
    use opentelemetry::global;
    use tokio::sync::{Mutex, Notify};
    use tokio::task::JoinSet;
    use tokio_util::sync::CancellationToken;

    use super::*;

    const DROPPED_COMMAND_METRIC: &str = "carbide_bms_dsx_commands_dropped_total";

    #[derive(Default)]
    struct RecordingPublisher {
        published: Mutex<Vec<(String, serde_json::Value)>>,
        notify: Notify,
    }

    #[async_trait::async_trait]
    impl MqttPublisher for RecordingPublisher {
        async fn publish(&self, topic: &str, payload: Vec<u8>) -> Result<(), MqtteaClientError> {
            self.published.lock().await.push((
                topic.to_string(),
                serde_json::from_slice(&payload).expect("valid json"),
            ));
            self.notify.notify_waiters();
            Ok(())
        }
    }

    impl RecordingPublisher {
        async fn wait_for_len(&self, expected: usize) -> Vec<(String, serde_json::Value)> {
            loop {
                let notified = self.notify.notified();
                {
                    let published = self.published.lock().await;
                    if published.len() >= expected {
                        return published.clone();
                    }
                }
                notified.await;
            }
        }
    }

    fn test_meter() -> Meter {
        global::meter("bms-client-test")
    }

    fn leak_metadata_json() -> Vec<u8> {
        serde_json::json!({
            "pointType": "RackLeakDetect",
            "objectType": "Rack",
            "rackName": "Rack-01",
            "rackId": "rack-01",
            "integration": "CM"
        })
        .to_string()
        .into_bytes()
    }

    fn liquid_metadata_json() -> Vec<u8> {
        serde_json::json!({
            "pointType": "RackLiquidIsolationRequest",
            "objectType": "Rack",
            "rackName": "Rack-01",
            "rackId": "rack-01",
            "integration": "CM"
        })
        .to_string()
        .into_bytes()
    }

    fn electrical_metadata_json() -> Vec<u8> {
        serde_json::json!({
            "pointType": "RackElectricalIsolationRequest",
            "objectType": "Rack",
            "rackName": "Rack-01",
            "rackId": "rack-01",
            "integration": "CM"
        })
        .to_string()
        .into_bytes()
    }

    fn report(source: &str, leaking: bool) -> HealthReport {
        HealthReport {
            source: source.to_string(),
            triggered_by: None,
            observed_at: Some(Utc::now()),
            successes: if leaking {
                vec![]
            } else {
                vec![health_report::HealthProbeSuccess {
                    id: "BmcLeakDetection".parse().expect("valid id"),
                    target: None,
                }]
            },
            alerts: if leaking {
                vec![health_report::HealthProbeAlert {
                    id: RACK_LEAK_ALERT_ID.parse().expect("valid id"),
                    target: None,
                    in_alert_since: Some(Utc::now()),
                    message: "Leak detected".to_string(),
                    tenant_message: None,
                    classifications: vec![],
                }]
            } else {
                vec![]
            },
        }
    }

    fn spawn_test_handle(
        publisher: Arc<RecordingPublisher>,
    ) -> (Arc<BmsDsxExchangeHandle>, JoinSet<()>, CancellationToken) {
        let publisher_config = PublisherConfig::default();
        let tick_interval = publisher_config
            .heartbeat_interval
            .min(publisher_config.republish_interval);
        let mut join_set = JoinSet::new();
        let cancel_token = CancellationToken::new();
        let (sender, receiver) = mpsc::channel(32);
        let metrics =
            MqttHookMetrics::new(&test_meter(), sender.downgrade(), PublishComponent::Bms);

        join_set.spawn(run_worker(
            receiver,
            publisher,
            Duration::from_secs(1),
            tick_interval,
            publisher_config,
            metrics,
            cancel_token.clone(),
        ));

        (
            Arc::new(BmsDsxExchangeHandle { sender }),
            join_set,
            cancel_token,
        )
    }

    async fn shutdown(mut join_set: JoinSet<()>, cancel_token: CancellationToken) {
        cancel_token.cancel();
        while join_set.join_next().await.is_some() {}
    }

    #[derive(Debug)]
    enum DroppedCommandCase {
        Metadata,
        SourceUpdate,
        RackLeakState,
    }

    #[derive(Debug, PartialEq)]
    struct DroppedCommandObservation {
        log_count: usize,
        command_kinds: Vec<String>,
        metadata_count: f64,
        source_update_count: f64,
    }

    #[test]
    fn commands_dropped_after_the_worker_stops_log_and_count_by_kind() {
        value_scenarios!(
            run = |case| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("test runtime");
                let (sender, receiver) = mpsc::channel(1);
                drop(receiver);
                let handle = BmsDsxExchangeHandle { sender };
                let metrics = MetricsCapture::start();

                let logs: Vec<_> = capture_logs(|| {
                    runtime.block_on(async {
                        match case {
                            DroppedCommandCase::Metadata => {
                                handle
                                    .handle_metadata(
                                        "BMS/v1/PUB/Metadata/private-topic".to_string(),
                                        b"private payload".to_vec(),
                                    )
                                    .await;
                            }
                            DroppedCommandCase::SourceUpdate => {
                                handle
                                    .send(Command::SourceUpdate(
                                        SourceUpdate::liquid_isolation_request(
                                            "private-rack".to_string(),
                                            true,
                                        ),
                                    ))
                                    .await;
                            }
                            DroppedCommandCase::RackLeakState => {
                                handle
                                    .set_rack_leak_state(&RackId::new("private-rack"), true)
                                    .await;
                            }
                        }
                    });
                })
                .into_iter()
                .filter(|log| log.metadata_name == "bms_dsx_exchange_command_dropped")
                .collect();

                for log in &logs {
                    assert_eq!(log.level, tracing::Level::WARN);
                    assert_eq!(
                        log.metadata_name,
                        "bms_dsx_exchange_command_dropped"
                    );
                    assert_eq!(
                        log.message,
                        "BMS DSX Exchange command dropped because the worker stopped"
                    );
                    assert_eq!(
                        log.field("event_name"),
                        Some("bms_dsx_exchange_command_dropped")
                    );
                    assert_eq!(log.field("metric_name"), Some(DROPPED_COMMAND_METRIC));
                    assert_eq!(log.field("error"), Some("SendError { .. }"));
                    for private_field in ["topic", "payload", "rack_id", "source_update"] {
                        assert_eq!(log.field(private_field), None);
                    }
                }

                DroppedCommandObservation {
                    log_count: logs.len(),
                    command_kinds: logs
                        .iter()
                        .map(|log| {
                            log.field("command_kind")
                                .expect("dropped command kind")
                                .to_string()
                        })
                        .collect(),
                    metadata_count: metrics
                        .counter_delta(DROPPED_COMMAND_METRIC, &[("command_kind", "metadata")]),
                    source_update_count: metrics.counter_delta(
                        DROPPED_COMMAND_METRIC,
                        &[("command_kind", "source_update")],
                    ),
                }
            };
            "one command is dropped" {
                DroppedCommandCase::Metadata => DroppedCommandObservation {
                    log_count: 1,
                    command_kinds: vec!["metadata".to_string()],
                    metadata_count: 1.0,
                    source_update_count: 0.0,
                },
                DroppedCommandCase::SourceUpdate => DroppedCommandObservation {
                    log_count: 1,
                    command_kinds: vec!["source_update".to_string()],
                    metadata_count: 0.0,
                    source_update_count: 1.0,
                },
            }
            "`set_rack_leak_state` drops both isolation requests" {
                DroppedCommandCase::RackLeakState => DroppedCommandObservation {
                    log_count: 2,
                    command_kinds: vec![
                        "source_update".to_string(),
                        "source_update".to_string(),
                    ],
                    metadata_count: 0.0,
                    source_update_count: 2.0,
                },
            }
        );
    }

    #[test]
    fn ignores_non_rack_leak_override_source() {
        let report = report("hardware-health.bmc-sensors", true);

        assert_eq!(explicit_rack_leak_state(&report), None);
    }

    #[test]
    fn accepts_explicit_rack_leak_override_source() {
        let report = report(RACK_LEAK_OVERRIDE_SOURCE, true);

        assert_eq!(explicit_rack_leak_state(&report), Some(true));
    }

    #[test]
    fn explicit_rack_leak_override_can_clear_state() {
        let report = report(RACK_LEAK_OVERRIDE_SOURCE, false);

        assert_eq!(explicit_rack_leak_state(&report), Some(false));
    }

    #[tokio::test]
    async fn set_rack_leak_state_publishes_isolation_values_without_sleeping() {
        let publisher = Arc::new(RecordingPublisher::default());
        let (handle, join_set, cancel_token) = spawn_test_handle(publisher.clone());

        handle
            .handle_metadata(
                "BMS/v1/PUB/Metadata/Rack/RackLiquidIsolationRequest/site/rack-01".to_string(),
                liquid_metadata_json(),
            )
            .await;
        handle
            .handle_metadata(
                "BMS/v1/PUB/Metadata/Rack/RackElectricalIsolationRequest/site/rack-01".to_string(),
                electrical_metadata_json(),
            )
            .await;

        handle
            .set_rack_leak_state(&RackId::new("rack-01"), true)
            .await;

        let published = publisher.wait_for_len(2).await;
        assert!(published.iter().any(|(topic, payload)| {
            topic.contains("RackLiquidIsolationRequest") && payload["value"] == 1
        }));
        assert!(published.iter().any(|(topic, payload)| {
            topic.contains("RackElectricalIsolationRequest") && payload["value"] == 1
        }));

        shutdown(join_set, cancel_token).await;
    }

    #[tokio::test]
    async fn update_rack_leak_state_uses_explicit_override_source_without_sleeping() {
        let publisher = Arc::new(RecordingPublisher::default());
        let (handle, join_set, cancel_token) = spawn_test_handle(publisher.clone());

        handle
            .handle_metadata(
                "BMS/v1/PUB/Metadata/Rack/RackLiquidIsolationRequest/site/rack-01".to_string(),
                liquid_metadata_json(),
            )
            .await;
        handle
            .handle_metadata(
                "BMS/v1/PUB/Metadata/Rack/RackElectricalIsolationRequest/site/rack-01".to_string(),
                electrical_metadata_json(),
            )
            .await;

        handle
            .update_rack_leak_state(
                &RackId::new("rack-01"),
                &report(RACK_LEAK_OVERRIDE_SOURCE, true),
            )
            .await;

        let published = publisher.wait_for_len(2).await;
        assert!(published.iter().all(|(_, payload)| payload["value"] == 1));

        shutdown(join_set, cancel_token).await;
    }

    #[tokio::test]
    async fn rack_leak_metadata_does_not_publish_without_rack_leak_source_update() {
        let publisher = Arc::new(RecordingPublisher::default());
        let (handle, join_set, cancel_token) = spawn_test_handle(publisher.clone());

        handle
            .handle_metadata(
                "BMS/v1/PUB/Metadata/Rack/RackLeakDetect/site/rack-01".to_string(),
                leak_metadata_json(),
            )
            .await;

        handle
            .update_rack_leak_state(
                &RackId::new("rack-01"),
                &report("hardware-health.bmc-sensors", true),
            )
            .await;

        tokio::task::yield_now().await;
        assert!(publisher.published.lock().await.is_empty());

        shutdown(join_set, cancel_token).await;
    }
}
