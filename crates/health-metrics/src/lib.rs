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

use std::collections::{HashMap, HashSet};
use std::fmt::Debug;
use std::hash::Hash;
use std::sync::Arc;

use carbide_utils::metrics::SharedMetricsHolder;
use health_report::{HealthAlertClassification, HealthProbeId, HealthReport};
use model::health::HealthReportSources;
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;

mod per_object;
pub use per_object::{PerObjectGauge, PerObjectMetricsRegistry};

pub trait HealthMetricDimension:
    Hash + Eq + Clone + Default + Debug + Send + Sync + 'static
{
    fn key_values(&self) -> Vec<KeyValue>;

    fn all_values() -> Vec<Self>;
}

impl HealthMetricDimension for () {
    fn key_values(&self) -> Vec<KeyValue> {
        Vec::new()
    }

    fn all_values() -> Vec<Self> {
        vec![()]
    }
}

/// Per object snapshot of the health-related signals
#[derive(Debug, Default, Clone)]
pub struct HealthObjectMetrics {
    pub object_id: String,
    pub health_probe_alerts: HashSet<(HealthProbeId, Option<String>)>,
    pub health_alert_classifications: HashSet<HealthAlertClassification>,
    pub alerts_suppressed: bool,
    pub num_merge_overrides: usize,
    pub replace_override_enabled: bool,
}

impl HealthObjectMetrics {
    pub fn populate(
        &mut self,
        object_id: String,
        aggregate_health: &HealthReport,
        sources: &HealthReportSources,
    ) {
        self.object_id = object_id;

        let suppress_alerts = HealthAlertClassification::suppress_external_alerting();
        for alert in aggregate_health.alerts.iter() {
            self.health_probe_alerts
                .insert((alert.id.clone(), alert.target.clone()));
            for c in alert.classifications.iter() {
                self.health_alert_classifications.insert(c.clone());
                if *c == suppress_alerts {
                    self.alerts_suppressed = true;
                }
            }
        }

        self.num_merge_overrides = sources.merges.len();
        self.replace_override_enabled = sources.replace.is_some();
    }
}

#[derive(Debug, Default)]
pub struct HealthIterationMetrics<D: HealthMetricDimension> {
    pub healthy: HashMap<(bool, D), usize>,
    pub unhealthy_by_probe_id: HashMap<(String, Option<String>, D), usize>,
    pub unhealthy_by_classification_id: HashMap<(String, D), usize>,
    pub alerts_suppressed_by_object_id: HashSet<String>,
    pub num_overrides: HashMap<(&'static str, D), usize>,
}

impl<D: HealthMetricDimension> HealthIterationMetrics<D> {
    pub fn merge(&mut self, dimension: D, object: &HealthObjectMetrics) {
        let is_healthy = object.health_probe_alerts.is_empty();
        *self
            .healthy
            .entry((is_healthy, dimension.clone()))
            .or_default() += 1;

        for (probe_id, target) in &object.health_probe_alerts {
            *self
                .unhealthy_by_probe_id
                .entry((probe_id.to_string(), target.clone(), dimension.clone()))
                .or_default() += 1;
        }
        for classification in &object.health_alert_classifications {
            *self
                .unhealthy_by_classification_id
                .entry((classification.to_string(), dimension.clone()))
                .or_default() += 1;
        }
        if object.alerts_suppressed {
            self.alerts_suppressed_by_object_id
                .insert(object.object_id.clone());
        }
        *self
            .num_overrides
            .entry(("merge", dimension.clone()))
            .or_default() += object.num_merge_overrides;
        if object.replace_override_enabled {
            *self
                .num_overrides
                .entry(("replace", dimension))
                .or_default() += 1;
        }
    }
}

pub fn register_health_gauges<T, D, F>(
    metric_prefix: &str,
    suppressed_label_key: &'static str,
    display_name_plural: &str,
    meter: &Meter,
    shared: SharedMetricsHolder<T>,
    project: F,
) where
    T: Debug + Send + Sync + 'static,
    D: HealthMetricDimension,
    F: Fn(&T) -> &HealthIterationMetrics<D> + Send + Sync + 'static,
{
    let project = Arc::new(project);

    // {prefix}_health_status_count
    {
        let metrics = shared.clone();
        let project = project.clone();
        meter
            .u64_observable_gauge(format!("{metric_prefix}_health_status_count"))
            .with_description(
                format!("Number of {display_name_plural} in the system that have reported either a healthy or not healthy status - based on the presence of health probe alerts"),
            )
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    let health = project(metrics);
                    for healthy in [true, false] {
                        for dimension in D::all_values() {
                            let count = health
                                .healthy
                                .get(&(healthy, dimension.clone()))
                                .copied()
                                .unwrap_or_default();
                            let mut labels = vec![KeyValue::new("healthy", healthy.to_string())];
                            labels.extend(dimension.key_values());
                            observer.observe(count as u64, &[attrs, &labels].concat());
                        }
                    }
                })
            })
            .build();
    }

    // {prefix}_health_overrides_count
    {
        let metrics = shared.clone();
        let project = project.clone();
        meter
            .u64_observable_gauge(format!("{metric_prefix}_health_overrides_count"))
            .with_description("Number of health overrides configured in the site")
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    let health = project(metrics);
                    for override_type in ["merge", "replace"] {
                        for dimension in D::all_values() {
                            let count = health
                                .num_overrides
                                .get(&(override_type, dimension.clone()))
                                .copied()
                                .unwrap_or_default();
                            let mut labels =
                                vec![KeyValue::new("override_type", override_type.to_string())];
                            labels.extend(dimension.key_values());
                            observer.observe(count as u64, &[attrs, &labels].concat());
                        }
                    }
                })
            })
            .build();
    }

    // {prefix}_unhealthy_by_probe_id_count
    {
        let metrics = shared.clone();
        let project = project.clone();
        meter
            .u64_observable_gauge(format!("{metric_prefix}_unhealthy_by_probe_id_count"))
            .with_description("Number of objects which reported a certain Health Probe Alert")
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    let health = project(metrics);
                    for ((probe, target, dimension), count) in &health.unhealthy_by_probe_id {
                        let mut labels = vec![
                            KeyValue::new("probe_id", probe.clone()),
                            KeyValue::new("probe_target", target.clone().unwrap_or_default()),
                        ];
                        labels.extend(dimension.key_values());
                        observer.observe(*count as u64, &[attrs, &labels].concat());
                    }
                })
            })
            .build();
    }

    // {prefix}_unhealthy_by_classification_count
    {
        let metrics = shared.clone();
        let project = project.clone();
        meter
            .u64_observable_gauge(format!("{metric_prefix}_unhealthy_by_classification_count"))
            .with_description(
                "Number of objects marked with a certain classification due to being unhealthy",
            )
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    let health = project(metrics);
                    for ((classification, dimension), count) in
                        &health.unhealthy_by_classification_id
                    {
                        let mut labels =
                            vec![KeyValue::new("classification", classification.clone())];
                        labels.extend(dimension.key_values());
                        observer.observe(*count as u64, &[attrs, &labels].concat());
                    }
                })
            })
            .build();
    }

    // {prefix}_alerts_suppressed_count
    register_alerts_suppressed_gauge(
        &format!("{metric_prefix}_alerts_suppressed_count"),
        suppressed_label_key,
        meter,
        shared,
        move |m| &project(m).alerts_suppressed_by_object_id,
    );
}

pub fn register_alerts_suppressed_gauge<T, F>(
    metric_name: &str,
    suppressed_label_key: &'static str,
    meter: &Meter,
    shared: SharedMetricsHolder<T>,
    project: F,
) where
    T: Debug + Send + Sync + 'static,
    F: Fn(&T) -> &HashSet<String> + Send + Sync + 'static,
{
    meter
        .u64_observable_gauge(metric_name.to_string())
        .with_description(
            "Whether external metrics based alerting is suppressed for a specific object",
        )
        .with_callback(move |observer| {
            shared.if_available(|metrics, attrs| {
                for object_id in project(metrics) {
                    observer.observe(
                        1u64,
                        &[
                            attrs,
                            &[KeyValue::new(suppressed_label_key, object_id.clone())],
                        ]
                        .concat(),
                    );
                }
            })
        })
        .build();
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use health_report::{HealthProbeAlert, HealthReport};

    use super::*;

    fn alert(id: &str, target: Option<&str>, classifications: Vec<&str>) -> HealthProbeAlert {
        HealthProbeAlert {
            id: id.parse().unwrap(),
            target: target.map(str::to_string),
            in_alert_since: None,
            message: String::new(),
            tenant_message: None,
            classifications: classifications
                .into_iter()
                .map(|c| c.parse().unwrap())
                .collect(),
        }
    }

    fn report_with(alerts: Vec<HealthProbeAlert>) -> HealthReport {
        HealthReport {
            source: "test".to_string(),
            triggered_by: None,
            observed_at: None,
            successes: vec![],
            alerts,
        }
    }

    #[test]
    fn populate_from_aggregate_health() {
        let mut object = HealthObjectMetrics::default();
        let aggregate = report_with(vec![
            alert("BgpStats", None, vec!["Class1", "SuppressExternalAlerting"]),
            alert("FileExists", Some("abc"), vec!["Class2"]),
        ]);
        let sources = HealthReportSources {
            replace: Some(report_with(vec![])),
            merges: BTreeMap::from_iter([
                ("a".to_string(), report_with(vec![])),
                ("b".to_string(), report_with(vec![])),
            ]),
        };

        object.populate("obj-1".to_string(), &aggregate, &sources);

        assert_eq!(object.object_id, "obj-1");
        assert_eq!(object.health_probe_alerts.len(), 2);
        assert!(object.alerts_suppressed);
        assert_eq!(object.health_alert_classifications.len(), 3);
        assert_eq!(object.num_merge_overrides, 2);
        assert!(object.replace_override_enabled);
    }

    #[test]
    fn merge_no_dimension_aggregates_across_objects() {
        let mut iteration = HealthIterationMetrics::<()>::default();

        let mut healthy = HealthObjectMetrics::default();
        healthy.populate(
            "switch-a".to_string(),
            &report_with(vec![]),
            &HealthReportSources::default(),
        );
        let mut unhealthy_suppressed = HealthObjectMetrics::default();
        unhealthy_suppressed.populate(
            "switch-b".to_string(),
            &report_with(vec![alert(
                "BgpStats",
                None,
                vec!["Class1", "SuppressExternalAlerting"],
            )]),
            &HealthReportSources {
                replace: Some(report_with(vec![])),
                merges: BTreeMap::from_iter([("a".to_string(), report_with(vec![]))]),
            },
        );

        iteration.merge((), &healthy);
        iteration.merge((), &unhealthy_suppressed);

        assert_eq!(
            iteration.healthy,
            HashMap::from_iter([((true, ()), 1), ((false, ()), 1)])
        );
        assert_eq!(
            iteration.unhealthy_by_probe_id,
            HashMap::from_iter([(("BgpStats".to_string(), None, ()), 1)])
        );
        assert_eq!(
            iteration.unhealthy_by_classification_id,
            HashMap::from_iter([
                (("Class1".to_string(), ()), 1),
                (("SuppressExternalAlerting".to_string(), ()), 1),
            ])
        );
        assert_eq!(
            iteration.alerts_suppressed_by_object_id,
            HashSet::from_iter(["switch-b".to_string()])
        );
        assert_eq!(
            iteration.num_overrides,
            HashMap::from_iter([(("merge", ()), 1), (("replace", ()), 1)])
        );
    }
}
