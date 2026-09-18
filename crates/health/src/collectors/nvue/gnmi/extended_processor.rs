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

//! Metric projection for extended gNMI subscriptions.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use super::client::{typed_value_to_f64, typed_value_to_string};
use super::proto::{self, PathElem};
use super::sample_processor::now_unix_secs;
use super::subscriber::GnmiStreamMetrics;
use crate::config::{NvueGnmiMetricConfig, NvueGnmiMetricOutput, NvueGnmiSubscriptionConfig};
use crate::metrics::MetricLabel;
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample};

pub(super) const EXTENDED_GNMI_STREAM_ID: &str = "nvue_gnmi_extended";

/// Projects one named subscription without interpreting its data-tree schema.
pub(super) struct ExtendedGnmiProcessor {
    data_sink: Option<Arc<dyn DataSink>>,
    event_context: EventContext,
    pub(super) subscription_name: String,
    pub(super) switch_id: String,
    mappings: HashMap<Vec<String>, NvueGnmiMetricConfig>,
}

impl ExtendedGnmiProcessor {
    pub(super) fn new(
        config: &NvueGnmiSubscriptionConfig,
        data_sink: Option<Arc<dyn DataSink>>,
        event_context: EventContext,
        switch_id: String,
    ) -> Self {
        let mappings = config
            .metrics
            .iter()
            .map(|metric| {
                let combined_path = config.prefix.iter().chain(&metric.path).cloned().collect();

                (combined_path, metric.clone())
            })
            .collect();

        Self {
            data_sink,
            event_context,
            subscription_name: config.name.clone(),
            switch_id,
            mappings,
        }
    }

    /// Processes one response and reports whether it established a usable stream.
    ///
    /// An in-band gNMI error is returned as a status so the subscriber can
    /// refresh credentials when required and reconnect.
    pub(super) fn process_subscribe_response(
        &mut self,
        response: &proto::SubscribeResponse,
        stream_metrics: &GnmiStreamMetrics,
    ) -> Result<bool, tonic::Status> {
        let Some(response) = response.response.as_ref() else {
            return Ok(false);
        };

        let notification = match response {
            proto::subscribe_response::Response::Update(notification) => notification,
            proto::subscribe_response::Response::SyncResponse(_) => return Ok(true),
            #[allow(deprecated, reason = "accept the legacy in-band gNMI error response")]
            proto::subscribe_response::Response::Error(error) => {
                stream_metrics.stream_errors_total.inc();

                tracing::warn!(
                    grpc_status_code = error.code,
                    error = %error.message,
                    switch_id = %self.switch_id,
                    subscription = %self.subscription_name,
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "extended gNMI stream reported an error"
                );

                let Ok(code) = i32::try_from(error.code) else {
                    return Err(tonic::Status::unknown(error.message.clone()));
                };

                return Err(tonic::Status::new(
                    tonic::Code::from_i32(code),
                    error.message.clone(),
                ));
            }
        };

        stream_metrics.notifications_received_total.inc();
        stream_metrics
            .last_notification_timestamp
            .set(now_unix_secs());

        let start = Instant::now();
        let emitted = self.process_notification(notification);

        stream_metrics
            .notification_processing_seconds
            .observe(start.elapsed().as_secs_f64());

        stream_metrics.monitored_entities.set(emitted as f64);

        Ok(true)
    }

    fn process_notification(&mut self, notification: &proto::Notification) -> usize {
        let prefix = notification
            .prefix
            .as_ref()
            .map(|path| path.elem.as_slice())
            .unwrap_or_default();

        let mut entities = HashSet::new();

        for update in &notification.update {
            let Some(value) = update.val.as_ref() else {
                continue;
            };

            let update_path = update
                .path
                .as_ref()
                .map(|path| path.elem.as_slice())
                .unwrap_or_default();

            let combined = prefix.iter().chain(update_path).collect::<Vec<_>>();

            let names = combined
                .iter()
                .map(|element| element.name.clone())
                .collect::<Vec<_>>();

            let Some(mapping) = self.mappings.get(names.as_slice()) else {
                continue;
            };

            if let Some(entity) = self.emit_metric(mapping, &combined, value) {
                entities.insert(entity);
            }
        }

        entities.len()
    }

    fn emit_metric(
        &self,
        mapping: &NvueGnmiMetricConfig,
        path: &[&PathElem],
        value: &proto::TypedValue,
    ) -> Option<String> {
        let mut labels = response_key_labels(mapping, path)?;

        let mut entity = String::new();
        push_key_component(&mut entity, &self.subscription_name);

        for (name, value) in &labels {
            push_key_component(&mut entity, name.as_ref());
            push_key_component(&mut entity, value);
        }

        let mut key = String::new();
        push_key_component(&mut key, &self.subscription_name);
        push_key_component(&mut key, &mapping.metric_type);

        for (name, value) in &labels {
            push_key_component(&mut key, name.as_ref());
            push_key_component(&mut key, value);
        }

        labels.insert(
            0,
            (
                Cow::Borrowed("subscription"),
                self.subscription_name.clone(),
            ),
        );

        match &mapping.output {
            NvueGnmiMetricOutput::Gauge { unit } => {
                let value = extended_value_to_f64(value)?;
                let sample = metric_sample(key, &mapping.metric_type, unit, value, labels);

                self.emit_sample(sample);
            }
            NvueGnmiMetricOutput::StateSet { states } => {
                let current = categorical_value(value)?;

                if !states.iter().any(|state| state == &current) {
                    return None;
                }

                for state in states {
                    let mut state_key = key.clone();
                    push_key_component(&mut state_key, state);

                    let mut state_labels = labels.clone();
                    state_labels.push((Cow::Borrowed("state"), state.clone()));

                    let sample = metric_sample(
                        state_key,
                        &mapping.metric_type,
                        "state",
                        if state == &current { 1.0 } else { 0.0 },
                        state_labels,
                    );

                    self.emit_sample(sample);
                }
            }
            NvueGnmiMetricOutput::Info {
                label,
                values: allowed_values,
            } => {
                let value = categorical_value(value)?;

                if !allowed_values.iter().any(|allowed| allowed == &value) {
                    return None;
                }

                labels.push((Cow::Owned(label.clone()), value));

                let sample = metric_sample(key, &mapping.metric_type, "info", 1.0, labels);
                self.emit_sample(sample);
            }
        }

        Some(entity)
    }

    fn emit_sample(&self, sample: MetricSample) {
        if let Some(sink) = &self.data_sink {
            sink.handle_event(
                &self.event_context,
                &CollectorEvent::Metric(Box::new(sample)),
            );
        }
    }
}

fn categorical_value(value: &proto::TypedValue) -> Option<String> {
    use proto::typed_value::Value;

    match &value.value {
        Some(Value::JsonVal(bytes)) | Some(Value::JsonIetfVal(bytes)) => {
            match serde_json::from_slice(bytes).ok()? {
                serde_json::Value::String(value) => Some(value),
                serde_json::Value::Number(value) => Some(value.to_string()),
                serde_json::Value::Bool(value) => Some(value.to_string()),
                _ => None,
            }
        }
        _ => typed_value_to_string(value),
    }
}

/// Adds ASCII numeric support and rejects non-finite results.
fn extended_value_to_f64(value: &proto::TypedValue) -> Option<f64> {
    use proto::typed_value::Value;

    let value = match &value.value {
        Some(Value::AsciiVal(value)) => value.parse().ok(),
        _ => typed_value_to_f64(value),
    }?;

    value.is_finite().then_some(value)
}

fn response_key_labels(
    mapping: &NvueGnmiMetricConfig,
    path: &[&PathElem],
) -> Option<Vec<MetricLabel>> {
    mapping
        .labels
        .iter()
        .map(|label| {
            let element = path.iter().find(|element| element.name == label.element)?;

            let value = element
                .key
                .get(&label.key)
                .filter(|value| !value.is_empty())?;

            Some((Cow::Owned(label.name.clone()), value.clone()))
        })
        .collect()
}

fn metric_sample(
    key: String,
    metric_type: &str,
    unit: &str,
    value: f64,
    labels: Vec<MetricLabel>,
) -> MetricSample {
    MetricSample {
        key,
        name: EXTENDED_GNMI_STREAM_ID.to_string(),
        metric_type: metric_type.to_string(),
        unit: unit.to_string(),
        value,
        labels,
        context: None,
    }
}

/// Uses length prefixes so extended names and response keys cannot alias.
fn push_key_component(key: &mut String, component: &str) {
    key.push_str(&component.len().to_string());
    key.push(':');
    key.push_str(component);
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;
    use std::sync::Mutex;

    use mac_address::MacAddress;

    use super::*;
    use crate::config::NvueGnmiResponseKeyLabel;
    use crate::endpoint::BmcAddr;

    #[derive(Default)]
    struct CapturingSink {
        events: Mutex<Vec<CollectorEvent>>,
    }

    impl DataSink for CapturingSink {
        fn sink_type(&self) -> &'static str {
            "capturing_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), crate::HealthError> {
            self.events
                .lock()
                .expect("event capture mutex should not be poisoned")
                .push(event.clone());

            Ok(())
        }
    }

    fn event_context() -> EventContext {
        EventContext {
            endpoint_key: "aa:bb:cc:dd:ee:ff".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse().expect("test address should parse"),
                port: None,
                mac: MacAddress::from_str("AA:BB:CC:DD:EE:FF").expect("test MAC should parse"),
            },
            collector_type: EXTENDED_GNMI_STREAM_ID,
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    fn subscription(output: NvueGnmiMetricOutput) -> NvueGnmiSubscriptionConfig {
        NvueGnmiSubscriptionConfig {
            name: "external_metrics".to_string(),
            prefix: vec!["interfaces".to_string()],
            paths: vec![vec!["interface".to_string()]],
            metrics: vec![NvueGnmiMetricConfig {
                path: vec![
                    "interface".to_string(),
                    "state".to_string(),
                    "reading".to_string(),
                ],
                metric_type: "interface_reading".to_string(),
                labels: vec![NvueGnmiResponseKeyLabel {
                    name: "interface_name".to_string(),
                    element: "interface".to_string(),
                    key: "name".to_string(),
                }],
                output,
            }],
            ..Default::default()
        }
    }

    fn processor(output: NvueGnmiMetricOutput) -> (ExtendedGnmiProcessor, Arc<CapturingSink>) {
        let sink = Arc::new(CapturingSink::default());

        let processor = ExtendedGnmiProcessor::new(
            &subscription(output),
            Some(sink.clone()),
            event_context(),
            "switch-1".to_string(),
        );

        (processor, sink)
    }

    fn path_element(name: &str, keys: &[(&str, &str)]) -> PathElem {
        PathElem {
            name: name.to_string(),
            key: keys
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
        }
    }

    fn notification(value: Option<proto::TypedValue>, keyed: bool) -> proto::Notification {
        proto::Notification {
            prefix: Some(proto::Path {
                elem: vec![path_element("interfaces", &[])],
                ..Default::default()
            }),
            update: vec![proto::Update {
                path: Some(proto::Path {
                    elem: vec![
                        path_element("interface", if keyed { &[("name", "port-1")] } else { &[] }),
                        path_element("state", &[]),
                        path_element("reading", &[]),
                    ],
                    ..Default::default()
                }),
                val: value,
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    fn captured_metrics(sink: &CapturingSink) -> Vec<MetricSample> {
        sink.events
            .lock()
            .expect("event capture mutex should not be poisoned")
            .iter()
            .map(|event| {
                let CollectorEvent::Metric(sample) = event else {
                    panic!("extended processor should emit only metrics");
                };

                sample.as_ref().clone()
            })
            .collect()
    }

    fn stream_metrics() -> GnmiStreamMetrics {
        super::super::subscriber::test_gnmi_stream_metrics()
    }

    #[test]
    fn extended_gauge_maps_response_path_key() {
        let (mut processor, sink) = processor(NvueGnmiMetricOutput::Gauge {
            unit: "count".to_string(),
        });

        let value = proto::TypedValue {
            value: Some(proto::typed_value::Value::DoubleVal(42.5)),
        };

        let mut sample_notification = notification(Some(value), true);
        sample_notification
            .update
            .push(sample_notification.update[0].clone());

        assert_eq!(processor.process_notification(&sample_notification), 1);

        let samples = captured_metrics(&sink);

        assert_eq!(samples.len(), 2);
        assert_eq!(samples[0].name, EXTENDED_GNMI_STREAM_ID);
        assert_eq!(samples[0].metric_type, "interface_reading");
        assert_eq!(samples[0].unit, "count");
        assert_eq!(samples[0].value, 42.5);

        assert_eq!(
            samples[0].labels,
            [
                (
                    Cow::Borrowed("subscription"),
                    "external_metrics".to_string()
                ),
                (Cow::Borrowed("interface_name"), "port-1".to_string()),
            ]
        );

        let ascii = proto::TypedValue {
            value: Some(proto::typed_value::Value::AsciiVal("1.25".to_string())),
        };

        assert_eq!(
            processor.process_notification(&notification(Some(ascii), true)),
            1
        );

        assert_eq!(captured_metrics(&sink).last().unwrap().value, 1.25);
    }

    #[test]
    fn extended_categorical_outputs_use_finite_domains() {
        let (mut state_processor, state_sink) = processor(NvueGnmiMetricOutput::StateSet {
            states: vec!["normal".to_string(), "attention".to_string()],
        });

        let attention = proto::TypedValue {
            value: Some(proto::typed_value::Value::StringVal(
                "attention".to_string(),
            )),
        };

        assert_eq!(
            state_processor.process_notification(&notification(Some(attention), true)),
            1
        );

        let state_samples = captured_metrics(&state_sink);

        assert_eq!(state_samples.len(), 2);
        assert_eq!(state_samples[0].value, 0.0);
        assert_eq!(state_samples[1].value, 1.0);

        let (mut info_processor, info_sink) = processor(NvueGnmiMetricOutput::Info {
            label: "mode".to_string(),
            values: vec!["active".to_string(), "standby".to_string()],
        });

        let active = proto::TypedValue {
            value: Some(proto::typed_value::Value::JsonIetfVal(
                br#""active""#.to_vec(),
            )),
        };

        assert_eq!(
            info_processor.process_notification(&notification(Some(active), true)),
            1
        );

        let info_samples = captured_metrics(&info_sink);

        assert_eq!(info_samples.len(), 1);
        assert_eq!(info_samples[0].unit, "info");

        assert!(
            info_samples[0]
                .labels
                .contains(&(Cow::Owned("mode".to_string()), "active".to_string()))
        );
    }

    #[test]
    fn extended_metrics_ignore_unusable_updates() {
        let (mut processor, sink) = processor(NvueGnmiMetricOutput::StateSet {
            states: vec!["normal".to_string(), "attention".to_string()],
        });

        let unknown = proto::TypedValue {
            value: Some(proto::typed_value::Value::StringVal("other".to_string())),
        };

        let malformed_json = proto::TypedValue {
            value: Some(proto::typed_value::Value::JsonVal(b"attention".to_vec())),
        };

        assert_eq!(
            processor.process_notification(&notification(Some(unknown), true)),
            0
        );

        assert_eq!(
            processor.process_notification(&notification(Some(malformed_json), true)),
            0
        );

        assert_eq!(processor.process_notification(&notification(None, true)), 0);

        assert_eq!(
            processor.process_notification(&notification(
                Some(proto::TypedValue {
                    value: Some(proto::typed_value::Value::StringVal("normal".to_string(),)),
                }),
                false,
            )),
            0
        );

        assert!(captured_metrics(&sink).is_empty());
    }

    #[test]
    #[allow(deprecated, reason = "exercise legacy in-band gNMI error handling")]
    fn extended_stream_response_types_are_isolated() {
        let (mut processor, sink) = processor(NvueGnmiMetricOutput::Gauge {
            unit: "count".to_string(),
        });

        let metrics = stream_metrics();

        assert!(
            processor
                .process_subscribe_response(
                    &proto::SubscribeResponse {
                        response: Some(proto::subscribe_response::Response::SyncResponse(true)),
                        ..Default::default()
                    },
                    &metrics,
                )
                .is_ok_and(|usable| usable)
        );

        let error = processor
            .process_subscribe_response(
                &proto::SubscribeResponse {
                    response: Some(proto::subscribe_response::Response::Error(proto::Error {
                        code: tonic::Code::Unauthenticated as u32,
                        message: "test error".to_string(),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                &metrics,
            )
            .expect_err("in-band gNMI error should request reconnection");

        assert_eq!(error.code(), tonic::Code::Unauthenticated);

        assert_eq!(metrics.notifications_received_total.get(), 0.0);
        assert_eq!(metrics.stream_errors_total.get(), 1.0);
        assert!(captured_metrics(&sink).is_empty());
    }
}
