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

use std::collections::HashMap;
use std::time::SystemTime;

use opentelemetry::logs::AnyValue;
use opentelemetry::{Key, KeyValue};
use opentelemetry_proto::transform::common::tonic::Attributes;
use serde::Serialize;

use super::collector_logs::ExportLogsServiceRequest;
use super::collector_metrics::ExportMetricsServiceRequest;
use super::common::KeyValue as OtlpKeyValue;
use super::logs::{LogRecord as OtlpLogRecord, ResourceLogs, ScopeLogs, SeverityNumber};
use super::metrics::{
    Gauge as OtlpGauge, Metric as OtlpMetric, NumberDataPoint, ResourceMetrics, ScopeMetrics,
    metric, number_data_point,
};
use super::resource::Resource;
use crate::endpoint::SwitchEndpointRole;
use crate::sink::{
    CollectorEvent, EventContext, HealthReport, HealthReportAlert, HealthReportSuccess,
    LogSeverity, MetricSample,
};

/// Maximum alerts serialized into the `health_report.alerts` attribute.
///
/// A fully degraded endpoint reports an alert per failing sensor, and an export
/// that exceeds the collector's receive limit fails as `ResourceExhausted`,
/// which the drain retries before dropping the whole batch. Truncating here
/// keeps one degraded endpoint from taking unrelated records down with it.
const MAX_SERIALIZED_ALERTS: usize = 64;

const HEALTH_REPORT_SCHEMA_VERSION: &str = "v1";
const HEALTH_REPORT_SCHEMA_VERSION_KEY: &str = "health_report.schema_version";
const HEALTH_REPORT_SOURCE_KEY: &str = "health_report.source";
const HEALTH_REPORT_TARGET_KEY: &str = "health_report.target";
const HEALTH_REPORT_OBSERVED_AT_KEY: &str = "health_report.observed_at";
const HEALTH_REPORT_SUCCESS_COUNT_KEY: &str = "health_report.success_count";
const HEALTH_REPORT_ALERT_COUNT_KEY: &str = "health_report.alert_count";
const HEALTH_REPORT_SUCCESSES_KEY: &str = "health_report.successes";
const HEALTH_REPORT_SUCCESS_PROBE_ID_KEY: &str = "probe_id";
const HEALTH_REPORT_SUCCESS_TARGET_KEY: &str = "target";

fn severity_number(severity: LogSeverity) -> i32 {
    match severity {
        LogSeverity::Unspecified => SeverityNumber::Unspecified as i32,
        LogSeverity::Trace => SeverityNumber::Trace as i32,
        LogSeverity::Debug => SeverityNumber::Debug as i32,
        LogSeverity::Info => SeverityNumber::Info as i32,
        LogSeverity::Warn => SeverityNumber::Warn as i32,
        LogSeverity::Error => SeverityNumber::Error as i32,
        LogSeverity::Fatal => SeverityNumber::Fatal as i32,
    }
}

fn resource_group_key(context: &EventContext) -> String {
    format!("{}|{}", context.endpoint_key, context.collector_type)
}

/// Lowers [`KeyValue`] attributes onto the wire.
///
/// [`Attributes`] is opentelemetry-proto's newtype over the OTLP attribute list;
/// it owns the [`KeyValue`] conversion, so this only unwraps it.
fn otlp_attributes(attributes: impl IntoIterator<Item = KeyValue>) -> Vec<OtlpKeyValue> {
    Attributes::from(attributes).0
}

fn resource_attributes(context: &EventContext) -> Vec<KeyValue> {
    let mut attrs = Vec::new();
    match context.switch_endpoint_role() {
        Some(SwitchEndpointRole::Host) => {
            attrs.push(KeyValue::new(
                "switch.endpoint",
                context.endpoint_key.clone(),
            ));
            attrs.push(KeyValue::new("switch.ip", context.addr.ip.to_string()));
        }
        _ => {
            attrs.push(KeyValue::new("bmc.endpoint", context.endpoint_key.clone()));
            attrs.push(KeyValue::new("bmc.ip", context.addr.ip.to_string()));
        }
    }
    attrs.push(KeyValue::new("collector.type", context.collector_type));
    if let Some(machine_id) = context.machine_id() {
        attrs.push(KeyValue::new("machine.id", machine_id.to_string()));
    }
    if let Some(system_uuid) = context.system_uuid() {
        attrs.push(KeyValue::new("system.uuid", system_uuid.to_string()));
    }
    if let Some(machine_serial) = context.machine_serial() {
        attrs.push(KeyValue::new("machine.serial", machine_serial.to_string()));
    }
    if let Some(driver_version) = context.driver_version() {
        attrs.push(KeyValue::new("driver.version", driver_version.to_string()));
    }
    if let Some(component_type) = context.component_type() {
        attrs.push(KeyValue::new("component.type", component_type.to_string()));
    }
    if let Some(power_shelf_id) = context.power_shelf_id() {
        attrs.push(KeyValue::new("power_shelf.id", power_shelf_id.to_string()));
    }
    if context.component_type() == Some("power_shelf")
        && let Some(serial) = context.serial_number()
    {
        attrs.push(KeyValue::new(
            "power_shelf.serial_number",
            serial.to_string(),
        ));
    }
    if let Some(switch_id) = context.switch_id() {
        attrs.push(KeyValue::new("switch.id", switch_id.to_string()));
    }
    if let Some(serial) = context.switch_serial() {
        attrs.push(KeyValue::new("switch.serial_number", serial.to_string()));
    }
    if let Some(role) = context.switch_endpoint_role() {
        let endpoint_role = match role {
            SwitchEndpointRole::Bmc => "bmc",
            SwitchEndpointRole::Host => "host",
        };
        attrs.push(KeyValue::new("switch.endpoint_role", endpoint_role));
    }
    if let Some(is_primary) = context.switch_is_primary() {
        attrs.push(KeyValue::new("switch.is_primary", is_primary));
    }
    if let Some(rack_id) = context.rack_id() {
        attrs.push(KeyValue::new("rack.id", rack_id.to_string()));
    }
    if let Some(slot) = context.slot_number() {
        attrs.push(KeyValue::new("machine.slot_number", i64::from(slot)));
    }
    if let Some(tray) = context.tray_index() {
        attrs.push(KeyValue::new("machine.tray_index", i64::from(tray)));
    }
    if let Some(domain) = context.nvlink_domain_uuid() {
        attrs.push(KeyValue::new("nvlink.domain.uuid", domain.to_string()));
    }
    if let Some(slot) = context.switch_slot_number() {
        attrs.push(KeyValue::new("switch.slot_number", i64::from(slot)));
    }
    if let Some(tray) = context.switch_tray_index() {
        attrs.push(KeyValue::new("switch.tray_index", i64::from(tray)));
    }
    attrs.extend(
        context
            .labels()
            .iter()
            .map(|(name, value)| KeyValue::new(name.to_owned(), value.clone())),
    );
    attrs
}

fn convert_log(log: &crate::sink::LogRecord, observed_nanos: u64) -> OtlpLogRecord {
    let attributes = log
        .attributes
        .iter()
        .map(|(k, v)| KeyValue::new(k.to_string(), v.clone()));

    OtlpLogRecord {
        time_unix_nano: observed_nanos,
        observed_time_unix_nano: observed_nanos,
        severity_number: severity_number(log.severity),
        severity_text: log.severity.as_str().to_string(),
        body: Some(AnyValue::from(log.body.clone()).into()),
        attributes: otlp_attributes(attributes),
        ..Default::default()
    }
}

/// One passing probe, as a nested kvlist under `health_report.successes`.
fn health_report_success_value(success: &HealthReportSuccess) -> AnyValue {
    let mut entries = HashMap::from([(
        Key::from_static_str(HEALTH_REPORT_SUCCESS_PROBE_ID_KEY),
        AnyValue::from(success.probe_id.as_str()),
    )]);
    if let Some(target) = &success.target {
        entries.insert(
            Key::from_static_str(HEALTH_REPORT_SUCCESS_TARGET_KEY),
            AnyValue::from(target.clone()),
        );
    }

    AnyValue::Map(Box::new(entries))
}

/// The versioned `health_report.*` attribute contract.
///
/// Scalar routing fields let a consumer filter without parsing the summary body;
/// `health_report.successes` carries the passing probes as nested kvlists.
fn health_report_attributes(report: &HealthReport) -> Vec<(&'static str, AnyValue)> {
    let successes = report
        .successes
        .iter()
        .map(health_report_success_value)
        .collect();

    let mut attributes = vec![
        ("event.type", AnyValue::from("health_report")),
        (
            HEALTH_REPORT_SCHEMA_VERSION_KEY,
            AnyValue::from(HEALTH_REPORT_SCHEMA_VERSION),
        ),
        (
            HEALTH_REPORT_SOURCE_KEY,
            AnyValue::from(report.source.as_str()),
        ),
        (
            HEALTH_REPORT_SUCCESS_COUNT_KEY,
            AnyValue::from(i64::try_from(report.successes.len()).unwrap_or(i64::MAX)),
        ),
        (
            HEALTH_REPORT_ALERT_COUNT_KEY,
            AnyValue::from(i64::try_from(report.alerts.len()).unwrap_or(i64::MAX)),
        ),
        (
            HEALTH_REPORT_SUCCESSES_KEY,
            AnyValue::ListAny(Box::new(successes)),
        ),
    ];
    if let Some(target) = report.target {
        attributes.push((HEALTH_REPORT_TARGET_KEY, AnyValue::from(target.as_str())));
    }
    if let Some(observed_at) = &report.observed_at {
        attributes.push((
            HEALTH_REPORT_OBSERVED_AT_KEY,
            AnyValue::from(observed_at.to_rfc3339_opts(chrono::SecondsFormat::Nanos, true)),
        ));
    }

    attributes
}

/// When the report was observed, falling back to the export time for reports
/// without an observation time or dated before the Unix epoch.
fn health_report_event_time(report: &HealthReport, fallback_nanos: u64) -> u64 {
    report
        .observed_at
        .as_ref()
        .and_then(chrono::DateTime::timestamp_nanos_opt)
        .and_then(|nanos| u64::try_from(nanos).ok())
        .unwrap_or(fallback_nanos)
}

/// Alert detail as it appears inside the `health_report.alerts` JSON array.
///
/// Probe and classification identities use the wire names the health API
/// already accepts, so `Probe::GpuInventory` reports as `SkuValidation`.
#[derive(Serialize)]
struct AlertDetail<'a> {
    probe_id: &'static str,

    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<&'a str>,

    message: &'a str,
    classifications: Vec<&'static str>,
}

/// Serializes alert detail, truncating to [`MAX_SERIALIZED_ALERTS`].
///
/// Returns no attributes when serialization fails so a malformed report costs
/// only the detail, not the record.
fn alert_detail_attributes(alerts: &[HealthReportAlert]) -> Vec<(&'static str, AnyValue)> {
    let details: Vec<AlertDetail<'_>> = alerts
        .iter()
        .take(MAX_SERIALIZED_ALERTS)
        .map(|alert| AlertDetail {
            probe_id: alert.probe_id.as_str(),
            target: alert.target.as_deref(),
            message: &alert.message,
            classifications: alert
                .classifications
                .iter()
                .map(|classification| classification.as_str())
                .collect(),
        })
        .collect();

    let json = match serde_json::to_string(&details) {
        Ok(json) => json,
        Err(error) => {
            tracing::warn!(
                ?error,
                alert_count = alerts.len(),
                "failed to serialize health report alert details"
            );

            return Vec::new();
        }
    };

    let mut attributes = vec![("health_report.alerts", AnyValue::from(json))];
    let dropped = alerts.len().saturating_sub(MAX_SERIALIZED_ALERTS);

    if dropped > 0 {
        attributes.push((
            "health_report.alerts.dropped",
            AnyValue::from(i64::try_from(dropped).unwrap_or(i64::MAX)),
        ));
    }

    attributes
}

fn convert_event(
    event: &CollectorEvent,
    observed_nanos: u64,
    include_alert_details: bool,
) -> Option<OtlpLogRecord> {
    match event {
        CollectorEvent::Log(log) => Some(convert_log(log, observed_nanos)),
        CollectorEvent::HealthReport(report) => {
            let body = format!(
                "health report: {} alerts, {} ok (source: {:?})",
                report.alerts.len(),
                report.successes.len(),
                report.source,
            );
            let severity = if report.alerts.is_empty() {
                LogSeverity::Info
            } else {
                LogSeverity::Warn
            };

            let mut attributes = health_report_attributes(report);

            if include_alert_details && !report.alerts.is_empty() {
                attributes.extend(alert_detail_attributes(&report.alerts));
            }

            Some(OtlpLogRecord {
                time_unix_nano: health_report_event_time(report, observed_nanos),
                observed_time_unix_nano: observed_nanos,
                severity_number: severity_number(severity),
                severity_text: severity.as_str().to_string(),
                body: Some(AnyValue::from(body).into()),
                attributes: attributes.into_iter().collect::<Attributes>().0,
                ..Default::default()
            })
        }
        CollectorEvent::Firmware(info) => {
            let body = format!("{}: {}", info.component, info.version);
            Some(OtlpLogRecord {
                time_unix_nano: observed_nanos,
                observed_time_unix_nano: observed_nanos,
                severity_number: severity_number(LogSeverity::Info),
                severity_text: LogSeverity::Info.as_str().to_string(),
                body: Some(AnyValue::from(body).into()),
                attributes: otlp_attributes([KeyValue::new("event.type", "firmware")]),
                ..Default::default()
            })
        }
        CollectorEvent::Metric(_)
        | CollectorEvent::MetricCollectionStart
        | CollectorEvent::MetricCollectionEnd
        | CollectorEvent::CollectorRemoved => None,
    }
}

/// Builds an OTLP log export request grouped by endpoint.
///
/// `include_alert_details` is the receiving target's policy, so one target can
/// carry per-alert detail while another receives only the report counts.
pub fn build_export_request(
    batch: &[(EventContext, CollectorEvent)],
    include_alert_details: bool,
) -> ExportLogsServiceRequest {
    let observed_nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    let mut by_endpoint: HashMap<String, (Vec<KeyValue>, Vec<OtlpLogRecord>)> = HashMap::new();

    for (context, event) in batch {
        let Some(record) = convert_event(event, observed_nanos, include_alert_details) else {
            continue;
        };
        by_endpoint
            .entry(resource_group_key(context))
            .or_insert_with(|| (resource_attributes(context), Vec::new()))
            .1
            .push(record);
    }

    let resource_logs = by_endpoint
        .into_values()
        .map(|(attrs, records)| ResourceLogs {
            resource: Some(Resource {
                attributes: otlp_attributes(attrs),
                ..Default::default()
            }),
            scope_logs: vec![ScopeLogs {
                scope: None,
                log_records: records,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        })
        .collect();

    ExportLogsServiceRequest { resource_logs }
}

/// Builds an OTLP metric export request grouped by endpoint.
///
/// Every sample maps to an OTLP `Gauge` point; Sum and Histogram mapping can
/// be added when the health metric model exposes those temporality choices.
pub fn build_metrics_export_request(
    batch: &[(EventContext, MetricSample)],
    metric_name_prefix: &str,
) -> ExportMetricsServiceRequest {
    let observed_nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as u64;

    let mut by_endpoint: HashMap<String, (Vec<KeyValue>, Vec<OtlpMetric>)> = HashMap::new();

    for (context, sample) in batch {
        // Switch identity rides once on the resource attributes (switch.id,
        // switch.serial_number, switch.ip). VictoriaMetrics flattens resource
        // attributes onto every series, so promoting them onto the datapoint too
        // only duplicates the same value under a second (underscore) label name.
        let attributes = sample
            .labels
            .iter()
            .map(|(k, v)| KeyValue::new(k.to_string(), v.clone()));

        let data_point = NumberDataPoint {
            attributes: otlp_attributes(attributes),
            time_unix_nano: observed_nanos,
            value: Some(number_data_point::Value::AsDouble(sample.value)),
            ..Default::default()
        };

        let otlp_metric = OtlpMetric {
            // match the Prometheus sink's full series name exactly so Grafana queries
            // resolve identically across both export paths.
            name: format!(
                "{}_{}_{}_{}",
                metric_name_prefix, sample.name, sample.metric_type, sample.unit
            ),
            description: String::new(),
            unit: sample.unit.clone(),
            data: Some(metric::Data::Gauge(OtlpGauge {
                data_points: vec![data_point],
            })),
            ..Default::default()
        };

        by_endpoint
            .entry(resource_group_key(context))
            .or_insert_with(|| (resource_attributes(context), Vec::new()))
            .1
            .push(otlp_metric);
    }

    let resource_metrics = by_endpoint
        .into_values()
        .map(|(attrs, metrics)| ResourceMetrics {
            resource: Some(Resource {
                attributes: otlp_attributes(attrs),
                ..Default::default()
            }),
            scope_metrics: vec![ScopeMetrics {
                scope: None,
                metrics,
                schema_url: String::new(),
            }],
            schema_url: String::new(),
        })
        .collect();

    ExportMetricsServiceRequest { resource_metrics }
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;

    use carbide_test_support::value_scenarios;
    use carbide_uuid::nvlink::NvLinkDomainId;
    use carbide_uuid::power_shelf::PowerShelfId;
    use carbide_uuid::rack::RackId;
    use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
    use chrono::{TimeZone, Utc};
    use mac_address::MacAddress;

    use super::*;
    use crate::endpoint::{
        BmcAddr, EndpointMetadata, MachineData, PowerShelfData, SharedSystemUuid, SwitchData,
        SwitchEndpointRole,
    };
    use crate::otlp::common::{AnyValue as OtlpAnyValue, any_value};
    use crate::sink::{
        Classification, HealthReport, HealthReportAlert, HealthReportSuccess, HealthReportTarget,
        LogRecord, Probe, ReportSource,
    };

    fn test_context() -> EventContext {
        EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac"),
            },
            collector_type: "test",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        }
    }

    fn test_switch_id(label: &str) -> SwitchId {
        let mut hash = [0u8; 32];
        let bytes = label.as_bytes();
        hash[..bytes.len().min(32)].copy_from_slice(&bytes[..bytes.len().min(32)]);
        SwitchId::new(SwitchIdSource::Tpm, hash, SwitchType::NvLink)
    }

    /// Resource attributes as an export carries them, so assertions read the
    /// same wire form the collector receives.
    fn otlp_resource_attributes(context: &EventContext) -> Vec<OtlpKeyValue> {
        otlp_attributes(resource_attributes(context))
    }

    fn attr_value<'a>(attrs: &'a [OtlpKeyValue], key: &str) -> Option<&'a str> {
        attrs
            .iter()
            .find(|attr| attr.key == key)
            .and_then(|attr| attr.value.as_ref())
            .and_then(|value| match value.value.as_ref()? {
                any_value::Value::StringValue(value) => Some(value.as_str()),
                _ => None,
            })
    }

    fn attr_int_value(attrs: &[OtlpKeyValue], key: &str) -> Option<i64> {
        attrs
            .iter()
            .find(|attr| attr.key == key)
            .and_then(|attr| attr.value.as_ref())
            .and_then(|value| match value.value.as_ref()? {
                any_value::Value::IntValue(value) => Some(*value),
                _ => None,
            })
    }

    fn attr_bool_value(attrs: &[OtlpKeyValue], key: &str) -> Option<bool> {
        attrs
            .iter()
            .find(|attr| attr.key == key)
            .and_then(|attr| attr.value.as_ref())
            .and_then(|value| match value.value.as_ref()? {
                any_value::Value::BoolValue(value) => Some(*value),
                _ => None,
            })
    }

    /// The record's body when it is a string, the only body shape the health
    /// crate emits.
    fn body_value(record: &OtlpLogRecord) -> Option<&str> {
        match record.body.as_ref()?.value.as_ref()? {
            any_value::Value::StringValue(value) => Some(value.as_str()),
            _ => None,
        }
    }

    fn any_array_value(value: &OtlpAnyValue) -> Option<&[OtlpAnyValue]> {
        match value.value.as_ref()? {
            any_value::Value::ArrayValue(value) => Some(value.values.as_slice()),
            _ => None,
        }
    }

    fn any_kvlist_value(value: &OtlpAnyValue) -> Option<&[OtlpKeyValue]> {
        match value.value.as_ref()? {
            any_value::Value::KvlistValue(value) => Some(value.values.as_slice()),
            _ => None,
        }
    }

    fn attr_array_value<'a>(attrs: &'a [OtlpKeyValue], key: &str) -> Option<&'a [OtlpAnyValue]> {
        attrs
            .iter()
            .find(|attr| attr.key == key)
            .and_then(|attr| attr.value.as_ref())
            .and_then(any_array_value)
    }

    #[test]
    fn resource_attributes_include_machine_metadata_when_present() {
        let domain_uuid = NvLinkDomainId::nil();
        let context = EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac"),
            },
            collector_type: "test",
            labels: std::collections::BTreeMap::from([(
                "site".to_string(),
                "rno-dev7".to_string(),
            )]),
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: Some(
                    "fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0"
                        .parse()
                        .expect("valid machine id"),
                ),
                machine_serial: Some("MN-001".to_string()),
                system_uuid: Some(uuid::uuid!("4c4c4544-0044-4710-8052-cac04f4b4632")).into(),
                slot_number: Some(15),
                tray_index: Some(5),
                nvlink_domain_uuid: Some(domain_uuid),
                driver_version: Some("570.82".to_string()),
            })),
            rack_id: Some(RackId::new("RACK_1")),
        };

        let attrs = otlp_resource_attributes(&context);

        assert_eq!(attr_value(&attrs, "rack.id"), Some("RACK_1"));
        assert_eq!(attr_value(&attrs, "site"), Some("rno-dev7"));
        assert_eq!(
            attr_value(&attrs, "system.uuid"),
            Some("4c4c4544-0044-4710-8052-cac04f4b4632")
        );
        assert_eq!(attr_value(&attrs, "machine.serial"), Some("MN-001"));
        assert_eq!(attr_value(&attrs, "driver.version"), Some("570.82"));
        assert_eq!(attr_value(&attrs, "component.type"), Some("compute_node"));
        assert_eq!(attr_int_value(&attrs, "machine.slot_number"), Some(15));
        assert_eq!(attr_int_value(&attrs, "machine.tray_index"), Some(5));
        assert_eq!(
            attr_value(&attrs, "nvlink.domain.uuid"),
            Some("00000000-0000-0000-0000-000000000000")
        );
    }

    /// Verifies that absent optional machine metadata does not emit empty resource attributes.
    #[test]
    fn resource_attributes_omit_absent_optional_machine_metadata() {
        let context = EventContext {
            endpoint_key: "42:9e:b1:bd:9d:dd".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac"),
            },
            collector_type: "test",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Machine(MachineData {
                machine_id: None,
                machine_serial: None,
                system_uuid: SharedSystemUuid::default(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: None,
                driver_version: None,
            })),
            rack_id: None,
        };

        let attrs = otlp_resource_attributes(&context);

        assert_eq!(attr_value(&attrs, "machine.id"), None);
        assert_eq!(attr_value(&attrs, "component.type"), Some("compute_node"));
        assert_eq!(attr_value(&attrs, "machine.serial"), None);
        assert_eq!(attr_value(&attrs, "system.uuid"), None);
        assert_eq!(attr_value(&attrs, "driver.version"), None);
        assert_eq!(attr_value(&attrs, "nvlink.domain.uuid"), None);
    }

    #[test]
    fn resource_attributes_include_switch_placement_metadata_when_present() {
        let switch_id = test_switch_id("switch-a");
        let switch_id_attr = switch_id.to_string();
        let nvlink_domain_uuid = NvLinkDomainId::new();
        let nvlink_domain_uuid_attr = nvlink_domain_uuid.to_string();

        let context = EventContext {
            endpoint_key: "11:22:33:44:55:66".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)),
                port: Some(443),
                mac: MacAddress::from_str("11:22:33:44:55:66").expect("valid mac"),
            },
            collector_type: "test",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Switch(SwitchData {
                id: Some(switch_id),
                serial: "SN-SWITCH-001".to_string(),
                slot_number: Some(7),
                tray_index: Some(3),
                nvlink_domain_uuid: Some(nvlink_domain_uuid),
                endpoint_role: SwitchEndpointRole::Host,
                is_primary: false,
                nmxc_enabled: false,
                nmxt_enabled: false,
            })),
            rack_id: Some(RackId::new("RACK_2")),
        };

        let attrs = otlp_resource_attributes(&context);

        assert_eq!(
            attr_value(&attrs, "switch.id"),
            Some(switch_id_attr.as_str())
        );
        assert_eq!(attr_value(&attrs, "rack.id"), Some("RACK_2"));
        assert_eq!(attr_value(&attrs, "component.type"), Some("nvlink_switch"));
        assert_eq!(attr_int_value(&attrs, "switch.slot_number"), Some(7));
        assert_eq!(attr_int_value(&attrs, "switch.tray_index"), Some(3));

        assert_eq!(
            attr_value(&attrs, "nvlink.domain.uuid"),
            Some(nvlink_domain_uuid_attr.as_str())
        );
    }

    #[test]
    fn switch_host_resource_uses_switch_endpoint_identity() {
        let switch_id = test_switch_id("switch-host");
        let switch_id_attr = switch_id.to_string();
        let context = EventContext {
            endpoint_key: "11:22:33:44:55:66".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)),
                port: Some(443),
                mac: MacAddress::from_str("11:22:33:44:55:66").expect("valid mac"),
            },
            collector_type: "nvue_gnmi",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Switch(SwitchData {
                id: Some(switch_id),
                serial: "SN-SWITCH-001".to_string(),
                slot_number: Some(7),
                tray_index: Some(3),
                nvlink_domain_uuid: None,
                endpoint_role: SwitchEndpointRole::Host,
                is_primary: true,
                nmxc_enabled: true,
                nmxt_enabled: true,
            })),
            rack_id: Some(RackId::new("RACK_2")),
        };

        let attrs = otlp_resource_attributes(&context);

        assert_eq!(attr_value(&attrs, "bmc.endpoint"), None);
        assert_eq!(attr_value(&attrs, "bmc.ip"), None);
        assert_eq!(
            attr_value(&attrs, "switch.endpoint"),
            Some("11:22:33:44:55:66")
        );
        assert_eq!(attr_value(&attrs, "switch.ip"), Some("10.0.1.1"));
        assert_eq!(
            attr_value(&attrs, "switch.id"),
            Some(switch_id_attr.as_str())
        );
        assert_eq!(
            attr_value(&attrs, "switch.serial_number"),
            Some("SN-SWITCH-001")
        );
        assert_eq!(attr_value(&attrs, "switch.endpoint_role"), Some("host"));
        assert_eq!(attr_bool_value(&attrs, "switch.is_primary"), Some(true));
        assert_eq!(attr_int_value(&attrs, "switch.slot_number"), Some(7));
        assert_eq!(attr_int_value(&attrs, "switch.tray_index"), Some(3));
        assert_eq!(attr_value(&attrs, "nvlink.domain.uuid"), None);
        assert_eq!(attr_value(&attrs, "rack.id"), Some("RACK_2"));
        assert_eq!(attr_value(&attrs, "collector.type"), Some("nvue_gnmi"));
        assert_eq!(attr_value(&attrs, "component.type"), Some("nvlink_switch"));
    }

    #[test]
    fn switch_bmc_log_resource_keeps_endpoint_identity_and_switch_metadata() {
        let switch_id = test_switch_id("switch-bmc");
        let switch_id_attr = switch_id.to_string();
        let nvlink_domain_uuid = NvLinkDomainId::new();
        let nvlink_domain_uuid_attr = nvlink_domain_uuid.to_string();
        let context = EventContext {
            endpoint_key: "22:33:44:55:66:77".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 2, 1)),
                port: Some(443),
                mac: MacAddress::from_str("22:33:44:55:66:77").expect("valid mac"),
            },
            collector_type: "logs_collector",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Switch(SwitchData {
                id: Some(switch_id),
                serial: "SN-SWITCH-BMC-001".to_string(),
                slot_number: Some(8),
                tray_index: Some(4),
                nvlink_domain_uuid: Some(nvlink_domain_uuid),
                endpoint_role: SwitchEndpointRole::Bmc,
                is_primary: false,
                nmxc_enabled: false,
                nmxt_enabled: false,
            })),
            rack_id: Some(RackId::new("RACK_3")),
        };
        let event = CollectorEvent::Log(Box::new(LogRecord {
            body: "switch BMC event".to_string(),
            severity: LogSeverity::Info,
            attributes: Vec::new(),
            diagnostic_record: None,
        }));

        let request = build_export_request(&[(context, event)], false);
        let attrs = &request.resource_logs[0]
            .resource
            .as_ref()
            .expect("log resource metadata")
            .attributes;

        assert_eq!(attr_value(attrs, "bmc.endpoint"), Some("22:33:44:55:66:77"));
        assert_eq!(attr_value(attrs, "bmc.ip"), Some("10.0.2.1"));
        assert_eq!(attr_value(attrs, "switch.endpoint"), None);
        assert_eq!(attr_value(attrs, "switch.ip"), None);
        assert_eq!(
            attr_value(attrs, "switch.id"),
            Some(switch_id_attr.as_str())
        );
        assert_eq!(
            attr_value(attrs, "switch.serial_number"),
            Some("SN-SWITCH-BMC-001")
        );
        assert_eq!(attr_value(attrs, "switch.endpoint_role"), Some("bmc"));
        assert_eq!(attr_bool_value(attrs, "switch.is_primary"), Some(false));
        assert_eq!(attr_int_value(attrs, "switch.slot_number"), Some(8));
        assert_eq!(attr_int_value(attrs, "switch.tray_index"), Some(4));
        assert_eq!(attr_value(attrs, "rack.id"), Some("RACK_3"));
        assert_eq!(attr_value(attrs, "collector.type"), Some("logs_collector"));
        assert_eq!(
            attr_value(attrs, "nvlink.domain.uuid"),
            Some(nvlink_domain_uuid_attr.as_str())
        );
        assert_eq!(attr_value(attrs, "component.type"), Some("nvlink_switch"));
    }

    #[test]
    fn switch_bmc_log_resource_omits_unavailable_optional_metadata() {
        let context = EventContext {
            endpoint_key: "33:44:55:66:77:88".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 2, 2)),
                port: Some(443),
                mac: MacAddress::from_str("33:44:55:66:77:88").expect("valid mac"),
            },
            collector_type: "logs_collector",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Switch(SwitchData {
                id: None,
                serial: "SN-SWITCH-BMC-002".to_string(),
                slot_number: None,
                tray_index: None,
                nvlink_domain_uuid: None,
                endpoint_role: SwitchEndpointRole::Bmc,
                is_primary: true,
                nmxc_enabled: false,
                nmxt_enabled: false,
            })),
            rack_id: None,
        };
        let event = CollectorEvent::Log(Box::new(LogRecord {
            body: "switch BMC event".to_string(),
            severity: LogSeverity::Info,
            attributes: Vec::new(),
            diagnostic_record: None,
        }));

        let request = build_export_request(&[(context, event)], false);
        let attrs = &request.resource_logs[0]
            .resource
            .as_ref()
            .expect("log resource metadata")
            .attributes;

        assert_eq!(attr_value(attrs, "bmc.endpoint"), Some("33:44:55:66:77:88"));
        assert_eq!(attr_value(attrs, "bmc.ip"), Some("10.0.2.2"));
        assert_eq!(
            attr_value(attrs, "switch.serial_number"),
            Some("SN-SWITCH-BMC-002")
        );
        assert_eq!(attr_value(attrs, "switch.endpoint_role"), Some("bmc"));
        assert_eq!(attr_bool_value(attrs, "switch.is_primary"), Some(true));
        assert_eq!(attr_value(attrs, "component.type"), Some("nvlink_switch"));
        assert_eq!(attr_value(attrs, "switch.id"), None);
        assert_eq!(attr_int_value(attrs, "switch.slot_number"), None);
        assert_eq!(attr_int_value(attrs, "switch.tray_index"), None);
        assert_eq!(attr_value(attrs, "nvlink.domain.uuid"), None);
        assert_eq!(attr_value(attrs, "rack.id"), None);
    }

    #[test]
    fn resource_attributes_include_power_shelf_identity() {
        let power_shelf_id =
            PowerShelfId::from_str("ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg")
                .expect("valid power shelf id");
        let power_shelf_id_string = power_shelf_id.to_string();
        let context = EventContext {
            endpoint_key: "33:44:55:66:77:88".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 3, 1)),
                port: Some(443),
                mac: MacAddress::from_str("33:44:55:66:77:88").expect("valid mac"),
            },
            collector_type: "sensor_collector",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::PowerShelf(PowerShelfData {
                id: Some(power_shelf_id),
                serial: Some("SN-PS-001".to_string()),
            })),
            rack_id: Some(RackId::new("RACK_4")),
        };

        let attrs = otlp_resource_attributes(&context);

        assert_eq!(attr_value(&attrs, "component.type"), Some("power_shelf"));
        assert_eq!(
            attr_value(&attrs, "power_shelf.id"),
            Some(power_shelf_id_string.as_str())
        );
        assert_eq!(
            attr_value(&attrs, "power_shelf.serial_number"),
            Some("SN-PS-001")
        );
        assert_eq!(attr_value(&attrs, "rack.id"), Some("RACK_4"));
    }

    #[test]
    fn resource_attributes_omit_missing_power_shelf_serial() {
        let context = EventContext {
            endpoint_key: "33:44:55:66:77:88".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 3, 1)),
                port: Some(443),
                mac: MacAddress::from_str("33:44:55:66:77:88").expect("valid mac"),
            },
            collector_type: "sensor_collector",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::PowerShelf(PowerShelfData {
                id: Some(
                    PowerShelfId::from_str(
                        "ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg",
                    )
                    .expect("valid power shelf id"),
                ),
                serial: None,
            })),
            rack_id: Some(RackId::new("RACK_4")),
        };

        let attrs = otlp_resource_attributes(&context);

        assert_eq!(attr_value(&attrs, "power_shelf.serial_number"), None);
    }

    #[test]
    fn log_event_converts_to_otlp_record() {
        let ctx = test_context();
        let log = CollectorEvent::Log(Box::new(LogRecord {
            body: "something happened".to_string(),
            severity: LogSeverity::Warn,
            attributes: vec![(Cow::Borrowed("entry_id"), "42".to_string())],
            diagnostic_record: None,
        }));

        let request = build_export_request(&[(ctx, log)], false);
        assert_eq!(request.resource_logs.len(), 1);

        let records = &request.resource_logs[0].scope_logs[0].log_records;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].severity_text, "WARN");
        assert_eq!(records[0].severity_number, SeverityNumber::Warn as i32);
    }

    #[test]
    fn unspecified_log_severity_remains_unspecified_in_otlp() {
        let log = CollectorEvent::Log(Box::new(LogRecord {
            body: "severity unavailable".to_string(),
            severity: LogSeverity::Unspecified,
            attributes: Vec::new(),
            diagnostic_record: None,
        }));

        let request = build_export_request(&[(test_context(), log)], false);
        let record = &request.resource_logs[0].scope_logs[0].log_records[0];

        assert_eq!(record.severity_text, "UNSPECIFIED");
        assert_eq!(record.severity_number, SeverityNumber::Unspecified as i32);
    }

    #[test]
    fn empty_log_body_is_exported_as_empty_string() {
        let log = CollectorEvent::Log(Box::new(LogRecord {
            body: String::new(),
            severity: LogSeverity::Fatal,
            attributes: Vec::new(),
            diagnostic_record: None,
        }));

        let request = build_export_request(&[(test_context(), log)], false);
        let record = &request.resource_logs[0].scope_logs[0].log_records[0];

        assert_eq!(record.severity_text, "FATAL");
        assert_eq!(record.severity_number, SeverityNumber::Fatal as i32);
        assert_eq!(
            record.body.as_ref().and_then(|body| body.value.as_ref()),
            Some(&any_value::Value::StringValue(String::new()))
        );
    }

    /// Verifies OTLP conversion preserves an already-emitted diagnostic log.
    #[test]
    fn diagnostic_log_event_preserves_diagnostic_body_and_attributes() {
        let ctx = test_context();
        let body = concat!(
            r#"{"message":"parent message","#,
            r#""diagnostic_data":"opaque-base64-payload","#,
            r#""diagnostic_attributes":["#,
            r#"{"key":"redfish.diagnostic_data.type","value":"cper"},"#,
            r#"{"key":"redfish.parent.log_entry_id","value":"42"}]}"#
        );

        let log = CollectorEvent::Log(Box::new(LogRecord {
            body: body.to_string(),
            severity: LogSeverity::Warn,
            attributes: vec![
                (
                    Cow::Borrowed("redfish.diagnostic_data.type"),
                    "cper".to_string(),
                ),
                (
                    Cow::Borrowed("redfish.parent.log_entry_id"),
                    "42".to_string(),
                ),
            ],
            diagnostic_record: None,
        }));

        let request = build_export_request(&[(ctx, log)], false);

        let records = &request.resource_logs[0].scope_logs[0].log_records;
        let record = &records[0];

        assert_eq!(body_value(record), Some(body));
        assert_eq!(
            attr_value(&record.attributes, "redfish.diagnostic_data.type"),
            Some("cper")
        );
        assert_eq!(
            attr_value(&record.attributes, "redfish.parent.log_entry_id"),
            Some("42")
        );
    }

    #[test]
    fn metric_events_are_filtered_out() {
        let ctx = test_context();
        let batch = vec![
            (ctx.clone(), CollectorEvent::MetricCollectionStart),
            (ctx, CollectorEvent::MetricCollectionEnd),
        ];
        let request = build_export_request(&batch, false);
        assert!(request.resource_logs.is_empty());
    }

    #[test]
    fn health_report_attribute_keys_are_stable() {
        assert_eq!(
            [
                HEALTH_REPORT_SCHEMA_VERSION_KEY,
                HEALTH_REPORT_SOURCE_KEY,
                HEALTH_REPORT_TARGET_KEY,
                HEALTH_REPORT_OBSERVED_AT_KEY,
                HEALTH_REPORT_SUCCESS_COUNT_KEY,
                HEALTH_REPORT_ALERT_COUNT_KEY,
                HEALTH_REPORT_SUCCESSES_KEY,
                HEALTH_REPORT_SUCCESS_PROBE_ID_KEY,
                HEALTH_REPORT_SUCCESS_TARGET_KEY,
            ],
            [
                "health_report.schema_version",
                "health_report.source",
                "health_report.target",
                "health_report.observed_at",
                "health_report.success_count",
                "health_report.alert_count",
                "health_report.successes",
                "probe_id",
                "target",
            ]
        );
    }

    fn sensor_alert() -> HealthReportAlert {
        HealthReportAlert {
            probe_id: Probe::Sensor,
            target: Some("Temp1".to_string()),
            message: "critical".to_string(),
            classifications: vec![Classification::SensorCritical],
        }
    }

    fn health_report_record(
        alerts: Vec<HealthReportAlert>,
        include_alert_details: bool,
    ) -> OtlpLogRecord {
        let report = CollectorEvent::HealthReport(
            HealthReport {
                source: ReportSource::BmcSensors,
                target: None,
                observed_at: None,
                successes: vec![],
                alerts,
            }
            .into(),
        );

        let request = build_export_request(&[(test_context(), report)], include_alert_details);

        request.resource_logs[0].scope_logs[0].log_records[0].clone()
    }

    fn alert_details(record: &OtlpLogRecord) -> Vec<serde_json::Value> {
        let json = attr_value(&record.attributes, "health_report.alerts")
            .expect("alert details attribute");

        serde_json::from_str::<serde_json::Value>(json)
            .expect("alert details parse as JSON")
            .as_array()
            .expect("alert details are a JSON array")
            .clone()
    }

    #[test]
    fn health_report_converts_with_alert_severity() {
        let record = health_report_record(vec![sensor_alert()], false);

        assert_eq!(record.severity_text, "WARN");
    }

    /// Guards the per-target policy: scalar evidence remains available while
    /// free-form alert detail stays absent when the flag is disabled.
    #[test]
    fn health_report_omits_alert_details_when_disabled() {
        let record = health_report_record(vec![sensor_alert()], false);

        assert_eq!(
            attr_value(&record.attributes, "event.type"),
            Some("health_report")
        );
        assert_eq!(
            attr_value(&record.attributes, HEALTH_REPORT_SCHEMA_VERSION_KEY),
            Some("v1")
        );
        assert_eq!(
            attr_int_value(&record.attributes, HEALTH_REPORT_ALERT_COUNT_KEY),
            Some(1)
        );
        assert_eq!(attr_value(&record.attributes, "health_report.alerts"), None);
        assert_eq!(
            attr_int_value(&record.attributes, "health_report.alerts.dropped"),
            None
        );
    }

    #[test]
    fn health_report_serializes_alert_details_when_enabled() {
        let alerts = vec![
            HealthReportAlert {
                probe_id: Probe::LeakDetection,
                target: Some(
                    "/redfish/v1/Chassis/BMC_0/ThermalSubsystem/LeakDetection/LeakDetectors/1"
                        .to_string(),
                ),
                message: "Leak detected: 2 detector alerts reached threshold 1".to_string(),
                classifications: vec![Classification::Leak, Classification::PreventAllocations],
            },
            sensor_alert(),
        ];

        let record = health_report_record(alerts, true);

        // The body, severity, and event.type stay as they are without the flag.
        assert_eq!(record.severity_text, "WARN");
        assert_eq!(record.severity_number, SeverityNumber::Warn as i32);
        assert_eq!(
            body_value(&record),
            Some("health report: 2 alerts, 0 ok (source: BmcSensors)")
        );
        assert_eq!(
            attr_value(&record.attributes, "event.type"),
            Some("health_report")
        );

        let details = alert_details(&record);

        assert_eq!(details.len(), 2);
        assert_eq!(details[0]["probe_id"], "BmcLeakDetection");
        assert_eq!(
            details[0]["target"],
            "/redfish/v1/Chassis/BMC_0/ThermalSubsystem/LeakDetection/LeakDetectors/1"
        );
        assert_eq!(
            details[0]["message"],
            "Leak detected: 2 detector alerts reached threshold 1"
        );
        assert_eq!(
            details[0]["classifications"],
            serde_json::json!(["Leak", "PreventAllocations"])
        );

        assert_eq!(details[1]["probe_id"], "BmcSensor");
        assert_eq!(details[1]["target"], "Temp1");
        assert_eq!(details[1]["message"], "critical");
        assert_eq!(
            details[1]["classifications"],
            serde_json::json!(["SensorCritical"])
        );

        assert_eq!(
            attr_int_value(&record.attributes, "health_report.alerts.dropped"),
            None
        );
    }

    #[test]
    fn health_report_without_alerts_omits_alert_details() {
        let record = health_report_record(vec![], true);

        assert_eq!(attr_value(&record.attributes, "health_report.alerts"), None);
        assert_eq!(
            attr_int_value(&record.attributes, HEALTH_REPORT_ALERT_COUNT_KEY),
            Some(0)
        );
    }

    #[test]
    fn health_report_alert_details_omit_absent_target() {
        let alert = HealthReportAlert {
            target: None,
            ..sensor_alert()
        };

        let record = health_report_record(vec![alert], true);
        let details = alert_details(&record);

        assert_eq!(details.len(), 1);
        assert!(details[0].get("target").is_none());
        assert_eq!(details[0]["probe_id"], "BmcSensor");
    }

    /// A fully degraded endpoint reports an alert per sensor; the attribute is
    /// capped so one endpoint cannot push the export past the receive limit.
    #[test]
    fn health_report_alert_details_are_capped() {
        let alerts = (0..MAX_SERIALIZED_ALERTS + 3)
            .map(|index| HealthReportAlert {
                target: Some(format!("Temp{index}")),
                ..sensor_alert()
            })
            .collect();

        let record = health_report_record(alerts, true);
        let details = alert_details(&record);

        assert_eq!(details.len(), MAX_SERIALIZED_ALERTS);
        assert_eq!(details[0]["target"], "Temp0");
        assert_eq!(
            attr_int_value(&record.attributes, "health_report.alerts.dropped"),
            Some(3)
        );
    }

    #[test]
    fn health_report_converts_with_structured_evidence() {
        let ctx = test_context();
        let observed_at = Utc
            .with_ymd_and_hms(2026, 7, 31, 12, 34, 56)
            .single()
            .expect("valid timestamp");
        let report = CollectorEvent::HealthReport(
            HealthReport {
                source: ReportSource::NvueLeakage,
                target: Some(HealthReportTarget::Switch),
                observed_at: Some(observed_at),
                successes: vec![HealthReportSuccess {
                    probe_id: Probe::NvueLeakage,
                    target: Some("LEAK1".to_string()),
                }],
                alerts: vec![HealthReportAlert {
                    probe_id: Probe::NvueLeakage,
                    target: Some("LEAK2".to_string()),
                    message: "NVUE leakage sensor LEAK2 reports leak".to_string(),
                    classifications: vec![Classification::Leak, Classification::SensorFailure],
                }],
            }
            .into(),
        );

        let request = build_export_request(&[(ctx, report)], true);
        let records = &request.resource_logs[0].scope_logs[0].log_records;
        let record = &records[0];
        let attrs = record.attributes.as_slice();

        assert_eq!(record.severity_text, "WARN");
        assert_eq!(
            body_value(record),
            Some("health report: 1 alerts, 1 ok (source: NvueLeakage)")
        );
        assert_eq!(attr_value(attrs, "event.type"), Some("health_report"));
        assert_eq!(
            record.time_unix_nano,
            u64::try_from(
                observed_at
                    .timestamp_nanos_opt()
                    .expect("timestamp has nanoseconds")
            )
            .expect("timestamp is after the Unix epoch")
        );
        assert_eq!(
            attr_value(attrs, HEALTH_REPORT_SCHEMA_VERSION_KEY),
            Some("v1")
        );
        assert_eq!(
            attr_value(attrs, HEALTH_REPORT_SOURCE_KEY),
            Some("nvue-leakage")
        );
        assert_eq!(attr_value(attrs, HEALTH_REPORT_TARGET_KEY), Some("switch"));
        assert_eq!(
            attr_value(attrs, HEALTH_REPORT_OBSERVED_AT_KEY),
            Some("2026-07-31T12:34:56.000000000Z")
        );
        assert_eq!(
            attr_int_value(attrs, HEALTH_REPORT_SUCCESS_COUNT_KEY),
            Some(1)
        );
        assert_eq!(
            attr_int_value(attrs, HEALTH_REPORT_ALERT_COUNT_KEY),
            Some(1)
        );

        let successes =
            attr_array_value(attrs, HEALTH_REPORT_SUCCESSES_KEY).expect("success array");
        let success = any_kvlist_value(&successes[0]).expect("structured success");
        assert_eq!(
            attr_value(success, HEALTH_REPORT_SUCCESS_PROBE_ID_KEY),
            Some("NvueLeakage")
        );
        assert_eq!(
            attr_value(success, HEALTH_REPORT_SUCCESS_TARGET_KEY),
            Some("LEAK1")
        );

        let alerts = alert_details(record);
        let alert = &alerts[0];
        assert_eq!(alert["probe_id"], "NvueLeakage");
        assert_eq!(alert["target"], "LEAK2");
        assert_eq!(alert["message"], "NVUE leakage sensor LEAK2 reports leak");
        assert_eq!(
            alert["classifications"],
            serde_json::json!(["Leak", "SensorFailure"])
        );
        assert_eq!(attr_int_value(attrs, "health_report.alerts.dropped"), None);
    }

    /// Export timestamp handed to `convert_event` so timestamp expectations stay
    /// independent of the wall clock.
    const EXPORT_NANOS: u64 = 1_784_500_000_000_000_000;

    #[derive(Debug, PartialEq)]
    struct RecordTimestamps {
        time_unix_nano: u64,
        observed_time_unix_nano: u64,
    }

    fn health_report_timestamps(observed_at: Option<chrono::DateTime<Utc>>) -> RecordTimestamps {
        let event = CollectorEvent::HealthReport(
            HealthReport {
                source: ReportSource::BmcSensors,
                target: Some(HealthReportTarget::Machine),
                observed_at,
                successes: vec![HealthReportSuccess {
                    probe_id: Probe::Sensor,
                    target: Some("Temp1".to_string()),
                }],
                alerts: vec![],
            }
            .into(),
        );
        let record = convert_event(&event, EXPORT_NANOS, false).expect("health report converts");

        RecordTimestamps {
            time_unix_nano: record.time_unix_nano,
            observed_time_unix_nano: record.observed_time_unix_nano,
        }
    }

    #[test]
    fn health_report_event_time_prefers_the_observation_time() {
        let observed_at = Utc
            .with_ymd_and_hms(2026, 7, 31, 12, 34, 56)
            .single()
            .expect("valid timestamp");
        let observed_nanos = u64::try_from(
            observed_at
                .timestamp_nanos_opt()
                .expect("timestamp has nanoseconds"),
        )
        .expect("timestamp is after the Unix epoch");

        value_scenarios!(health_report_timestamps:
            "observation time present" {
                Some(observed_at) => RecordTimestamps {
                    time_unix_nano: observed_nanos,
                    observed_time_unix_nano: EXPORT_NANOS,
                },
            }

            "observation time absent" {
                None => RecordTimestamps {
                    time_unix_nano: EXPORT_NANOS,
                    observed_time_unix_nano: EXPORT_NANOS,
                },
            }

            "observation time before the Unix epoch" {
                Utc.with_ymd_and_hms(1969, 12, 31, 23, 59, 59).single() => RecordTimestamps {
                    time_unix_nano: EXPORT_NANOS,
                    observed_time_unix_nano: EXPORT_NANOS,
                },
            }
        );
    }

    #[test]
    fn events_grouped_by_endpoint() {
        let ctx1 = EventContext {
            endpoint_key: "endpoint-a".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)),
                port: Some(443),
                mac: MacAddress::from_str("42:9e:b1:bd:9d:dd").expect("valid mac"),
            },
            collector_type: "test",
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        };
        let ctx2 = EventContext {
            endpoint_key: "endpoint-b".to_string(),
            ..ctx1.clone()
        };

        let log = |ctx| {
            (
                ctx,
                CollectorEvent::Log(Box::new(LogRecord {
                    body: "x".to_string(),
                    severity: LogSeverity::Info,
                    attributes: vec![],
                    diagnostic_record: None,
                })),
            )
        };

        let batch = vec![log(ctx1.clone()), log(ctx2), log(ctx1)];
        let request = build_export_request(&batch, false);

        assert_eq!(request.resource_logs.len(), 2);
        let total_records: usize = request
            .resource_logs
            .iter()
            .flat_map(|rl| &rl.scope_logs)
            .map(|sl| sl.log_records.len())
            .sum();
        assert_eq!(total_records, 3);
    }

    #[test]
    fn metric_resources_are_grouped_by_endpoint_and_collector() {
        let base_ctx = test_context();
        let rest_ctx = EventContext {
            collector_type: "nvue_rest",
            ..base_ctx.clone()
        };
        let gnmi_ctx = EventContext {
            collector_type: "nvue_gnmi",
            ..base_ctx
        };
        let sample = |name: &str| MetricSample {
            key: "status:swp1".to_string(),
            name: name.to_string(),
            metric_type: "interface_oper_status".to_string(),
            unit: "state".to_string(),
            value: 1.0,
            labels: vec![(Cow::Borrowed("interface_name"), "swp1".to_string())],
            context: None,
        };

        let request = build_metrics_export_request(
            &[
                (rest_ctx, sample("nvue_rest")),
                (gnmi_ctx, sample("nvue_gnmi")),
            ],
            "carbide_hardware_health",
        );

        let collector_types: std::collections::HashSet<_> = request
            .resource_metrics
            .iter()
            .filter_map(|resource_metrics| resource_metrics.resource.as_ref())
            .filter_map(|resource| attr_value(&resource.attributes, "collector.type"))
            .collect();

        assert_eq!(request.resource_metrics.len(), 2);
        assert!(collector_types.contains("nvue_rest"));
        assert!(collector_types.contains("nvue_gnmi"));
    }

    #[test]
    fn metric_export_name_uses_full_prometheus_series_name() {
        let ctx = test_context();
        let sample = MetricSample {
            key: "asic0/oper_status".to_string(),
            name: "nvue_gnmi".to_string(),
            metric_type: "interface_oper_status".to_string(),
            unit: "state".to_string(),
            value: 1.0,
            labels: vec![(Cow::Borrowed("path"), "/system/state".to_string())],
            context: None,
        };

        let request = build_metrics_export_request(&[(ctx, sample)], "carbide_hardware_health");
        let metrics = &request.resource_metrics[0].scope_metrics[0].metrics;

        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[0].name,
            "carbide_hardware_health_nvue_gnmi_interface_oper_status_state"
        );
        assert_eq!(metrics[0].unit, "state");
    }

    #[test]
    fn switch_nmxt_identity_is_resource_only_not_on_datapoint() {
        let switch_id = test_switch_id("switch-nmxt");
        let switch_id_attr = switch_id.to_string();
        let context = EventContext {
            endpoint_key: "11:22:33:44:55:66".to_string(),
            addr: BmcAddr {
                ip: IpAddr::V4(Ipv4Addr::new(10, 0, 1, 1)),
                port: Some(443),
                mac: MacAddress::from_str("11:22:33:44:55:66").expect("valid mac"),
            },
            collector_type: "nvue_gnmi",
            labels: Default::default(),
            metadata: Some(EndpointMetadata::Switch(SwitchData {
                id: Some(switch_id),
                serial: "SN-SWITCH-001".to_string(),
                slot_number: Some(7),
                tray_index: Some(3),
                nvlink_domain_uuid: None,
                endpoint_role: SwitchEndpointRole::Host,
                is_primary: true,
                nmxc_enabled: true,
                nmxt_enabled: true,
            })),
            rack_id: Some(RackId::new("RACK_2")),
        };
        let sample = MetricSample {
            key: "effective_ber".to_string(),
            name: "switch_nmxt".to_string(),
            metric_type: "effective_ber".to_string(),
            unit: "ratio".to_string(),
            value: 0.5,
            labels: vec![],
            context: None,
        };

        let request = build_metrics_export_request(&[(context, sample)], "carbide_hardware_health");
        let resource_metrics = &request.resource_metrics[0];
        let metrics = &resource_metrics.scope_metrics[0].metrics;

        assert_eq!(metrics.len(), 1);
        assert_eq!(
            metrics[0].name,
            "carbide_hardware_health_switch_nmxt_effective_ber_ratio"
        );

        let metric::Data::Gauge(gauge) = metrics[0].data.as_ref().expect("metric data") else {
            panic!("expected gauge data");
        };
        // Identity must NOT be promoted onto the datapoint (VM duplicates it from the resource).
        let attrs = &gauge.data_points[0].attributes;
        assert_eq!(attr_value(attrs, "switch_serial"), None);
        assert_eq!(attr_value(attrs, "switch_id"), None);

        // It lives once, on the resource (dotted form).
        let resource_attrs = &resource_metrics
            .resource
            .as_ref()
            .expect("resource")
            .attributes;
        assert_eq!(
            attr_value(resource_attrs, "switch.serial_number"),
            Some("SN-SWITCH-001")
        );
        assert_eq!(
            attr_value(resource_attrs, "switch.id"),
            Some(switch_id_attr.as_str())
        );
    }
}
