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
use super::reconciliation::{MetricReconciler, SharedMetricSources};
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
    pub(super) reconciliation: Arc<MetricReconciler>,
}

impl ExtendedGnmiProcessor {
    pub(super) fn new(
        config: &NvueGnmiSubscriptionConfig,
        data_sink: Option<Arc<dyn DataSink>>,
        event_context: EventContext,
        switch_id: String,
        sources: Option<SharedMetricSources>,
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
            reconciliation: Arc::new(MetricReconciler::new(
                data_sink.clone(),
                event_context.clone(),
                sources,
            )),
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

        for path in &notification.delete {
            let combined = prefix.iter().chain(&path.elem).collect::<Vec<_>>();

            // A reading belongs to its last source, including when different
            // paths project to the same metric and labels.
            self.reconciliation.delete(&combined);
        }

        for update in &notification.update {
            let update_path = update
                .path
                .as_ref()
                .map(|path| path.elem.as_slice())
                .unwrap_or_default();

            let combined = prefix.iter().chain(update_path).collect::<Vec<_>>();

            self.reconciliation.touch(&combined);

            let Some(value) = update.val.as_ref() else {
                continue;
            };

            let names = combined
                .iter()
                .map(|element| element.name.clone())
                .collect::<Vec<_>>();

            let Some(mapping) = self.mappings.get(names.as_slice()) else {
                continue;
            };

            if let Some((entity, samples)) = self.metric_samples(mapping, &combined, value) {
                for sample in samples {
                    let Some(sink) = &self.data_sink else {
                        continue;
                    };

                    self.reconciliation.record(&sample, &combined);

                    sink.handle_event(
                        &self.event_context,
                        &CollectorEvent::Metric(Box::new(sample)),
                    );
                }

                entities.insert(entity);
            }
        }

        entities.len()
    }

    fn metric_samples(
        &self,
        mapping: &NvueGnmiMetricConfig,
        path: &[&PathElem],
        value: &proto::TypedValue,
    ) -> Option<(String, Vec<MetricSample>)> {
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

        let samples = match &mapping.output {
            NvueGnmiMetricOutput::Gauge { unit } => {
                let value = extended_value_to_f64(value)?;

                vec![metric_sample(
                    key,
                    &mapping.metric_type,
                    unit,
                    value,
                    labels,
                )]
            }
            NvueGnmiMetricOutput::StateSet { states } => {
                let current = categorical_value(value)?;

                if !states.iter().any(|state| state == &current) {
                    return None;
                }

                states
                    .iter()
                    .map(|state| {
                        let mut state_key = key.clone();
                        push_key_component(&mut state_key, state);

                        let mut state_labels = labels.clone();
                        state_labels.push((Cow::Borrowed("state"), state.clone()));

                        metric_sample(
                            state_key,
                            &mapping.metric_type,
                            "state",
                            if state == &current { 1.0 } else { 0.0 },
                            state_labels,
                        )
                    })
                    .collect()
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

                vec![metric_sample(
                    key,
                    &mapping.metric_type,
                    "info",
                    1.0,
                    labels,
                )]
            }
        };

        Some((entity, samples))
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
    use crate::metrics::MetricsManager;
    use crate::sink::PrometheusSink;

    #[derive(Default)]
    struct CapturingSink {
        events: Mutex<Vec<CollectorEvent>>,
        prunes: Mutex<Vec<(String, String, String)>>,
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

        fn prune_metric_key(
            &self,
            _context: &EventContext,
            key: &str,
            metric_type: &str,
            unit: &str,
        ) {
            self.prunes
                .lock()
                .expect("event capture mutex should not be poisoned")
                .push((key.to_string(), metric_type.to_string(), unit.to_string()));
        }
    }

    fn event_context() -> EventContext {
        EventContext {
            endpoint_key: "aa:bb:cc:dd:ee:ff".to_string(),
            addr: BmcAddr {
                ip: "10.0.0.1".parse().expect("test address should parse"),
                port: None,
                mac: Some(
                    MacAddress::from_str("AA:BB:CC:DD:EE:FF").expect("test MAC should parse"),
                ),
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
            None,
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
    fn reconciliation_preserves_present_and_live_touched_readings_without_replay() {
        let output = NvueGnmiMetricOutput::StateSet {
            states: vec!["up".into(), "down".into()],
        };

        let config = subscription(output.clone());
        let (mut processor, sink) = processor(output);

        let mut update = notification(
            Some(proto::TypedValue {
                value: Some(proto::typed_value::Value::StringVal("up".into())),
            }),
            true,
        );

        for id in [
            "removed",
            "present",
            "val-less",
            "empty-value",
            "touched",
            "recreated",
            "unprojectable",
            "live-val-less",
        ] {
            update.update[0].path.as_mut().unwrap().elem[0]
                .key
                .insert("name".into(), id.into());

            processor.process_notification(&update);
        }

        let request = super::super::client::build_snapshot_request(
            super::super::client::build_extended_subscribe_request(&config).unwrap(),
        );

        let mut snapshot = processor.reconciliation.begin(&request).unwrap();
        let emitted = captured_metrics(&sink).len();

        update.prefix.as_mut().unwrap().origin = "openconfig".into();
        update.update[0].val.as_mut().unwrap().value =
            Some(proto::typed_value::Value::StringVal("down".into()));

        for (id, value) in [
            ("present", update.update[0].val.clone()),
            ("val-less", None),
            ("empty-value", Some(proto::TypedValue::default())),
        ] {
            let mut present = update.clone();
            present.update[0].path.as_mut().unwrap().elem[0]
                .key
                .insert("name".into(), id.into());

            present.update[0].val = value;

            snapshot
                .process_response(&proto::SubscribeResponse {
                    response: Some(proto::subscribe_response::Response::Update(present)),
                    ..Default::default()
                })
                .unwrap();
        }

        assert_eq!(captured_metrics(&sink).len(), emitted);

        let mut unrelated = update.clone();
        unrelated.update[0]
            .path
            .as_mut()
            .unwrap()
            .elem
            .last_mut()
            .unwrap()
            .name = "unmapped".into();

        unrelated.update[0].val.as_mut().unwrap().value =
            Some(proto::typed_value::Value::JsonVal(b"{}".to_vec()));

        snapshot
            .process_response(&proto::SubscribeResponse {
                response: Some(proto::subscribe_response::Response::Update(unrelated)),
                ..Default::default()
            })
            .unwrap();

        for id in ["touched", "new", "recreated"] {
            update.update[0].path.as_mut().unwrap().elem[0]
                .key
                .insert("name".into(), id.into());

            if id == "recreated" {
                let deleted = update.update[0].path.clone().unwrap();

                processor.process_notification(&proto::Notification {
                    prefix: update.prefix.clone(),
                    delete: vec![deleted],
                    ..Default::default()
                });
            }

            processor.process_notification(&update);
        }

        update.update[0].path.as_mut().unwrap().elem[0]
            .key
            .insert("name".into(), "unprojectable".into());

        update.update[0].val.as_mut().unwrap().value =
            Some(proto::typed_value::Value::StringVal("unknown".into()));

        processor.process_notification(&update);

        update.update[0].path.as_mut().unwrap().elem[0]
            .key
            .insert("name".into(), "live-val-less".into());

        update.update[0].val = None;
        processor.process_notification(&update);

        let prunes_before = sink.prunes.lock().unwrap().len();

        assert_eq!(processor.reconciliation.finish(Some(snapshot)), 2);
        let prunes = sink.prunes.lock().unwrap();

        assert_eq!(prunes.len() - prunes_before, 2);

        assert!(
            prunes[prunes_before..]
                .iter()
                .all(|(key, _, _)| key.contains("removed"))
        );
    }

    #[test]
    #[allow(deprecated)]
    fn reconciliation_rejects_unsupported_snapshot_paths_and_values() {
        let config = subscription(NvueGnmiMetricOutput::Gauge {
            unit: "count".into(),
        });

        let request = super::super::client::build_snapshot_request(
            super::super::client::build_extended_subscribe_request(&config).unwrap(),
        );

        let valid = notification(
            Some(proto::TypedValue {
                value: Some(proto::typed_value::Value::UintVal(1)),
            }),
            true,
        );

        type Invalidate = fn(&mut proto::Notification);

        let cases: &[(&str, Invalidate)] = &[
            ("target", |n| {
                n.prefix.as_mut().unwrap().target = "other".into()
            }),
            ("origin", |n| {
                n.prefix.as_mut().unwrap().origin = "other".into()
            }),
            ("scope", |n| {
                n.prefix.as_mut().unwrap().elem[0].name = "components".into()
            }),
            ("keys", |n| {
                n.update[0].path.as_mut().unwrap().elem[0].key =
                    [("unknown".into(), "port-1".into())].into()
            }),
            ("missing key", |n| {
                n.update[0].path.as_mut().unwrap().elem[0].key.clear()
            }),
            ("container path", |n| {
                n.update[0].path.as_mut().unwrap().elem.pop();
            }),
            ("aggregate", |n| {
                n.update[0].val.as_mut().unwrap().value =
                    Some(proto::typed_value::Value::JsonVal(b"{}".to_vec()))
            }),
            ("legacy Delete", |n| {
                n.delete.push(proto::Path {
                    element: vec!["interface[name=port-1]".into()],
                    ..Default::default()
                })
            }),
        ];

        for (name, invalidate) in cases {
            let (mut processor, sink) = processor(config.metrics[0].output.clone());
            processor.process_notification(&valid);
            let mut snapshot = processor.reconciliation.begin(&request).unwrap();
            let mut notification = valid.clone();

            invalidate(&mut notification);

            assert!(
                snapshot
                    .process_response(&proto::SubscribeResponse {
                        response: Some(proto::subscribe_response::Response::Update(notification)),
                        ..Default::default()
                    })
                    .is_err(),
                "{name}"
            );

            assert_eq!(processor.reconciliation.finish(None), 0, "{name}");
            assert!(sink.prunes.lock().unwrap().is_empty(), "{name}");
        }
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
    fn extended_delete_prunes_only_the_matching_subscription_entity() {
        let (mut processor, sink) = processor(NvueGnmiMetricOutput::Gauge {
            unit: "count".to_string(),
        });

        processor.process_notification(&notification(
            Some(proto::TypedValue {
                value: Some(proto::typed_value::Value::DoubleVal(1.0)),
            }),
            true,
        ));

        let deleted = proto::Notification {
            prefix: Some(proto::Path {
                elem: vec![path_element("interfaces", &[])],
                ..Default::default()
            }),
            delete: vec![proto::Path {
                elem: vec![path_element("interface", &[("name", "port-1")])],
                ..Default::default()
            }],
            ..Default::default()
        };

        assert_eq!(processor.process_notification(&deleted), 0);

        let sample = captured_metrics(&sink).remove(0);

        let prunes = sink
            .prunes
            .lock()
            .expect("event capture mutex should not be poisoned");

        assert_eq!(
            prunes.as_slice(),
            &[(sample.key, sample.metric_type, sample.unit)]
        );
    }

    #[test]
    fn extended_leaf_delete_preserves_other_metric_with_same_type() {
        let mut config = subscription(NvueGnmiMetricOutput::Gauge {
            unit: "count".to_string(),
        });

        let mut other = config.metrics[0].clone();

        other.path = vec![
            "interface".to_string(),
            "state".to_string(),
            "other-reading".to_string(),
        ];

        other.output = NvueGnmiMetricOutput::Gauge {
            unit: "volts".to_string(),
        };

        config.metrics.push(other);

        let mut with_lane = config.metrics[0].clone();

        with_lane.path = vec![
            "interface".to_string(),
            "lane".to_string(),
            "state".to_string(),
            "reading".to_string(),
        ];

        with_lane.labels.push(NvueGnmiResponseKeyLabel {
            name: "lane_id".to_string(),
            element: "lane".to_string(),
            key: "id".to_string(),
        });

        config.metrics.push(with_lane);

        let manager = Arc::new(MetricsManager::new("test").expect("metrics manager"));
        let sink = Arc::new(PrometheusSink::new(manager.clone(), "test_sink").expect("sink"));

        let mut processor = ExtendedGnmiProcessor::new(
            &config,
            Some(sink),
            event_context(),
            "switch-1".into(),
            None,
        );

        let mut initial = notification(
            Some(proto::TypedValue {
                value: Some(proto::typed_value::Value::DoubleVal(1.0)),
            }),
            true,
        );

        let deleted_path = initial.update[0].path.clone().expect("update path");
        let mut other_update = initial.update[0].clone();

        other_update.path.as_mut().expect("update path").elem[2].name = "other-reading".into();
        initial.update.push(other_update);

        let mut lane_update = initial.update[0].clone();

        lane_update.path.as_mut().expect("update path").elem = vec![
            path_element("interface", &[("name", "port-1")]),
            path_element("lane", &[("id", "7")]),
            path_element("state", &[]),
            path_element("reading", &[]),
        ];

        initial.update.push(lane_update);

        processor.process_notification(&initial);

        let before = manager.export_telemetry().expect("telemetry");

        assert_eq!(before.matches("_interface_reading_count{").count(), 2);
        assert!(before.contains("_interface_reading_volts{"));

        processor.process_notification(&proto::Notification {
            prefix: initial.prefix,
            delete: vec![deleted_path],
            ..Default::default()
        });

        let after = manager.export_telemetry().expect("telemetry");

        assert_eq!(after.matches("_interface_reading_count{").count(), 1);
        assert!(after.contains("_interface_reading_volts{"));
        assert!(after.contains("lane_id=\"7\""));
    }

    #[test]
    fn extended_delete_keeps_reading_last_written_by_another_path() {
        let mut config = subscription(NvueGnmiMetricOutput::Gauge {
            unit: "count".to_string(),
        });

        let mut other = config.metrics[0].clone();
        other.path[2] = "other-reading".to_string();
        config.metrics.push(other);

        let manager = Arc::new(MetricsManager::new("test").expect("metrics manager"));
        let sink = Arc::new(PrometheusSink::new(manager.clone(), "test_sink").expect("sink"));

        let mut processor = ExtendedGnmiProcessor::new(
            &config,
            Some(sink),
            event_context(),
            "switch-1".into(),
            None,
        );

        let mut update = notification(
            Some(proto::TypedValue {
                value: Some(proto::typed_value::Value::DoubleVal(1.0)),
            }),
            true,
        );

        let first_path = update.update[0].path.clone().expect("update path");
        processor.process_notification(&update);

        update.update[0].path.as_mut().expect("update path").elem[2].name =
            "other-reading".to_string();

        update.update[0].val = Some(proto::TypedValue {
            value: Some(proto::typed_value::Value::DoubleVal(2.0)),
        });

        let second_path = update.update[0].path.clone().expect("update path");
        processor.process_notification(&update);

        update.update.clear();
        update.delete.push(first_path);
        processor.process_notification(&update);

        let after_old_delete = manager.export_telemetry().expect("telemetry");

        assert_eq!(
            after_old_delete
                .matches("_interface_reading_count{")
                .count(),
            1
        );

        assert!(after_old_delete.contains("} 2\n"));

        update.delete = vec![second_path];
        processor.process_notification(&update);

        let after_current_delete = manager.export_telemetry().expect("telemetry");

        assert!(!after_current_delete.contains("_interface_reading_count{"));
    }

    #[test]
    fn keyed_delete_preserves_series_written_by_another_unlabeled_key() {
        let mut config = subscription(NvueGnmiMetricOutput::Gauge {
            unit: "count".to_string(),
        });

        config.metrics[0].labels.clear();

        let manager = Arc::new(MetricsManager::new("test").expect("metrics manager"));
        let sink = Arc::new(PrometheusSink::new(manager.clone(), "test_sink").expect("sink"));

        let mut processor = ExtendedGnmiProcessor::new(
            &config,
            Some(sink),
            event_context(),
            "switch-1".into(),
            None,
        );

        let mut update = notification(
            Some(proto::TypedValue {
                value: Some(proto::typed_value::Value::DoubleVal(1.0)),
            }),
            true,
        );

        processor.process_notification(&update);

        update.update[0].path.as_mut().expect("update path").elem[0]
            .key
            .insert("name".to_string(), "port-2".to_string());

        update.update[0].val = Some(proto::TypedValue {
            value: Some(proto::typed_value::Value::DoubleVal(2.0)),
        });

        processor.process_notification(&update);

        let before = manager.export_telemetry().expect("telemetry");

        assert!(before.contains("_interface_reading_count{") && before.contains("} 2\n"));

        let delete = |name| proto::Notification {
            delete: vec![proto::Path {
                elem: vec![path_element("interface", &[("name", name)])],
                ..Default::default()
            }],
            ..notification(None, true)
        };

        processor.process_notification(&delete("port-1"));

        let after_old_delete = manager.export_telemetry().expect("telemetry");

        assert!(after_old_delete.contains("_interface_reading_count{"));
        assert!(after_old_delete.contains("} 2\n"));

        processor.process_notification(&delete("port-2"));

        let after_current_delete = manager.export_telemetry().expect("telemetry");

        assert!(!after_current_delete.contains("_interface_reading_count{"));

        processor.process_notification(&update);

        processor.process_notification(&proto::Notification {
            delete: vec![proto::Path::default()],
            ..notification(None, true)
        });

        let after_ancestor_delete = manager.export_telemetry().expect("telemetry");

        assert!(!after_ancestor_delete.contains("_interface_reading_count{"));
    }

    #[test]
    fn extended_state_set_delete_preserves_states_owned_by_other_path() {
        let mut config = subscription(NvueGnmiMetricOutput::StateSet {
            states: vec!["up".to_string(), "down".to_string()],
        });

        let mut other = config.metrics[0].clone();
        other.path[2] = "other-reading".to_string();

        other.output = NvueGnmiMetricOutput::StateSet {
            states: vec!["up".to_string(), "ready".to_string()],
        };

        config.metrics.push(other);

        let manager = Arc::new(MetricsManager::new("test").expect("metrics manager"));
        let sink = Arc::new(PrometheusSink::new(manager.clone(), "test_sink").expect("sink"));

        let mut processor = ExtendedGnmiProcessor::new(
            &config,
            Some(sink),
            event_context(),
            "switch-1".into(),
            None,
        );

        let mut update = notification(
            Some(proto::TypedValue {
                value: Some(proto::typed_value::Value::StringVal("up".to_string())),
            }),
            true,
        );

        let deleted_path = update.update[0].path.clone().expect("update path");
        processor.process_notification(&update);

        update.update[0].path.as_mut().expect("update path").elem[2].name =
            "other-reading".to_string();

        update.update[0].val = Some(proto::TypedValue {
            value: Some(proto::typed_value::Value::StringVal("ready".to_string())),
        });

        processor.process_notification(&update);

        let before = manager.export_telemetry().expect("telemetry");

        assert!(before.contains("state=\"up\""));
        assert!(before.contains("state=\"ready\""));

        processor.process_notification(&proto::Notification {
            prefix: update.prefix,
            delete: vec![deleted_path],
            ..Default::default()
        });

        let after = manager.export_telemetry().expect("telemetry");

        assert!(
            after
                .lines()
                .any(|line| line.contains("state=\"up\"") && line.ends_with(" 0"))
        );

        assert!(!after.contains("state=\"down\""));
        assert!(after.contains("state=\"ready\""));
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
