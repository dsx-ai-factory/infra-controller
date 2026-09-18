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

use dashmap::DashMap;
use nv_redfish::resource::Health as BmcHealth;

use super::{CollectorEvent, EventContext, EventProcessor};
use crate::sink::{
    Classification, HealthReport, HealthReportAlert, HealthReportSuccess, MetricSample, Probe,
    ReportSource, SensorAttribution, SensorThresholdContext,
};

/// Sensor placement from the sample labels the sensor projection already set.
fn sensor_attribution(metric: &MetricSample) -> Option<SensorAttribution> {
    let label = |key: &str| {
        metric
            .labels
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    let attribution = SensorAttribution {
        powersupply_id: label("powersupply_id"),
        physical_context: label("physical_context"),
    };
    (attribution != SensorAttribution::default()).then_some(attribution)
}

#[derive(Debug, Clone, Copy)]
enum SensorHealth {
    Ok,
    Warning,
    Critical,
    Fatal,
    SensorFailure,
}

impl SensorHealth {
    fn to_classification(self) -> Classification {
        match self {
            Self::Ok => Classification::SensorOk,
            Self::Warning => Classification::SensorWarning,
            Self::Critical => Classification::SensorCritical,
            Self::Fatal => Classification::SensorFatal,
            Self::SensorFailure => Classification::SensorFailure,
        }
    }
}

enum SensorHealthResult {
    Success(HealthReportSuccess),
    Alert(HealthReportAlert),
}

#[derive(Default)]
struct HealthReportWindow {
    successes: Vec<HealthReportSuccess>,
    alerts: Vec<HealthReportAlert>,
}

#[derive(Default)]
pub struct HealthReportProcessor {
    windows: DashMap<String, HealthReportWindow>,
}

impl HealthReportProcessor {
    pub fn new() -> Self {
        Self {
            windows: DashMap::new(),
        }
    }

    fn stream_key(context: &EventContext) -> String {
        format!("{}::{}", context.endpoint_key(), context.collector_type)
    }

    fn fmt_range(low: Option<f64>, high: Option<f64>) -> String {
        match (low, high) {
            (None, None) => "not set".to_string(),
            (Some(l), Some(h)) => format!("{:.1} to {:.1}", l, h),
            (Some(l), None) => format!("min {:.1}", l),
            (None, Some(h)) => format!("max {:.1}", h),
        }
    }

    fn classify(health: &SensorThresholdContext, reading: f64) -> SensorHealth {
        if let Some(max) = health.range_max
            && reading > max
        {
            return SensorHealth::SensorFailure;
        }

        if let Some(min) = health.range_min
            && reading < min
        {
            return SensorHealth::SensorFailure;
        }

        if let Some(upper_fatal) = health.upper_fatal
            && reading >= upper_fatal
        {
            return SensorHealth::Fatal;
        }

        if let Some(lower_fatal) = health.lower_fatal
            && reading <= lower_fatal
        {
            return SensorHealth::Fatal;
        }

        if let Some(upper_critical) = health.upper_critical
            && reading >= upper_critical
        {
            return SensorHealth::Critical;
        }

        if let Some(lower_critical) = health.lower_critical
            && reading <= lower_critical
        {
            return SensorHealth::Critical;
        }

        if let Some(upper_caution) = health.upper_caution
            && reading >= upper_caution
        {
            return SensorHealth::Warning;
        }
        if let Some(lower_caution) = health.lower_caution
            && reading <= lower_caution
        {
            return SensorHealth::Warning;
        }

        SensorHealth::Ok
    }

    fn to_health_result(
        metric: &MetricSample,
        health: &SensorThresholdContext,
    ) -> SensorHealthResult {
        let classification = Self::classify(health, metric.value);
        let attribution = sensor_attribution(metric);

        match classification {
            SensorHealth::Ok => SensorHealthResult::Success(HealthReportSuccess {
                probe_id: Probe::Sensor,
                target: Some(health.sensor_id.clone()),
                attribution,
            }),
            state => {
                if health.bmc_health == BmcHealth::Ok {
                    tracing::warn!(
                        sensor_id = %health.sensor_id,
                        entity_type = %health.entity_type,
                        reading = metric.value,
                        unit = %metric.unit,
                        reading_type = %metric.metric_type,
                        valid_range = %Self::fmt_range(health.range_min, health.range_max),
                        caution_range = %Self::fmt_range(health.lower_caution, health.upper_caution),
                        critical_range = %Self::fmt_range(health.lower_critical, health.upper_critical),
                        calculated_status = ?state,
                        "Threshold check indicates issue but BMC reports sensor as OK - likely incorrect thresholds, reporting OK"
                    );
                    return SensorHealthResult::Success(HealthReportSuccess {
                        probe_id: Probe::Sensor,
                        target: Some(health.sensor_id.clone()),
                        attribution,
                    });
                }

                let status = match state {
                    SensorHealth::Warning => "Warning",
                    SensorHealth::Critical => "Critical",
                    SensorHealth::Fatal => "Fatal",
                    SensorHealth::SensorFailure => "Sensor Failure",
                    SensorHealth::Ok => "Ok",
                };

                let message = format!(
                    "{} '{}': {} - reading {:.2}{} ({}), valid range: {}, caution: {}, critical: {}, fatal: {}",
                    health.entity_type,
                    health.sensor_id,
                    status,
                    metric.value,
                    metric.unit,
                    metric.metric_type,
                    Self::fmt_range(health.range_min, health.range_max),
                    Self::fmt_range(health.lower_caution, health.upper_caution),
                    Self::fmt_range(health.lower_critical, health.upper_critical),
                    Self::fmt_range(health.lower_fatal, health.upper_fatal),
                );

                SensorHealthResult::Alert(HealthReportAlert {
                    probe_id: Probe::Sensor,
                    target: Some(health.sensor_id.clone()),
                    message,
                    classifications: vec![state.to_classification()],
                    attribution,
                })
            }
        }
    }
}

impl EventProcessor for HealthReportProcessor {
    fn processor_type(&self) -> &'static str {
        "health_report_processor"
    }

    fn process_event(&self, context: &EventContext, event: &CollectorEvent) -> Vec<CollectorEvent> {
        match event {
            CollectorEvent::MetricCollectionStart => {
                self.windows
                    .insert(Self::stream_key(context), HealthReportWindow::default());
            }
            CollectorEvent::Metric(metric) => {
                let Some(health) = metric.context.as_ref() else {
                    return Vec::new();
                };
                let mut window = self.windows.entry(Self::stream_key(context)).or_default();
                match Self::to_health_result(metric, health) {
                    SensorHealthResult::Success(success) => window.successes.push(success),
                    SensorHealthResult::Alert(alert) => window.alerts.push(alert),
                }
            }
            CollectorEvent::MetricCollectionEnd => {
                let Some((_, window)) = self.windows.remove(&Self::stream_key(context)) else {
                    return Vec::new();
                };
                let report = HealthReport {
                    source: ReportSource::BmcSensors,
                    target: context.health_report_target(),
                    observed_at: Some(chrono::Utc::now()),
                    successes: window.successes,
                    alerts: window.alerts,
                };

                tracing::info!(
                    endpoint = %context.addr.mac,
                    success_count = report.successes.len(),
                    alert_count = report.alerts.len(),
                    "Sending hardware health report"
                );

                return vec![CollectorEvent::HealthReport(Arc::new(report))];
            }
            CollectorEvent::CollectorRemoved => {
                self.windows.remove(&Self::stream_key(context));
            }
            CollectorEvent::Log(_)
            | CollectorEvent::Firmware(_)
            | CollectorEvent::HealthReport(_) => {}
        }

        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use carbide_test_support::{Check, check_values};
    use mac_address::MacAddress;
    use nv_redfish::resource::Health as BmcHealth;

    use super::*;
    use crate::endpoint::{BmcAddr, EndpointMetadata, MachineData, SharedSystemUuid};
    use crate::sink::HealthReportTarget;

    fn test_context() -> EventContext {
        EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac"),
            },
            collector_type: "sensor_collector",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: Some(
                    "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                        .parse()
                        .expect("valid machine id"),
                ),
                machine_serial: None,
                system_uuid: SharedSystemUuid::default(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: None,
                driver_version: None,
            })),
            rack_id: None,
        }
    }

    #[test]
    fn metric_window_emits_abstract_health_report() {
        let processor = HealthReportProcessor::new();
        let context = test_context();

        let _ = processor.process_event(&context, &CollectorEvent::MetricCollectionStart);
        let _ = processor.process_event(
            &context,
            &CollectorEvent::Metric(
                MetricSample {
                    key: "sensor-1".to_string(),
                    name: "hw_sensor".to_string(),
                    metric_type: "temperature".to_string(),
                    unit: "celsius".to_string(),
                    value: 42.0,
                    labels: vec![],
                    context: Some(SensorThresholdContext {
                        entity_type: "sensor".to_string(),
                        sensor_id: "Temp1".to_string(),
                        upper_fatal: None,
                        lower_fatal: None,
                        upper_critical: Some(30.0),
                        lower_critical: None,
                        upper_caution: None,
                        lower_caution: None,
                        range_max: None,
                        range_min: None,
                        bmc_health: BmcHealth::Critical,
                    }),
                }
                .into(),
            ),
        );
        let emitted = processor.process_event(&context, &CollectorEvent::MetricCollectionEnd);

        let Some(CollectorEvent::HealthReport(report)) = emitted.last() else {
            panic!("expected health report event");
        };

        assert_eq!(report.source, ReportSource::BmcSensors);
        assert_eq!(report.target, Some(HealthReportTarget::Machine));
        assert!(report.successes.is_empty());
        assert_eq!(report.alerts.len(), 1);
    }

    /// Runs one critical sensor sample through the processor and returns the
    /// attribution of the alert it produces.
    fn observe_alert_attribution(labels: Vec<(&str, &str)>) -> Option<SensorAttribution> {
        let processor = HealthReportProcessor::new();
        let context = test_context();
        let _ = processor.process_event(&context, &CollectorEvent::MetricCollectionStart);
        let _ = processor.process_event(
            &context,
            &CollectorEvent::Metric(
                MetricSample {
                    key: "sensor-1".to_string(),
                    name: "hw_sensor".to_string(),
                    metric_type: "power".to_string(),
                    unit: "watts".to_string(),
                    value: 5600.0,
                    labels: labels
                        .into_iter()
                        .map(|(key, value)| (Cow::Owned(key.to_string()), value.to_string()))
                        .collect(),
                    context: Some(SensorThresholdContext {
                        entity_type: "powersupply".to_string(),
                        sensor_id: "PSU0_Power".to_string(),
                        upper_fatal: None,
                        lower_fatal: None,
                        upper_critical: Some(5500.0),
                        lower_critical: None,
                        upper_caution: None,
                        lower_caution: None,
                        range_max: None,
                        range_min: None,
                        bmc_health: BmcHealth::Critical,
                    }),
                }
                .into(),
            ),
        );
        let emitted = processor.process_event(&context, &CollectorEvent::MetricCollectionEnd);
        let Some(CollectorEvent::HealthReport(report)) = emitted.last() else {
            panic!("expected health report event");
        };
        report.alerts[0].attribution.clone()
    }

    #[test]
    fn sensor_alert_attribution_cases() {
        check_values(
            [
                Check {
                    scenario: "power supply sensor carries its PSU and context",
                    input: vec![
                        ("powersupply_id", "PSU0"),
                        ("physical_context", "power_supply"),
                    ],
                    expect: Some(SensorAttribution {
                        powersupply_id: Some("PSU0".to_string()),
                        physical_context: Some("power_supply".to_string()),
                    }),
                },
                Check {
                    scenario: "sensor without placement labels carries none",
                    input: vec![("sensor_name", "PSU0_Power")],
                    expect: None,
                },
            ],
            observe_alert_attribution,
        );
    }

    #[test]
    fn collector_removed_clears_metric_window() {
        let processor = HealthReportProcessor::new();
        let context = test_context();

        let _ = processor.process_event(&context, &CollectorEvent::MetricCollectionStart);
        assert_eq!(processor.windows.len(), 1);

        let emitted = processor.process_event(&context, &CollectorEvent::CollectorRemoved);

        assert!(emitted.is_empty());
        assert!(processor.windows.is_empty());
    }
}
