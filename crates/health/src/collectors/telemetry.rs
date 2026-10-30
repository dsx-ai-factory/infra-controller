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

//! Collect readings from the Redfish telemetry service.
//!
//! A BMC that implements `TelemetryService` publishes pre-aggregated
//! metric reports: one GET returns every reading the platform batched
//! into that report, instead of the one-GET-per-resource walk the
//! sensor and entity-metric collectors do. Where a platform offers it,
//! this is the cheap path to the same numbers.
//!
//! Reports carry no units -- `MetricReport` only pairs a `MetricId`
//! with a stringly-typed `MetricValue`. The unit lives on the matching
//! `MetricDefinition`, so definitions are read once and cached for the
//! lifetime of the collector, indexed by the definition's `Id` and by each
//! URI in its `MetricProperties`, with `{Name}` wildcards expanded from
//! the definition's `Wildcards`. A value that carries a `MetricId` takes
//! its unit from that definition only; a value without one is matched
//! through its `MetricProperty`.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::Arc;

use nv_redfish::ServiceRoot;
use nv_redfish::core::Bmc;
use nv_redfish::telemetry_service::MetricReport;

use crate::HealthError;
use crate::collectors::runtime::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;
use crate::metrics::{MetricLabel, sanitize_unit};
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample};

/// Series name every telemetry-service reading is published under.
///
/// Deliberately distinct from the sensor collector's `hw_sensor` and
/// the entity-metric collector's `hw_metric`: the same reading can be
/// reachable through more than one of them, and keeping the names
/// apart makes the source obvious rather than silently double-counting
/// into one series.
const METRIC_NAME: &str = "hw_telemetry";

/// Unit recorded when no `MetricDefinition` declares one.
const UNKNOWN_UNIT: &str = "unknown";

/// Turn a Redfish `MetricId` into the series-name fragment.
///
/// Platforms spell ids both ways -- `TotalGPUPowerWatts` and
/// `HGX_Chassis_0_HSC_0_Power_0` -- so word boundaries are taken from
/// case transitions as well as separators, and anything that is not
/// alphanumeric collapses to a single underscore.
fn metric_type_from_id(metric_id: &str) -> String {
    let chars: Vec<char> = metric_id.chars().collect();
    let mut out = String::with_capacity(metric_id.len() + 4);

    for (index, &current) in chars.iter().enumerate() {
        if !current.is_ascii_alphanumeric() {
            push_separator(&mut out);
            continue;
        }

        if current.is_ascii_uppercase() && index > 0 {
            let previous = chars[index - 1];
            // `...aX` starts a new word, and so does the `X` that ends
            // an acronym run before a lowercase letter (`GPUPower` ->
            // `gpu_power`).
            let leaves_word = previous.is_ascii_lowercase() || previous.is_ascii_digit();
            let ends_acronym = previous.is_ascii_uppercase()
                && chars.get(index + 1).is_some_and(char::is_ascii_lowercase);
            if leaves_word || ends_acronym {
                push_separator(&mut out);
            }
        }

        out.push(current.to_ascii_lowercase());
    }

    out.trim_matches('_').to_string()
}

/// Append `_` unless the output is empty or already ends in one.
fn push_separator(out: &mut String) {
    if !out.is_empty() && !out.ends_with('_') {
        out.push('_');
    }
}

/// Extract the property identifier leaf from a Redfish `MetricProperty` URI.
///
/// When a URI fragment exists (e.g. `#/0/PowerConsumedWatts` or
/// `#/Sensors/GPU_0_Temp_0`), the target property is named in the fragment.
/// When no fragment exists, the last path segment of the resource path is used.
fn leaf_from_property(property: &str) -> &str {
    match property.split_once('#') {
        Some((path, fragment)) => {
            let frag = fragment.trim_end_matches('/');
            if frag.is_empty() {
                path.trim_end_matches('/')
            } else {
                frag
            }
        }
        None => property.trim_end_matches('/'),
    }
    .rsplit('/')
    .next()
    .filter(|s| !s.is_empty())
    .unwrap_or("unknown")
}

/// Expand the `{Name}` wildcards in a `MetricDefinition` property URI.
///
/// Each wildcard is replaced by every one of its listed values, so a URI
/// with several wildcards expands to their cross product. A `*` value
/// stands for "all possible values" and cannot be enumerated, so it stays
/// in the URI as a literal `*` that [`glob_matches`] treats as a pattern. A
/// URI that still contains a placeholder afterwards matches no concrete
/// `MetricProperty` and is dropped.
fn expand_wildcards(template: &str, wildcards: &[(String, Vec<String>)]) -> Vec<String> {
    let mut expanded = vec![template.to_string()];
    for (name, values) in wildcards {
        let placeholder = format!("{{{name}}}");
        if !template.contains(&placeholder) {
            continue;
        }
        expanded = expanded
            .iter()
            .flat_map(|uri| values.iter().map(|value| uri.replace(&placeholder, value)))
            .collect();
    }
    expanded.retain(|uri| !uri.contains('{'));
    expanded
}

/// Whether `text` matches `pattern`, where each `*` stands for one or more
/// characters that do not cross a `/`, i.e. at most one URI path segment
/// (or the part of it a wildcard fills).
fn glob_matches(pattern: &str, text: &str) -> bool {
    let Some((head, rest)) = pattern.split_once('*') else {
        return pattern == text;
    };
    let Some(after_head) = text.strip_prefix(head) else {
        return false;
    };
    let longest = after_head.find('/').unwrap_or(after_head.len());
    (1..=longest)
        .filter(|&len| after_head.is_char_boundary(len))
        .any(|len| glob_matches(rest, &after_head[len..]))
}

/// Units declared by the service's metric definitions.
#[derive(Debug, Default)]
struct UnitIndex {
    /// Definition `Id` (what a value's `MetricId` names) -> sanitized unit.
    by_id: HashMap<String, String>,
    /// Concrete `MetricProperties` URI -> sanitized unit.
    by_property: HashMap<String, String>,
    /// `MetricProperties` URIs with a `*` ("all values") wildcard, tried in
    /// definition order when no concrete URI matches.
    property_patterns: Vec<(String, String)>,
}

impl UnitIndex {
    /// The unit for a value. A `MetricId` names the definition that
    /// describes the reading -- which may be a calculation over the
    /// property in another unit -- so the property is consulted only
    /// when the value carries no `MetricId`.
    fn unit(&self, metric_id: Option<&str>, property: Option<&str>) -> Option<&str> {
        match metric_id {
            Some(metric_id) => self.by_id.get(metric_id),
            None => property.and_then(|property| {
                self.by_property.get(property).or_else(|| {
                    self.property_patterns
                        .iter()
                        .find(|(pattern, _)| glob_matches(pattern, property))
                        .map(|(_, unit)| unit)
                })
            }),
        }
        .map(String::as_str)
    }
}

pub struct TelemetryCollectorConfig {
    pub data_sink: Option<Arc<dyn DataSink>>,
}

pub struct TelemetryCollector<B: Bmc> {
    bmc: Arc<B>,
    endpoint: Arc<BmcEndpoint>,
    event_context: EventContext,
    data_sink: Option<Arc<dyn DataSink>>,
    /// Units read once from the service's metric definitions.
    units: Option<UnitIndex>,
}

impl<B: Bmc + 'static> PeriodicCollector<B> for TelemetryCollector<B> {
    type Config = TelemetryCollectorConfig;

    fn new_runner(
        bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let event_context = EventContext::from_endpoint(endpoint.as_ref(), "telemetry_collector");
        Ok(Self {
            bmc,
            endpoint,
            event_context,
            data_sink: config.data_sink,
            units: None,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        let service_root = ServiceRoot::new(self.bmc.clone()).await?;
        let Some(telemetry_service) = service_root.telemetry_service().await? else {
            tracing::debug!(
                bmc_address = ?self.endpoint.addr,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "BMC exposes no telemetry service; skipping iteration"
            );
            return Ok(IterationResult {
                refresh_triggered: true,
                entity_count: Some(0),
                fetch_failures: 0,
            });
        };

        let mut fetch_failures = 0;
        if self.units.is_none() {
            self.units = self.load_units(&telemetry_service).await;
            if self.units.is_none() {
                fetch_failures += 1;
            }
        }

        let Some(report_links) = telemetry_service.metric_report_links().await? else {
            return Ok(IterationResult {
                refresh_triggered: true,
                entity_count: Some(0),
                fetch_failures,
            });
        };

        self.emit_event(CollectorEvent::MetricCollectionStart);

        let mut sample_count = 0;
        for link in report_links {
            match link.upgrade::<MetricReport<B>>().await {
                Ok(report) => sample_count += self.publish_report(&report),
                Err(error) => {
                    fetch_failures += 1;
                    tracing::warn!(
                        ?error,
                        report = %link.odata_id(),
                        bmc_address = ?self.endpoint.addr,
                        rack_id = self.event_context.rack_id().map(tracing::field::display),
                        "Failed to fetch metric report"
                    );
                }
            }
        }

        self.emit_event(CollectorEvent::MetricCollectionEnd);

        Ok(IterationResult {
            refresh_triggered: true,
            entity_count: Some(sample_count),
            fetch_failures,
        })
    }

    fn collector_type(&self) -> &'static str {
        "telemetry_collector"
    }

    async fn stop(&mut self) {
        self.emit_event(CollectorEvent::CollectorRemoved);
    }
}

impl<B: Bmc + 'static> TelemetryCollector<B> {
    fn emit_event(&self, event: CollectorEvent) {
        if let Some(data_sink) = &self.data_sink {
            data_sink.handle_event(&self.event_context, &event);
        }
    }

    /// Build the unit index from the service's metric definitions, keyed
    /// by each definition's `Id` and by every URI in its
    /// `MetricProperties` after wildcard expansion.
    ///
    /// A BMC that publishes no definitions is not an error; every
    /// reading then falls back to [`UNKNOWN_UNIT`]. Returning an empty
    /// map (rather than `None`) keeps that from being retried on every
    /// iteration. A failed fetch is different: the definitions may well
    /// exist, so `None` leaves the map unloaded and the next iteration
    /// tries again rather than pinning every reading to [`UNKNOWN_UNIT`]
    /// until restart.
    async fn load_units(
        &self,
        telemetry_service: &nv_redfish::telemetry_service::TelemetryService<B>,
    ) -> Option<UnitIndex> {
        let definitions = match telemetry_service.metric_definitions().await {
            Ok(Some(definitions)) => definitions,
            Ok(None) => Vec::new(),
            Err(error) => {
                tracing::warn!(
                    ?error,
                    bmc_address = ?self.endpoint.addr,
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "Failed to fetch metric definitions; readings are recorded \
                     without units until the next iteration succeeds"
                );
                return None;
            }
        };

        let mut index = UnitIndex::default();
        for definition in definitions {
            let raw = definition.raw();
            let Some(unit) = raw.units.clone().flatten() else {
                continue;
            };
            let unit = sanitize_unit(&unit);
            let wildcards: Vec<(String, Vec<String>)> = raw
                .wildcards
                .iter()
                .flatten()
                .filter_map(|wildcard| {
                    Some((
                        wildcard.name.clone().flatten()?,
                        wildcard.values.clone().flatten().unwrap_or_default(),
                    ))
                })
                .collect();
            for template in raw.metric_properties.iter().flatten().flatten() {
                for property in expand_wildcards(template, &wildcards) {
                    if property.contains('*') {
                        index.property_patterns.push((property, unit.clone()));
                    } else {
                        index.by_property.insert(property, unit.clone());
                    }
                }
            }
            index.by_id.insert(raw.id.clone(), unit);
        }
        Some(index)
    }

    /// Emit one sample per numeric reading in `report`, returning how
    /// many were published.
    fn publish_report(&self, report: &MetricReport<B>) -> usize {
        let raw = report.raw();
        let report_id = &raw.id;

        // A stale report is republishing the previous interval's
        // numbers. Emitting them would flatten real gaps into a held
        // value, so the whole report is dropped instead.
        if self.report_is_stale(report) {
            tracing::debug!(
                report_id,
                bmc_address = ?self.endpoint.addr,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "Skipping metric report marked stale by the NVIDIA OEM extension"
            );
            return 0;
        }

        let units = self.units.as_ref();
        let mut count = 0;
        for value in raw.metric_values.iter().flatten() {
            let metric_id = value.metric_id.clone().flatten();
            let property = value.metric_property.clone().flatten();
            let is_stale = value
                .oem
                .as_ref()
                .and_then(|oem| {
                    oem.additional_properties
                        .pointer("/Nvidia/MetricValueStale")
                        .and_then(serde_json::Value::as_bool)
                })
                .unwrap_or(false);
            if is_stale {
                tracing::debug!(
                    report_id,
                    metric_id = metric_id.as_deref().unwrap_or(""),
                    property = property.as_deref().unwrap_or(""),
                    bmc_address = ?self.endpoint.addr,
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "Skipping metric value marked stale by the NVIDIA OEM extension"
                );
                continue;
            }

            let Some(reading) = value.metric_value.as_ref().and_then(Option::as_ref) else {
                continue;
            };
            // `MetricValue` is a string in the schema and platforms use
            // it for discrete states ("Enabled", "OK") as well as
            // numbers. Only the numbers can become a gauge.
            let Ok(reading) = reading.trim().parse::<f64>() else {
                continue;
            };
            // `f64::from_str` also accepts "NaN" and "inf". Those are legal
            // Prometheus tokens, but as hardware readings they carry no
            // information and poison every aggregate they enter.
            if !reading.is_finite() {
                tracing::debug!(
                    report_id,
                    metric_id = metric_id.as_deref().unwrap_or(""),
                    property = property.as_deref().unwrap_or(""),
                    reading,
                    bmc_address = ?self.endpoint.addr,
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "Skipping non-finite metric reading"
                );
                continue;
            }

            let unit = units
                .and_then(|units| units.unit(metric_id.as_deref(), property.as_deref()))
                .unwrap_or(UNKNOWN_UNIT);

            let mut labels: Vec<MetricLabel> =
                vec![(Cow::Borrowed("report_id"), report_id.clone())];
            // `MetricId` names a *definition*, so one report repeats it
            // across every property that definition applies to -- a
            // per-chassis temperature is one id and many readings. The
            // property URI is what makes the reading unique, so it is
            // what the key is built from: sinks index gauges by key, and
            // keying on the id alone would drop every reading but the
            // last. This also matches the entity collectors, whose keys
            // are `{resource odata id}/{metric}`. A value without a
            // `MetricId` is keyed by its `MetricProperty` alone and its
            // `metric_type` comes from the URI's leaf. For JSON-pointer
            // style properties such as `.../Sensors/X#/Reading` that leaf
            // is `Reading`; series stay distinct through the key and the
            // `metric_property` label.
            let (key, metric_type) = match (property.as_deref(), metric_id.as_deref()) {
                (Some(property), Some(metric_id)) => (
                    format!("{property}/{metric_id}"),
                    metric_type_from_id(metric_id),
                ),
                (Some(property), None) => (
                    property.to_string(),
                    metric_type_from_id(leaf_from_property(property)),
                ),
                (None, Some(metric_id)) => (
                    format!("{}/{metric_id}", report.raw().odata_id),
                    metric_type_from_id(metric_id),
                ),
                (None, None) => continue,
            };
            if let Some(ref property) = property {
                labels.push((Cow::Borrowed("metric_property"), property.clone()));
            }

            self.emit_event(CollectorEvent::Metric(
                MetricSample {
                    semantics: crate::sink::MetricSemantics::Gauge,
                    key,
                    name: METRIC_NAME.to_string(),
                    metric_type,
                    unit: unit.to_string(),
                    value: reading,
                    labels,
                    context: None,
                }
                .into(),
            ));
            count += 1;
        }

        count
    }

    /// Whether the NVIDIA OEM extension marks this report's values as
    /// carried over from a previous interval.
    fn report_is_stale(&self, report: &MetricReport<B>) -> bool {
        match report.oem_nvidia() {
            Ok(Some(oem)) => oem.metric_value_stale.flatten().unwrap_or(false),
            Ok(None) => false,
            Err(error) => {
                tracing::debug!(
                    ?error,
                    bmc_address = ?self.endpoint.addr,
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "Failed to read NVIDIA OEM extension on metric report"
                );
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::Mutex as StdMutex;

    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, Check, check_cases_async, check_values};

    use super::*;
    use crate::collectors::projection_test_support::{ProjectionFixture, TestBmc};
    use crate::endpoint::test_support::{mac, test_endpoint};

    #[test]
    fn metric_type_from_id_cases() {
        check_values(
            [
                Check {
                    scenario: "camel case with an embedded acronym",
                    input: "TotalGPUPowerWatts",
                    expect: "total_gpu_power_watts".to_string(),
                },
                Check {
                    scenario: "already separated identifier",
                    input: "HGX_Chassis_0_HSC_0_Power_0",
                    expect: "hgx_chassis_0_hsc_0_power_0".to_string(),
                },
                Check {
                    scenario: "runs of separators collapse",
                    input: "GPU0__Temp--C",
                    expect: "gpu0_temp_c".to_string(),
                },
                Check {
                    scenario: "leading and trailing separators are trimmed",
                    input: "/Fan.Speed/",
                    expect: "fan_speed".to_string(),
                },
                Check {
                    scenario: "digits do not split an acronym",
                    input: "NVLinkRawTxBandwidthGbps",
                    expect: "nv_link_raw_tx_bandwidth_gbps".to_string(),
                },
            ],
            metric_type_from_id,
        );
    }

    #[test]
    fn leaf_from_property_cases() {
        check_values(
            [
                Check {
                    scenario: "fragment with numeric index",
                    input: "/redfish/v1/Chassis/Tray_1/Power#/0/PowerConsumedWatts",
                    expect: "PowerConsumedWatts",
                },
                Check {
                    scenario: "resource path without fragment",
                    input: "/redfish/v1/Chassis/CH0/Sensors/GPU_0_Temp_0",
                    expect: "GPU_0_Temp_0",
                },
                Check {
                    scenario: "fragment with trailing slash",
                    input: "/redfish/v1/Chassis/CH0/Thermal#/Sensors/GPU_0_Temp_0/",
                    expect: "GPU_0_Temp_0",
                },
                Check {
                    scenario: "empty fragment falls back to resource path",
                    input: "/redfish/v1/Chassis/Tray_1/Power#",
                    expect: "Power",
                },
                Check {
                    scenario: "empty fragment with slash falls back to resource path",
                    input: "/redfish/v1/Chassis/Tray_1/Power#/",
                    expect: "Power",
                },
                Check {
                    scenario: "empty input yields unknown",
                    input: "",
                    expect: "unknown",
                },
            ],
            leaf_from_property,
        );
    }

    #[test]
    fn expand_wildcards_cases() {
        const GPU_TEMP: &str = "/redfish/v1/Chassis/{ChassisId}/Sensors/GPU_{GpuId}_Temp";
        let wildcard = |name: &str, values: &[&str]| {
            (
                name.to_string(),
                values.iter().map(|value| (*value).to_string()).collect(),
            )
        };
        let uris =
            |uris: &[&str]| -> Vec<String> { uris.iter().map(|uri| (*uri).to_string()).collect() };
        check_values(
            [
                Check {
                    scenario: "URI without placeholders is kept as is",
                    input: ("/redfish/v1/Chassis/CH0/Sensors/GPU_0_Temp", vec![]),
                    expect: uris(&["/redfish/v1/Chassis/CH0/Sensors/GPU_0_Temp"]),
                },
                Check {
                    scenario: "several wildcards expand to their cross product",
                    input: (
                        GPU_TEMP,
                        vec![
                            wildcard("ChassisId", &["CH0", "CH1"]),
                            wildcard("GpuId", &["0", "1"]),
                            wildcard("Unused", &["x"]),
                        ],
                    ),
                    expect: uris(&[
                        "/redfish/v1/Chassis/CH0/Sensors/GPU_0_Temp",
                        "/redfish/v1/Chassis/CH0/Sensors/GPU_1_Temp",
                        "/redfish/v1/Chassis/CH1/Sensors/GPU_0_Temp",
                        "/redfish/v1/Chassis/CH1/Sensors/GPU_1_Temp",
                    ]),
                },
                Check {
                    scenario: "the all-values marker is kept as a pattern",
                    input: (
                        GPU_TEMP,
                        vec![
                            wildcard("ChassisId", &["CH0"]),
                            wildcard("GpuId", &["*", "2"]),
                        ],
                    ),
                    expect: uris(&[
                        "/redfish/v1/Chassis/CH0/Sensors/GPU_*_Temp",
                        "/redfish/v1/Chassis/CH0/Sensors/GPU_2_Temp",
                    ]),
                },
                Check {
                    scenario: "a placeholder without a wildcard entry is dropped",
                    input: (GPU_TEMP, vec![wildcard("GpuId", &["0"])]),
                    expect: uris(&[]),
                },
            ],
            |(template, wildcards)| expand_wildcards(template, &wildcards),
        );
    }

    #[test]
    fn glob_matches_cases() {
        const PATTERN: &str = "/redfish/v1/Chassis/CH0/Sensors/GPU_*_Power";
        check_values(
            [
                Check {
                    scenario: "a pattern without a star needs an exact match",
                    input: ("/redfish/v1/A", "/redfish/v1/A"),
                    expect: true,
                },
                Check {
                    scenario: "the star fills part of a path segment",
                    input: (PATTERN, "/redfish/v1/Chassis/CH0/Sensors/GPU_7_Power"),
                    expect: true,
                },
                Check {
                    scenario: "the star needs at least one character",
                    input: (PATTERN, "/redfish/v1/Chassis/CH0/Sensors/GPU__Power"),
                    expect: false,
                },
                Check {
                    scenario: "the star does not cross a path separator",
                    input: (PATTERN, "/redfish/v1/Chassis/CH0/Sensors/GPU_7/8_Power"),
                    expect: false,
                },
                Check {
                    scenario: "text after the pattern is not matched",
                    input: (PATTERN, "/redfish/v1/Chassis/CH0/Sensors/GPU_7_Power_0"),
                    expect: false,
                },
                Check {
                    scenario: "several stars each fill their own segment",
                    input: (
                        "/redfish/v1/Chassis/*/Sensors/*",
                        "/redfish/v1/Chassis/CH0/Sensors/Temp",
                    ),
                    expect: true,
                },
            ],
            |(pattern, text)| glob_matches(pattern, text),
        );
    }

    #[derive(Clone, Debug, PartialEq)]
    struct ObservedMetric {
        key: String,
        name: String,
        metric_type: String,
        unit: String,
        value: f64,
        labels: Vec<(String, String)>,
    }

    #[derive(Default)]
    struct CapturingSink {
        metrics: StdMutex<Vec<ObservedMetric>>,
    }

    impl DataSink for CapturingSink {
        fn sink_type(&self) -> &'static str {
            "capturing_sink"
        }

        fn try_handle_event(
            &self,
            _context: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), HealthError> {
            if let CollectorEvent::Metric(sample) = event {
                self.metrics.lock().unwrap().push(ObservedMetric {
                    key: sample.key.clone(),
                    name: sample.name.clone(),
                    metric_type: sample.metric_type.clone(),
                    unit: sample.unit.clone(),
                    value: sample.value,
                    labels: sample
                        .labels
                        .iter()
                        .map(|(key, value)| (key.to_string(), value.clone()))
                        .collect(),
                });
            }
            Ok(())
        }
    }

    #[derive(Debug, PartialEq)]
    struct ObservedIteration {
        entity_count: Option<usize>,
        fetch_failures: usize,
        metrics: Vec<ObservedMetric>,
    }

    fn metric(
        key: &str,
        metric_type: &str,
        unit: &str,
        value: f64,
        labels: &[(&str, &str)],
    ) -> ObservedMetric {
        ObservedMetric {
            key: key.to_string(),
            name: METRIC_NAME.to_string(),
            metric_type: metric_type.to_string(),
            unit: unit.to_string(),
            value,
            labels: labels
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
        }
    }

    async fn run(bmc: Arc<TestBmc>) -> Result<ObservedIteration, Infallible> {
        let capture = Arc::new(CapturingSink::default());
        let mut collector = TelemetryCollector::new_runner(
            bmc,
            Arc::new(test_endpoint(mac("00:11:22:33:44:55"))),
            TelemetryCollectorConfig {
                data_sink: Some(capture.clone() as Arc<dyn DataSink>),
            },
        )
        .expect("telemetry collector should build");

        let result = collector
            .run_iteration()
            .await
            .expect("telemetry iteration should succeed");
        let mut metrics = capture.metrics.lock().unwrap().clone();
        // Report order across the collection is not guaranteed.
        metrics.sort_by(|left, right| left.key.cmp(&right.key));

        Ok(ObservedIteration {
            entity_count: result.entity_count,
            fetch_failures: result.fetch_failures,
            metrics,
        })
    }

    #[tokio::test]
    async fn telemetry_iteration_cases() {
        let fixture = ProjectionFixture::new().await;

        check_cases_async(
            [Case {
                scenario: "finite readings publish with definition units; \
                           stale, non-numeric and non-finite values are dropped",
                input: fixture.bmc(),
                expect: Yields(ObservedIteration {
                    entity_count: Some(6),
                    fetch_failures: 0,
                    metrics: {
                        const REPORT: &str =
                            "/redfish/v1/TelemetryService/MetricReports/PlatformEnvironmentMetrics";
                        const SENSORS: &str = "/redfish/v1/Chassis/CH0/Sensors";
                        vec![
                            metric(
                                &format!("{SENSORS}/GPU0_Temp/GPU0_Temp"),
                                "gpu0_temp",
                                "celsius",
                                48.0,
                                &[
                                    ("report_id", "PlatformEnvironmentMetrics"),
                                    ("metric_property", &format!("{SENSORS}/GPU0_Temp")),
                                ],
                            ),
                            metric(
                                &format!("{SENSORS}/GPU_0_Temp_0"),
                                "gpu_0_temp_0",
                                "celsius",
                                48.0,
                                &[
                                    ("report_id", "PlatformEnvironmentMetrics"),
                                    ("metric_property", &format!("{SENSORS}/GPU_0_Temp_0")),
                                ],
                            ),
                            // Unit resolved through the wildcard-expanded
                            // MetricProperties of the GPU_Power definition.
                            metric(
                                &format!("{SENSORS}/GPU_1_Power_0"),
                                "gpu_1_power_0",
                                "watts",
                                600.0,
                                &[
                                    ("report_id", "PlatformEnvironmentMetrics"),
                                    ("metric_property", &format!("{SENSORS}/GPU_1_Power_0")),
                                ],
                            ),
                            // A MetricId without a unit-bearing definition
                            // does not borrow the property's unit.
                            metric(
                                &format!("{SENSORS}/GPU_1_Power_0/GPU1_PowerAvg"),
                                "gpu1_power_avg",
                                "unknown",
                                590.0,
                                &[
                                    ("report_id", "PlatformEnvironmentMetrics"),
                                    ("metric_property", &format!("{SENSORS}/GPU_1_Power_0")),
                                ],
                            ),
                            metric(
                                &format!("{SENSORS}/TotalPower/TotalGPUPowerWatts"),
                                "total_gpu_power_watts",
                                "watts",
                                612.5,
                                &[
                                    ("report_id", "PlatformEnvironmentMetrics"),
                                    ("metric_property", &format!("{SENSORS}/TotalPower")),
                                ],
                            ),
                            // Keyed off the report, because this value
                            // declares no MetricProperty.
                            metric(
                                &format!("{REPORT}/FanPWM"),
                                "fan_pwm",
                                // No definition declares a unit for it.
                                "unknown",
                                30.0,
                                &[("report_id", "PlatformEnvironmentMetrics")],
                            ),
                        ]
                    },
                }),
            }],
            run,
        )
        .await;
    }

    /// One failed definitions fetch must not pin every later reading to
    /// the unknown unit: the next iteration retries and picks up the
    /// declared units.
    #[tokio::test]
    async fn units_load_retries_after_transient_definitions_failure() {
        let fixture = ProjectionFixture::with_transient_metric_definitions_failure().await;
        let capture = Arc::new(CapturingSink::default());
        let mut collector = TelemetryCollector::new_runner(
            fixture.bmc(),
            Arc::new(test_endpoint(mac("00:11:22:33:44:55"))),
            TelemetryCollectorConfig {
                data_sink: Some(capture.clone() as Arc<dyn DataSink>),
            },
        )
        .expect("telemetry collector should build");

        let mut observed = Vec::new();
        for _ in 0..2 {
            let result = collector
                .run_iteration()
                .await
                .expect("telemetry iteration should succeed");
            let mut units: Vec<(String, String)> = capture
                .metrics
                .lock()
                .unwrap()
                .drain(..)
                .map(|metric| (metric.metric_type, metric.unit))
                .collect();
            units.sort();
            observed.push((result.fetch_failures, units));
        }

        let unit_pairs = |units: &[(&str, &str)]| -> Vec<(String, String)> {
            units
                .iter()
                .map(|(metric_type, unit)| ((*metric_type).to_string(), (*unit).to_string()))
                .collect()
        };
        assert_eq!(
            observed,
            vec![
                // The 503 counts as a fetch failure and readings still
                // publish, just without units.
                (
                    1,
                    unit_pairs(&[
                        ("fan_pwm", "unknown"),
                        ("gpu0_temp", "unknown"),
                        ("gpu1_power_avg", "unknown"),
                        ("gpu_0_temp_0", "unknown"),
                        ("gpu_1_power_0", "unknown"),
                        ("total_gpu_power_watts", "unknown"),
                    ]),
                ),
                (
                    0,
                    unit_pairs(&[
                        ("fan_pwm", "unknown"),
                        ("gpu0_temp", "celsius"),
                        ("gpu1_power_avg", "unknown"),
                        ("gpu_0_temp_0", "celsius"),
                        ("gpu_1_power_0", "watts"),
                        ("total_gpu_power_watts", "watts"),
                    ]),
                ),
            ]
        );
    }
}
