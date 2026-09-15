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

use std::borrow::Cow;
use std::num::NonZeroUsize;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::{StreamExt, stream};
use nv_redfish::core::{Bmc, EntityTypeRef, ToSnakeCase};
use nv_redfish::schema::sensor::Sensor;
use nv_redfish::sensor::SensorLink;

use crate::HealthError;
use crate::bmc::CollectorSweep;
use crate::collectors::inventory::{DiscoveredEntity, SharedInventory};
use crate::collectors::runtime::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;
use crate::metrics::{MetricLabel, sanitize_unit};
use crate::sink::{CollectorEvent, DataSink, EventContext, MetricSample, SensorThresholdContext};

#[derive(Clone, Copy)]
enum SensorRangeKind {
    Max,
    Min,
}

impl SensorRangeKind {
    fn metric_suffix(self) -> &'static str {
        match self {
            Self::Max => "range_max",
            Self::Min => "range_min",
        }
    }

    fn label_value(self) -> &'static str {
        match self {
            Self::Max => "reading_range_max",
            Self::Min => "reading_range_min",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SensorSweepPlan {
    Skip,
    Collect {
        include_derived_metrics: bool,
        sensor_limit: Option<usize>,
    },
}

impl From<CollectorSweep> for SensorSweepPlan {
    fn from(sweep: CollectorSweep) -> Self {
        match sweep {
            CollectorSweep::Full => Self::Collect {
                include_derived_metrics: true,
                sensor_limit: None,
            },
            CollectorSweep::Probe => Self::Collect {
                include_derived_metrics: false,
                sensor_limit: Some(1),
            },
            CollectorSweep::Skip => Self::Skip,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SensorProjectionError {
    NoHealth,
    IncompleteReading,
}

struct SensorProjection {
    primary: MetricSample,
    ranges: Vec<MetricSample>,
}

/// Configuration for the sensor collector.
pub struct SensorCollectorConfig<B: Bmc> {
    pub data_sink: Option<Arc<dyn DataSink>>,
    pub(crate) shared: SharedInventory<B>,

    /// Bounds local fan-out to the endpoint Redfish operation limit.
    pub request_concurrency: NonZeroUsize,

    pub include_sensor_thresholds: bool,
}

/// Sensor collector for a single BMC endpoint
pub struct SensorCollector<B: Bmc> {
    endpoint: Arc<BmcEndpoint>,
    event_context: EventContext,
    shared: SharedInventory<B>,
    data_sink: Option<Arc<dyn DataSink>>,
    request_concurrency: usize,
    include_sensor_thresholds: bool,
}

impl<B: Bmc + 'static> PeriodicCollector<B> for SensorCollector<B> {
    type Config = SensorCollectorConfig<B>;

    fn new_runner(
        _bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let event_context = EventContext::from_endpoint(endpoint.as_ref(), "sensor_collector");
        Ok(Self {
            endpoint,
            event_context,
            shared: config.shared,
            data_sink: config.data_sink,
            request_concurrency: config.request_concurrency.get(),
            include_sensor_thresholds: config.include_sensor_thresholds,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        let Some(inventory) = self.shared.load_full() else {
            tracing::debug!(
                bmc_address = ?self.endpoint.addr,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "No entity inventory available yet; skipping sensor iteration"
            );
            return Ok(IterationResult {
                refresh_triggered: false,
                entity_count: None,
                fetch_failures: 0,
            });
        };

        // Consult the endpoint's connection circuit breaker. When the BMC is
        // unreachable, firing one request per sensor would block on a connect
        // timeout apiece and log a warning apiece. So: skip entirely while the
        // backoff window is open, and once it elapses send a *single* probe
        // instead of the full fan-out — a still-dead BMC then costs one request,
        // not hundreds, and one fetch is enough to let the breaker self-heal.
        // See NVBug 6036327.
        let sweep_plan = SensorSweepPlan::from(self.endpoint.bmc.collector_sweep());
        let SensorSweepPlan::Collect {
            include_derived_metrics,
            sensor_limit,
        } = sweep_plan
        else {
            tracing::debug!(
                bmc_address = ?self.endpoint.addr,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "BMC connection circuit is open; skipping sensor iteration"
            );
            return Ok(IterationResult {
                refresh_triggered: false,
                entity_count: None,
                fetch_failures: 0,
            });
        };
        let probe_only = sensor_limit == Some(1);

        tracing::debug!(
            bmc_address = ?self.endpoint.addr,
            rack_id = self.event_context.rack_id().map(tracing::field::display),
            generation = inventory.generation,
            inventory_age_seconds = inventory.discovered_at.elapsed().as_secs(),
            entity_count = inventory.entities.len(),
            probe_only,
            "Reading entity inventory snapshot for sensor iteration"
        );

        let fetch_failures = AtomicUsize::new(0);
        self.emit_event(CollectorEvent::MetricCollectionStart);

        // Entity-level derived metrics (drive media life, PSU capacity), once
        // per entity. Skipped while probing — they would emit metrics from stale
        // inventory for a BMC we already believe is down.
        if include_derived_metrics {
            for entity in &inventory.entities {
                self.emit_derived_metrics(entity);
            }
        }

        // Build the fetch futures borrowing from the shared snapshot, then
        // drive them concurrently. Each future borrows `&self`, the entity, and
        // its sensor (all alive for as long as `inventory` is held here). When
        // probing, take just the first sensor: one fetch is enough to test
        // reachability and re-arm or clear the breaker.
        let this = &*self;
        let failures = &fetch_failures;
        let fetches = inventory.entities.iter().flat_map(|entity| {
            entity
                .sensors()
                .iter()
                .map(move |sensor| this.update_sensor(entity, sensor, failures))
        });
        let futures: Vec<_> = fetches.take(sensor_limit.unwrap_or(usize::MAX)).collect();

        let processed: usize = stream::iter(futures)
            .buffer_unordered(self.request_concurrency)
            .collect::<Vec<usize>>()
            .await
            .into_iter()
            .sum();

        self.emit_event(CollectorEvent::MetricCollectionEnd);

        Ok(IterationResult {
            refresh_triggered: false,
            entity_count: Some(processed),
            fetch_failures: fetch_failures.load(Ordering::Relaxed),
        })
    }

    fn collector_type(&self) -> &'static str {
        "sensor_collector"
    }

    async fn stop(&mut self) {
        self.emit_event(CollectorEvent::CollectorRemoved);
    }
}

impl<B: Bmc + 'static> SensorCollector<B> {
    fn emit_event(&self, event: CollectorEvent) {
        if let Some(data_sink) = &self.data_sink {
            data_sink.handle_event(&self.event_context, &event);
        }
    }

    fn emit_derived_metrics(&self, entity: &DiscoveredEntity<B>) {
        let derived = entity.derived_metrics();
        if derived.is_empty() {
            return;
        }
        let mut attributes = entity.base_attributes();
        attributes.extend(entity.entity_specific_attributes());
        for metric in derived {
            self.emit_event(CollectorEvent::Metric(
                MetricSample {
                    key: format!("{}/{}", entity.key(), metric.metric_type),
                    name: "hw".to_string(),
                    metric_type: metric.metric_type.to_string(),
                    unit: metric.unit.to_string(),
                    value: metric.value,
                    labels: attributes.clone(),
                    context: None,
                }
                .into(),
            ));
        }
    }

    async fn update_sensor(
        &self,
        entity: &DiscoveredEntity<B>,
        sensor_link: &SensorLink<B>,
        fetch_failures: &AtomicUsize,
    ) -> usize {
        let sensor = match sensor_link.fetch().await {
            Ok(s) => s,
            Err(e) => {
                fetch_failures.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    sensor_id = %sensor_link.odata_id(),
                    entity_type = entity.entity_type(),
                    error = ?e,
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "Failed to fetch sensor data"
                );
                return 0;
            }
        };

        let projection = match project_sensor(
            &sensor,
            entity.entity_type(),
            entity.physical_context_fallback(),
            entity.base_attributes(),
            entity.entity_specific_attributes(),
            self.include_sensor_thresholds,
        ) {
            Ok(projection) => projection,
            Err(SensorProjectionError::NoHealth) => {
                tracing::debug!(
                    sensor_id = %sensor.base.id,
                    entity_type = entity.entity_type(),
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "Sensor does not have health status field, skipping"
                );
                return 0;
            }
            Err(SensorProjectionError::IncompleteReading) => {
                tracing::warn!(
                    sensor_id = %sensor.base.id,
                    entity_type = entity.entity_type(),
                    rack_id = self.event_context.rack_id().map(tracing::field::display),
                    "Sensor missing required fields (reading, reading_type, or units)"
                );
                return 0;
            }
        };

        self.emit_event(CollectorEvent::Metric(projection.primary.into()));
        for metric in projection.ranges {
            self.emit_event(CollectorEvent::Metric(metric.into()));
        }

        1
    }
}

fn project_sensor(
    sensor: &Sensor,
    entity_type: &str,
    physical_context_fallback: &str,
    base_attributes: Vec<MetricLabel>,
    entity_attributes: Vec<MetricLabel>,
    include_sensor_thresholds: bool,
) -> Result<SensorProjection, SensorProjectionError> {
    let Some(bmc_health) = sensor.status.as_ref().and_then(|s| s.health.flatten()) else {
        return Err(SensorProjectionError::NoHealth);
    };

    let Some((reading, reading_type, unit)) = sensor
        .reading
        .flatten()
        .zip(sensor.reading_type.flatten())
        .zip(sensor.reading_units.clone().flatten())
        .filter(|(_, unit)| !unit.is_empty())
        .map(|((r, rt), u)| (r, rt, u))
    else {
        return Err(SensorProjectionError::IncompleteReading);
    };

    let mut attributes = base_attributes;
    attributes.reserve(6);
    attributes.push((Cow::Borrowed("sensor_name"), sensor.base.id.clone()));

    if let Some(thresholds) = sensor
        .thresholds
        .as_ref()
        .filter(|_| include_sensor_thresholds)
    {
        attributes.push((
            Cow::Borrowed("upper_critical_threshold"),
            thresholds
                .upper_critical
                .as_ref()
                .and_then(|th| th.reading.flatten())
                .unwrap_or_default()
                .to_string(),
        ));
        attributes.push((
            Cow::Borrowed("lower_critical_threshold"),
            thresholds
                .lower_critical
                .as_ref()
                .and_then(|th| th.reading.flatten())
                .unwrap_or_default()
                .to_string(),
        ));
    }

    let physical_context = sensor
        .physical_context
        .flatten()
        .map(|phc| phc.to_snake_case().to_string())
        .unwrap_or_else(|| physical_context_fallback.to_string());
    attributes.push((Cow::Borrowed("physical_context"), physical_context));
    attributes.extend(entity_attributes);

    let metric_type = reading_type.to_snake_case().to_string();
    let unit = sanitize_unit(&unit);
    let range_max = sensor.reading_range_max.flatten();
    let range_min = sensor.reading_range_min.flatten();

    let (upper_fatal, lower_fatal, upper_critical, lower_critical, upper_caution, lower_caution) =
        if let Some(thresholds) = &sensor.thresholds {
            (
                thresholds
                    .upper_fatal
                    .as_ref()
                    .and_then(|t| t.reading.flatten()),
                thresholds
                    .lower_fatal
                    .as_ref()
                    .and_then(|t| t.reading.flatten()),
                thresholds
                    .upper_critical
                    .as_ref()
                    .and_then(|t| t.reading.flatten()),
                thresholds
                    .lower_critical
                    .as_ref()
                    .and_then(|t| t.reading.flatten()),
                thresholds
                    .upper_caution
                    .as_ref()
                    .and_then(|t| t.reading.flatten()),
                thresholds
                    .lower_caution
                    .as_ref()
                    .and_then(|t| t.reading.flatten()),
            )
        } else {
            (None, None, None, None, None, None)
        };

    let primary = MetricSample {
        key: sensor.odata_id().to_string(),
        name: "hw_sensor".to_string(),
        metric_type: metric_type.clone(),
        unit: unit.clone(),
        value: reading,
        labels: attributes.clone(),
        context: Some(SensorThresholdContext {
            entity_type: entity_type.to_string(),
            sensor_id: sensor.base.id.clone(),
            upper_fatal,
            lower_fatal,
            upper_critical,
            lower_critical,
            upper_caution,
            lower_caution,
            range_max,
            range_min,
            bmc_health,
        }),
    };

    let ranges = if include_sensor_thresholds {
        [
            (SensorRangeKind::Max, range_max),
            (SensorRangeKind::Min, range_min),
        ]
        .into_iter()
        .filter_map(|(range_kind, value)| {
            sensor_range_metric(
                sensor.odata_id().to_string(),
                &metric_type,
                &unit,
                &attributes,
                range_kind,
                value,
            )
        })
        .collect()
    } else {
        Vec::new()
    };

    Ok(SensorProjection { primary, ranges })
}

fn sensor_range_metric(
    sensor_key: String,
    reading_type: &str,
    unit: &str,
    attributes: &[MetricLabel],
    range_kind: SensorRangeKind,
    value: Option<f64>,
) -> Option<MetricSample> {
    let value = value?;
    let metric_suffix = range_kind.metric_suffix();
    let mut labels = attributes.to_vec();
    labels.push((
        Cow::Borrowed("sensor_range"),
        range_kind.label_value().to_string(),
    ));
    Some(MetricSample {
        key: format!("{sensor_key}/{metric_suffix}"),
        name: "hw_sensor".to_string(),
        metric_type: format!("{reading_type}_{metric_suffix}"),
        unit: unit.to_string(),
        value,
        labels,
        context: None,
    })
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::sync::Mutex as StdMutex;
    use std::time::Instant;

    use arc_swap::ArcSwapOption;
    use bmc_mock::injection::{Action, Rule, RuleId, Selector};
    use bmc_mock::test_support::{TestBmc, TestBmcHandle, liteon_powershelf_bmc};
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, Check, check_cases, check_cases_async, check_values};
    use serde_json::{Value, json};

    use super::*;
    use crate::collectors::inventory::EntityInventory;
    use crate::endpoint::test_support::{mac, test_endpoint};

    const SENSOR_PATH: &str = "/redfish/v1/Chassis/powershelf/Sensors/Temp_1";

    #[derive(Clone, Debug, PartialEq)]
    struct ObservedContext {
        entity_type: String,
        sensor_id: String,
        upper_fatal: Option<f64>,
        lower_fatal: Option<f64>,
        upper_critical: Option<f64>,
        lower_critical: Option<f64>,
        upper_caution: Option<f64>,
        lower_caution: Option<f64>,
        range_max: Option<f64>,
        range_min: Option<f64>,
        bmc_health: String,
    }

    impl From<&SensorThresholdContext> for ObservedContext {
        fn from(context: &SensorThresholdContext) -> Self {
            Self {
                entity_type: context.entity_type.clone(),
                sensor_id: context.sensor_id.clone(),
                upper_fatal: context.upper_fatal,
                lower_fatal: context.lower_fatal,
                upper_critical: context.upper_critical,
                lower_critical: context.lower_critical,
                upper_caution: context.upper_caution,
                lower_caution: context.lower_caution,
                range_max: context.range_max,
                range_min: context.range_min,
                bmc_health: context.bmc_health.to_snake_case().to_string(),
            }
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct ObservedMetric {
        key: String,
        name: String,
        metric_type: String,
        unit: String,
        value: f64,
        labels: Vec<(String, String)>,
        context: Option<ObservedContext>,
    }

    impl From<&MetricSample> for ObservedMetric {
        fn from(sample: &MetricSample) -> Self {
            Self {
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
                context: sample.context.as_ref().map(ObservedContext::from),
            }
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    struct ObservedProjection {
        primary: ObservedMetric,
        ranges: Vec<ObservedMetric>,
    }

    fn observe_projection(projection: SensorProjection) -> ObservedProjection {
        ObservedProjection {
            primary: ObservedMetric::from(&projection.primary),
            ranges: projection.ranges.iter().map(ObservedMetric::from).collect(),
        }
    }

    #[derive(Clone, Debug, PartialEq)]
    enum ObservedEvent {
        Start,
        Metric(Box<ObservedMetric>),
        End,
        Removed,
    }

    #[derive(Default)]
    struct CapturingSink {
        events: StdMutex<Vec<ObservedEvent>>,
    }

    impl CapturingSink {
        fn events(&self) -> Vec<ObservedEvent> {
            self.events.lock().unwrap().clone()
        }
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
            let observed = match event {
                CollectorEvent::MetricCollectionStart => Some(ObservedEvent::Start),
                CollectorEvent::Metric(sample) => Some(ObservedEvent::Metric(Box::new(
                    ObservedMetric::from(sample.as_ref()),
                ))),
                CollectorEvent::MetricCollectionEnd => Some(ObservedEvent::End),
                CollectorEvent::CollectorRemoved => Some(ObservedEvent::Removed),
                CollectorEvent::Log(_)
                | CollectorEvent::Firmware(_)
                | CollectorEvent::HealthReport(_) => None,
            };

            if let Some(observed) = observed {
                self.events.lock().unwrap().push(observed);
            }
            Ok(())
        }
    }

    struct ProjectionInput {
        sensor: Value,
        entity_type: &'static str,
        physical_context_fallback: &'static str,
        base_attributes: Vec<(&'static str, &'static str)>,
        entity_attributes: Vec<(&'static str, &'static str)>,
        include_sensor_thresholds: bool,
    }

    fn metric_labels(labels: Vec<(&'static str, &'static str)>) -> Vec<MetricLabel> {
        labels
            .into_iter()
            .map(|(key, value)| (Cow::Borrowed(key), value.to_string()))
            .collect()
    }

    fn observed_labels(labels: &[(&str, &str)]) -> Vec<(String, String)> {
        labels
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect()
    }

    fn sensor_json() -> Value {
        json!({
            "@odata.id": SENSOR_PATH,
            "@odata.type": "#Sensor.v1_6_0.Sensor",
            "Id": "Sensor0",
            "Name": "Sensor 0",
            "Reading": 42.5,
            "ReadingType": "Temperature",
            "ReadingUnits": "Cel",
            "Status": {
                "Health": "OK",
                "State": "Enabled"
            }
        })
    }

    fn sensor_json_with(overrides: impl IntoIterator<Item = (&'static str, Value)>) -> Value {
        let mut sensor = sensor_json();
        let fields = sensor.as_object_mut().expect("sensor fixture is an object");
        for (name, value) in overrides {
            fields.insert(name.to_string(), value);
        }
        sensor
    }

    fn sensor_json_without(fields: &[&str]) -> Value {
        let mut sensor = sensor_json();
        let object = sensor.as_object_mut().expect("sensor fixture is an object");
        for field in fields {
            object.remove(*field);
        }
        sensor
    }

    fn threshold_sensor_json() -> Value {
        sensor_json_with([
            ("PhysicalContext", json!("CPU")),
            ("ReadingRangeMax", json!(2.0)),
            ("ReadingRangeMin", json!(1.0)),
            (
                "Status",
                json!({
                    "Health": "Warning",
                    "State": "Enabled"
                }),
            ),
            (
                "Thresholds",
                json!({
                    "UpperFatal": { "Reading": 1.9 },
                    "LowerFatal": { "Reading": 1.1 },
                    "UpperCritical": { "Reading": 1.8 },
                    "LowerCritical": { "Reading": 1.2 },
                    "UpperCaution": { "Reading": 1.7 },
                    "LowerCaution": { "Reading": 1.3 }
                }),
            ),
        ])
    }

    fn empty_context(entity_type: &str, sensor_id: &str, bmc_health: &str) -> ObservedContext {
        ObservedContext {
            entity_type: entity_type.to_string(),
            sensor_id: sensor_id.to_string(),
            upper_fatal: None,
            lower_fatal: None,
            upper_critical: None,
            lower_critical: None,
            upper_caution: None,
            lower_caution: None,
            range_max: None,
            range_min: None,
            bmc_health: bmc_health.to_string(),
        }
    }

    fn threshold_context() -> ObservedContext {
        ObservedContext {
            entity_type: "processor".to_string(),
            sensor_id: "Sensor0".to_string(),
            upper_fatal: Some(1.9),
            lower_fatal: Some(1.1),
            upper_critical: Some(1.8),
            lower_critical: Some(1.2),
            upper_caution: Some(1.7),
            lower_caution: Some(1.3),
            range_max: Some(2.0),
            range_min: Some(1.0),
            bmc_health: "warning".to_string(),
        }
    }

    fn observed_metric(
        key: &str,
        name: &str,
        metric_type: &str,
        unit: &str,
        value: f64,
        labels: &[(&str, &str)],
        context: Option<ObservedContext>,
    ) -> ObservedMetric {
        ObservedMetric {
            key: key.to_string(),
            name: name.to_string(),
            metric_type: metric_type.to_string(),
            unit: unit.to_string(),
            value,
            labels: observed_labels(labels),
            context,
        }
    }

    fn project(input: ProjectionInput) -> Result<ObservedProjection, SensorProjectionError> {
        let sensor = serde_json::from_value(input.sensor).expect("valid sensor fixture");
        project_sensor(
            &sensor,
            input.entity_type,
            input.physical_context_fallback,
            metric_labels(input.base_attributes),
            metric_labels(input.entity_attributes),
            input.include_sensor_thresholds,
        )
        .map(observe_projection)
    }

    #[test]
    fn incomplete_sensor_cases() {
        check_cases(
            [
                Case {
                    scenario: "missing status",
                    input: sensor_json_without(&["Status"]),
                    expect: Yields(SensorProjectionError::NoHealth),
                },
                Case {
                    scenario: "status missing health",
                    input: sensor_json_with([("Status", json!({ "State": "Enabled" }))]),
                    expect: Yields(SensorProjectionError::NoHealth),
                },
                Case {
                    scenario: "null health",
                    input: sensor_json_with([(
                        "Status",
                        json!({ "Health": null, "State": "Enabled" }),
                    )]),
                    expect: Yields(SensorProjectionError::NoHealth),
                },
                Case {
                    scenario: "missing reading",
                    input: sensor_json_without(&["Reading"]),
                    expect: Yields(SensorProjectionError::IncompleteReading),
                },
                Case {
                    scenario: "null reading",
                    input: sensor_json_with([("Reading", Value::Null)]),
                    expect: Yields(SensorProjectionError::IncompleteReading),
                },
                Case {
                    scenario: "missing reading type",
                    input: sensor_json_without(&["ReadingType"]),
                    expect: Yields(SensorProjectionError::IncompleteReading),
                },
                Case {
                    scenario: "empty reading units",
                    input: sensor_json_with([("ReadingUnits", json!(""))]),
                    expect: Yields(SensorProjectionError::IncompleteReading),
                },
                Case {
                    scenario: "missing reading units",
                    input: sensor_json_without(&["ReadingUnits"]),
                    expect: Yields(SensorProjectionError::IncompleteReading),
                },
            ],
            |sensor| {
                let sensor = serde_json::from_value(sensor).expect("valid sensor fixture");
                let error = match project_sensor(
                    &sensor,
                    "chassis",
                    "chassis",
                    Vec::new(),
                    Vec::new(),
                    false,
                ) {
                    Ok(_) => panic!("incomplete sensor must be rejected"),
                    Err(error) => error,
                };
                Ok::<_, Infallible>(error)
            },
        );
    }

    #[test]
    fn accepted_projection_cases() {
        let threshold_labels = [
            ("processor_id", "CPU0"),
            ("system_id", "SYS0"),
            ("sensor_name", "Sensor0"),
            ("upper_critical_threshold", "1.8"),
            ("lower_critical_threshold", "1.2"),
            ("physical_context", "cpu"),
            ("processor_type", "cpu"),
        ];
        let threshold_labels_without_exposition = [
            ("processor_id", "CPU0"),
            ("system_id", "SYS0"),
            ("sensor_name", "Sensor0"),
            ("physical_context", "cpu"),
            ("processor_type", "cpu"),
        ];

        check_cases(
            [
                Case {
                    scenario: "entity physical-context fallback",
                    input: ProjectionInput {
                        sensor: sensor_json(),
                        entity_type: "chassis",
                        physical_context_fallback: "chassis",
                        base_attributes: vec![("chassis_id", "CH0")],
                        entity_attributes: vec![("model", "Baseboard")],
                        include_sensor_thresholds: false,
                    },
                    expect: Yields(ObservedProjection {
                        primary: observed_metric(
                            SENSOR_PATH,
                            "hw_sensor",
                            "temperature",
                            "celsius",
                            42.5,
                            &[
                                ("chassis_id", "CH0"),
                                ("sensor_name", "Sensor0"),
                                ("physical_context", "chassis"),
                                ("model", "Baseboard"),
                            ],
                            Some(empty_context("chassis", "Sensor0", "ok")),
                        ),
                        ranges: Vec::new(),
                    }),
                },
                Case {
                    scenario: "threshold labels and range metrics",
                    input: ProjectionInput {
                        sensor: threshold_sensor_json(),
                        entity_type: "processor",
                        physical_context_fallback: "socket",
                        base_attributes: vec![("processor_id", "CPU0"), ("system_id", "SYS0")],
                        entity_attributes: vec![("processor_type", "cpu")],
                        include_sensor_thresholds: true,
                    },
                    expect: Yields(ObservedProjection {
                        primary: observed_metric(
                            SENSOR_PATH,
                            "hw_sensor",
                            "temperature",
                            "celsius",
                            42.5,
                            &threshold_labels,
                            Some(threshold_context()),
                        ),
                        ranges: vec![
                            observed_metric(
                                &format!("{SENSOR_PATH}/range_max"),
                                "hw_sensor",
                                "temperature_range_max",
                                "celsius",
                                2.0,
                                &[
                                    ("processor_id", "CPU0"),
                                    ("system_id", "SYS0"),
                                    ("sensor_name", "Sensor0"),
                                    ("upper_critical_threshold", "1.8"),
                                    ("lower_critical_threshold", "1.2"),
                                    ("physical_context", "cpu"),
                                    ("processor_type", "cpu"),
                                    ("sensor_range", "reading_range_max"),
                                ],
                                None,
                            ),
                            observed_metric(
                                &format!("{SENSOR_PATH}/range_min"),
                                "hw_sensor",
                                "temperature_range_min",
                                "celsius",
                                1.0,
                                &[
                                    ("processor_id", "CPU0"),
                                    ("system_id", "SYS0"),
                                    ("sensor_name", "Sensor0"),
                                    ("upper_critical_threshold", "1.8"),
                                    ("lower_critical_threshold", "1.2"),
                                    ("physical_context", "cpu"),
                                    ("processor_type", "cpu"),
                                    ("sensor_range", "reading_range_min"),
                                ],
                                None,
                            ),
                        ],
                    }),
                },
                Case {
                    scenario: "threshold context without threshold exposition",
                    input: ProjectionInput {
                        sensor: threshold_sensor_json(),
                        entity_type: "processor",
                        physical_context_fallback: "socket",
                        base_attributes: vec![("processor_id", "CPU0"), ("system_id", "SYS0")],
                        entity_attributes: vec![("processor_type", "cpu")],
                        include_sensor_thresholds: false,
                    },
                    expect: Yields(ObservedProjection {
                        primary: observed_metric(
                            SENSOR_PATH,
                            "hw_sensor",
                            "temperature",
                            "celsius",
                            42.5,
                            &threshold_labels_without_exposition,
                            Some(threshold_context()),
                        ),
                        ranges: Vec::new(),
                    }),
                },
                Case {
                    scenario: "threshold exposition without sensor thresholds",
                    input: ProjectionInput {
                        sensor: sensor_json(),
                        entity_type: "chassis",
                        physical_context_fallback: "chassis",
                        base_attributes: vec![("chassis_id", "CH0")],
                        entity_attributes: vec![("model", "Baseboard")],
                        include_sensor_thresholds: true,
                    },
                    expect: Yields(ObservedProjection {
                        primary: observed_metric(
                            SENSOR_PATH,
                            "hw_sensor",
                            "temperature",
                            "celsius",
                            42.5,
                            &[
                                ("chassis_id", "CH0"),
                                ("sensor_name", "Sensor0"),
                                ("physical_context", "chassis"),
                                ("model", "Baseboard"),
                            ],
                            Some(empty_context("chassis", "Sensor0", "ok")),
                        ),
                        ranges: Vec::new(),
                    }),
                },
                Case {
                    scenario: "critical percentage power-supply reading",
                    input: ProjectionInput {
                        sensor: sensor_json_with([
                            ("Reading", json!(90.0)),
                            ("ReadingType", json!("Percent")),
                            ("ReadingUnits", json!("%")),
                            (
                                "Status",
                                json!({ "Health": "Critical", "State": "Enabled" }),
                            ),
                        ]),
                        entity_type: "powersupply",
                        physical_context_fallback: "power_supply",
                        base_attributes: vec![("powersupply_id", "PSU0"), ("chassis_id", "CH0")],
                        entity_attributes: vec![("model", "PSU-3KW")],
                        include_sensor_thresholds: false,
                    },
                    expect: Yields(ObservedProjection {
                        primary: observed_metric(
                            SENSOR_PATH,
                            "hw_sensor",
                            "percent",
                            "percent",
                            90.0,
                            &[
                                ("powersupply_id", "PSU0"),
                                ("chassis_id", "CH0"),
                                ("sensor_name", "Sensor0"),
                                ("physical_context", "power_supply"),
                                ("model", "PSU-3KW"),
                            ],
                            Some(empty_context("powersupply", "Sensor0", "critical")),
                        ),
                        ranges: Vec::new(),
                    }),
                },
            ],
            project,
        );
    }

    #[test]
    fn sweep_plan_cases() {
        check_values(
            [
                Check {
                    scenario: "full sweep",
                    input: CollectorSweep::Full,
                    expect: SensorSweepPlan::Collect {
                        include_derived_metrics: true,
                        sensor_limit: None,
                    },
                },
                Check {
                    scenario: "probe sweep",
                    input: CollectorSweep::Probe,
                    expect: SensorSweepPlan::Collect {
                        include_derived_metrics: false,
                        sensor_limit: Some(1),
                    },
                },
                Check {
                    scenario: "skipped sweep",
                    input: CollectorSweep::Skip,
                    expect: SensorSweepPlan::Skip,
                },
            ],
            SensorSweepPlan::from,
        );
    }

    #[derive(Clone, Copy)]
    enum CollectionCase {
        MissingInventory,
        EmptyInventory,
        Sensor,
        SensorFetchFailure,
        DerivedMetric,
        Stop,
    }

    #[derive(Debug, PartialEq)]
    struct ObservedIteration {
        refresh_triggered: bool,
        entity_count: Option<usize>,
        fetch_failures: usize,
    }

    impl From<IterationResult> for ObservedIteration {
        fn from(result: IterationResult) -> Self {
            Self {
                refresh_triggered: result.refresh_triggered,
                entity_count: result.entity_count,
                fetch_failures: result.fetch_failures,
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct CollectionObservation {
        iteration: Option<ObservedIteration>,
        events: Vec<ObservedEvent>,
    }

    async fn first_chassis(handle: &TestBmcHandle) -> Arc<nv_redfish::chassis::Chassis<TestBmc>> {
        let collection = handle
            .service_root
            .chassis()
            .await
            .expect("chassis collection")
            .expect("fixture has chassis");
        Arc::new(
            collection
                .members()
                .await
                .expect("chassis members")
                .into_iter()
                .next()
                .expect("fixture has one chassis"),
        )
    }

    fn store_inventory(
        shared: &SharedInventory<TestBmc>,
        entities: Vec<DiscoveredEntity<TestBmc>>,
    ) {
        shared.store(Some(Arc::new(EntityInventory {
            entities,
            discovered_at: Instant::now(),
            generation: 1,
        })));
    }

    async fn observe_collection(case: CollectionCase) -> Result<CollectionObservation, Infallible> {
        let handle = liteon_powershelf_bmc().await;
        let endpoint = Arc::new(test_endpoint(mac("00:11:22:33:44:55")));
        let shared = Arc::new(ArcSwapOption::empty());
        let sink = Arc::new(CapturingSink::default());
        let mut collector = SensorCollector::<TestBmc> {
            endpoint: endpoint.clone(),
            event_context: EventContext::from_endpoint(endpoint.as_ref(), "sensor_collector"),
            shared: shared.clone(),
            data_sink: Some(sink.clone()),
            request_concurrency: 2,
            include_sensor_thresholds: true,
        };

        match case {
            CollectionCase::MissingInventory => {}
            CollectionCase::EmptyInventory => store_inventory(&shared, Vec::new()),
            CollectionCase::Sensor | CollectionCase::SensorFetchFailure => {
                let chassis = first_chassis(&handle).await;
                let sensor = chassis
                    .sensor_links()
                    .await
                    .expect("sensor links")
                    .expect("fixture has sensors")
                    .into_iter()
                    .next()
                    .expect("fixture has a sensor");
                assert_eq!(sensor.odata_id().to_string(), SENSOR_PATH);

                handle.state.injection.upsert(Rule {
                    id: RuleId::from("sensor-collection"),
                    selector: Selector::OdataId(SENSOR_PATH.to_string()),
                    action: match case {
                        CollectionCase::Sensor => Action::Replace(sensor_json()),
                        CollectionCase::SensorFetchFailure => Action::Status(500),
                        _ => unreachable!(),
                    },
                    remaining: None,
                });
                store_inventory(
                    &shared,
                    vec![DiscoveredEntity::Chassis {
                        entity: chassis,
                        sensors: vec![sensor],
                        gpu: None,
                    }],
                );
            }
            CollectionCase::DerivedMetric => {
                handle.state.injection.upsert(Rule {
                    id: RuleId::from("power-supply-capacity"),
                    selector: Selector::OdataId(
                        "/redfish/v1/Chassis/*/PowerSubsystem/PowerSupplies/*".to_string(),
                    ),
                    action: Action::JsonMerge(json!({
                        "Model": "PSU-3KW",
                        "PowerCapacityWatts": 3000.0
                    })),
                    remaining: None,
                });
                let chassis = first_chassis(&handle).await;
                let power_supply = Arc::new(
                    chassis
                        .power_supplies()
                        .await
                        .expect("power supplies")
                        .into_iter()
                        .next()
                        .expect("fixture has a power supply"),
                );
                store_inventory(
                    &shared,
                    vec![DiscoveredEntity::PowerSupply {
                        entity: power_supply,
                        chassis,
                        sensors: Vec::new(),
                    }],
                );
            }
            CollectionCase::Stop => {
                collector.stop().await;
                return Ok(CollectionObservation {
                    iteration: None,
                    events: sink.events(),
                });
            }
        }

        let iteration = collector
            .run_iteration()
            .await
            .expect("sensor collection succeeds");
        Ok(CollectionObservation {
            iteration: Some(iteration.into()),
            events: sink.events(),
        })
    }

    #[tokio::test]
    async fn collection_cases() {
        check_cases_async(
            [
                Case {
                    scenario: "missing inventory",
                    input: CollectionCase::MissingInventory,
                    expect: Yields(CollectionObservation {
                        iteration: Some(ObservedIteration {
                            refresh_triggered: false,
                            entity_count: None,
                            fetch_failures: 0,
                        }),
                        events: Vec::new(),
                    }),
                },
                Case {
                    scenario: "empty inventory",
                    input: CollectionCase::EmptyInventory,
                    expect: Yields(CollectionObservation {
                        iteration: Some(ObservedIteration {
                            refresh_triggered: false,
                            entity_count: Some(0),
                            fetch_failures: 0,
                        }),
                        events: vec![ObservedEvent::Start, ObservedEvent::End],
                    }),
                },
                Case {
                    scenario: "sensor metric",
                    input: CollectionCase::Sensor,
                    expect: Yields(CollectionObservation {
                        iteration: Some(ObservedIteration {
                            refresh_triggered: false,
                            entity_count: Some(1),
                            fetch_failures: 0,
                        }),
                        events: vec![
                            ObservedEvent::Start,
                            ObservedEvent::Metric(Box::new(observed_metric(
                                SENSOR_PATH,
                                "hw_sensor",
                                "temperature",
                                "celsius",
                                42.5,
                                &[
                                    ("chassis_id", "powershelf"),
                                    ("sensor_name", "Sensor0"),
                                    ("physical_context", "chassis"),
                                    ("model", "PF-1333-7R"),
                                ],
                                Some(empty_context("chassis", "Sensor0", "ok")),
                            ))),
                            ObservedEvent::End,
                        ],
                    }),
                },
                Case {
                    scenario: "sensor fetch failure",
                    input: CollectionCase::SensorFetchFailure,
                    expect: Yields(CollectionObservation {
                        iteration: Some(ObservedIteration {
                            refresh_triggered: false,
                            entity_count: Some(0),
                            fetch_failures: 1,
                        }),
                        events: vec![ObservedEvent::Start, ObservedEvent::End],
                    }),
                },
                Case {
                    scenario: "derived metric",
                    input: CollectionCase::DerivedMetric,
                    expect: Yields(CollectionObservation {
                        iteration: Some(ObservedIteration {
                            refresh_triggered: false,
                            entity_count: Some(0),
                            fetch_failures: 0,
                        }),
                        events: vec![
                            ObservedEvent::Start,
                            ObservedEvent::Metric(Box::new(observed_metric(
                                "/redfish/v1/Chassis/powershelf/PowerSubsystem/PowerSupplies/0/powersupply_capacity",
                                "hw",
                                "powersupply_capacity",
                                "watts",
                                3000.0,
                                &[
                                    ("powersupply_id", "0"),
                                    ("chassis_id", "powershelf"),
                                    ("model", "PSU-3KW"),
                                ],
                                None,
                            ))),
                            ObservedEvent::End,
                        ],
                    }),
                },
                Case {
                    scenario: "collector stop",
                    input: CollectionCase::Stop,
                    expect: Yields(CollectionObservation {
                        iteration: None,
                        events: vec![ObservedEvent::Removed],
                    }),
                },
            ],
            observe_collection,
        )
        .await;
    }
}
