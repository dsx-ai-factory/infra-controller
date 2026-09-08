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
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nv_redfish::core::{Bmc, FilterQuery, ODataId};
use nv_redfish::log_service::LogService;
use nv_redfish::{Resource, ServiceRoot};
use serde::{Deserialize, Serialize};

use super::diagnostic::{
    DiagnosticPayload, make_diagnostic_record, nullable_ref, nullable_str, redfish_enum_string,
};
use super::redfish::{
    RedfishLogFields, RedfishSeverity, add_redfish_analyzer_attributes,
    log_entry_diagnostic_is_cper, nvidia_error_id, push_message_identity,
    redfish_event_type_string, redfish_log_type,
};
use crate::HealthError;
use crate::collectors::{IterationResult, PeriodicCollector};
use crate::endpoint::BmcEndpoint;
use crate::sink::{CollectorEvent, DataSink, EventContext, LogRecord};

/// Configuration for logs collector
pub struct LogsCollectorConfig {
    pub state_file_path: PathBuf,
    pub service_refresh_interval: Duration,
    pub data_sink: Option<Arc<dyn DataSink>>,

    /// Attach Redfish diagnostic payloads to emitted log records.
    pub include_diagnostics: bool,

    /// Substrings; a discovered LogService whose odata id contains any of these
    /// is skipped. Empty collects from every service.
    pub exclude_services: Vec<String>,

    /// When true, on the first encounter of a LogService with no saved state,
    /// anchor at the current highest log entry ID without emitting historical
    /// entries. Subsequent polls collect only new entries forward, matching
    /// SSE behaviour. When false (default), all existing entries are collected
    /// on first encounter.
    pub skip_initial_history: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct PersistentState {
    last_seen_ids: HashMap<ODataId, i32>,
}

#[derive(Serialize)]
struct PersistentStateRef<'a> {
    last_seen_ids: &'a HashMap<ODataId, i32>,
}

struct LogsCollectorState<B: Bmc> {
    discovered_services: Vec<LogService<B>>,
    last_service_refresh: Instant,
    last_seen_ids: HashMap<ODataId, i32>,
}

/// Logs collector for a single BMC endpoint
pub struct LogsCollector<B: Bmc> {
    endpoint: Arc<BmcEndpoint>,
    bmc: Arc<B>,
    event_context: EventContext,
    state_file_path: PathBuf,
    state: Option<LogsCollectorState<B>>,
    service_refresh_interval: Duration,
    data_sink: Option<Arc<dyn DataSink>>,
    include_diagnostics: bool,
    exclude_services: Vec<String>,
    skip_initial_history: bool,
}

impl<B: Bmc + 'static> PeriodicCollector<B> for LogsCollector<B> {
    type Config = LogsCollectorConfig;

    fn new_runner(
        bmc: Arc<B>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let event_context = EventContext::from_endpoint(endpoint.as_ref(), "logs_collector");
        Ok(Self {
            bmc,
            endpoint,
            event_context,
            state_file_path: config.state_file_path,
            state: None,
            service_refresh_interval: config.service_refresh_interval,
            data_sink: config.data_sink,
            include_diagnostics: config.include_diagnostics,
            exclude_services: config.exclude_services,
            skip_initial_history: config.skip_initial_history,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        self.run_collection_iteration().await
    }

    fn collector_type(&self) -> &'static str {
        "logs_collector"
    }

    async fn stop(&mut self) {
        if let Some(data_sink) = &self.data_sink {
            data_sink.handle_event(&self.event_context, &CollectorEvent::CollectorRemoved);
        }
    }
}

impl<B: Bmc + 'static> LogsCollector<B> {
    async fn load_persistent_state(&self) -> PersistentState {
        match tokio::fs::read_to_string(&self.state_file_path).await {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => PersistentState::default(),
        }
    }

    async fn save_persistent_state(&self) -> Result<(), HealthError> {
        if let Some(state) = &self.state {
            let json = serde_json::to_string_pretty(&PersistentStateRef {
                last_seen_ids: &state.last_seen_ids,
            })
            .map_err(|e| HealthError::GenericError(format!("Failed to serialize state: {}", e)))?;

            tokio::fs::write(&self.state_file_path, json)
                .await
                .map_err(|e| HealthError::GenericError(format!("Failed to write state: {}", e)))?;
        }

        Ok(())
    }

    /// True if this service's odata id matches any configured exclude substring.
    fn is_excluded(&self, service_id: &str) -> bool {
        service_is_excluded(&self.exclude_services, service_id)
    }

    async fn discover_log_services(&self) -> Result<Vec<LogService<B>>, HealthError> {
        let service_root = ServiceRoot::new(self.bmc.clone()).await?;
        let mut services = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut excluded_count = 0usize;

        let consider = |service: LogService<B>,
                        services: &mut Vec<LogService<B>>,
                        seen_ids: &mut HashSet<String>,
                        excluded_count: &mut usize| {
            let service_id = service.odata_id().to_string();
            if self.is_excluded(&service_id) {
                *excluded_count += 1;
                return;
            }
            if seen_ids.insert(service_id) {
                services.push(service);
            }
        };

        if let Ok(Some(manager_collection)) = service_root.managers().await {
            for manager in manager_collection.members().await.iter().flatten() {
                if let Ok(Some(log_services)) = manager.log_services().await {
                    for service in log_services {
                        consider(service, &mut services, &mut seen_ids, &mut excluded_count);
                    }
                }
            }
        }

        if let Ok(Some(chassis_collection)) = service_root.chassis().await {
            for chassis in chassis_collection.members().await.iter().flatten() {
                if let Ok(Some(log_services)) = chassis.log_services().await {
                    for service in log_services {
                        consider(service, &mut services, &mut seen_ids, &mut excluded_count);
                    }
                }
            }
        }

        if let Ok(Some(system_collection)) = service_root.systems().await {
            for system in system_collection.members().await.iter().flatten() {
                if let Ok(Some(log_services)) = system.log_services().await {
                    for service in log_services {
                        consider(service, &mut services, &mut seen_ids, &mut excluded_count);
                    }
                }
            }
        }

        tracing::info!(
            service_count = services.len(),
            excluded_service_count = excluded_count,
            rack_id = self.event_context.rack_id().map(tracing::field::display),
            "Discovered distinct log services"
        );

        Ok(services)
    }

    async fn run_collection_iteration(&mut self) -> Result<IterationResult, HealthError> {
        let needs_refresh = self
            .state
            .as_ref()
            .map(|s| s.last_service_refresh.elapsed() > self.service_refresh_interval)
            .unwrap_or(true);

        let mut refresh_triggered = false;

        if needs_refresh {
            tracing::info!(
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "Refreshing log services for BMC"
            );

            match self.discover_log_services().await {
                Ok(services) => {
                    tracing::info!(
                        service_count = services.len(),
                        rack_id = self.event_context.rack_id().map(tracing::field::display),
                        "Log service discovery complete"
                    );

                    let persistent_state = self.load_persistent_state().await;

                    self.state = Some(LogsCollectorState {
                        discovered_services: services,
                        last_service_refresh: Instant::now(),
                        last_seen_ids: persistent_state.last_seen_ids,
                    });
                    refresh_triggered = true;
                }
                Err(e) => {
                    tracing::error!(
                        error = ?e,
                        rack_id = self.event_context.rack_id().map(tracing::field::display),
                        "Failed to discover log services"
                    );

                    if self.state.is_none() {
                        return Err(e);
                    }
                }
            }
        }

        let (log_count, fetch_failures) = self.collect_logs_from_services().await?;
        self.save_persistent_state().await?;

        Ok(IterationResult {
            refresh_triggered,
            entity_count: Some(log_count),
            fetch_failures,
        })
    }

    async fn collect_logs_from_services(&mut self) -> Result<(usize, usize), HealthError> {
        if !self.endpoint.supports_periodic_logs() {
            return Ok((0, 0));
        }

        let machine_id = self.event_context.machine_id().map(|id| id.to_string());

        let Some(state) = self.state.as_mut() else {
            return Ok((0, 0));
        };

        let mut total_log_count = 0;
        let mut fetch_failures = 0;

        for service in &state.discovered_services {
            let service_id = service.odata_id().to_string();
            let last_seen_id = state.last_seen_ids.get(service.odata_id()).copied();

            let entries = match last_seen_id {
                Some(last_id) => {
                    let entries = match service
                        .filter_entries(FilterQuery::gt(&"Id", last_id))
                        .await
                    {
                        Ok(Some(e)) => e,
                        Ok(None) => continue,
                        Err(error) => {
                            tracing::debug!(
                                %service_id,
                                ?error,
                                rack_id = self.event_context.rack_id().map(tracing::field::display),
                                "Failed to fetch filtered log entries, fetching all"
                            );
                            // Fallback - if filter is not supported properly
                            match service.entries().await {
                                Ok(Some(e)) => e,
                                Ok(None) => continue,
                                Err(error) => {
                                    fetch_failures += 1;
                                    tracing::warn!(
                                        %service_id,
                                        ?error,
                                        rack_id = self.event_context.rack_id().map(tracing::field::display),
                                        "Failed to fetch log entries"
                                    );
                                    continue;
                                }
                            }
                        }
                    };

                    // We apply manual filter in either case, if BMC is returns all entries even
                    // with filter applied
                    entries
                        .into_iter()
                        .filter(|entry| {
                            entry
                                .base
                                .id
                                .parse::<i32>()
                                .ok()
                                .map(|id| id > last_id)
                                .unwrap_or(false)
                        })
                        .collect()
                }
                None => {
                    let all_entries = match service.entries().await {
                        Ok(Some(v)) => v,
                        Ok(None) => continue,
                        Err(error) => {
                            fetch_failures += 1;
                            tracing::warn!(
                                %service_id,
                                ?error,
                                rack_id = self.event_context.rack_id().map(tracing::field::display),
                                "Failed to fetch log entries"
                            );
                            continue;
                        }
                    };

                    if self.skip_initial_history {
                        // Anchor at the current highest entry ID without emitting
                        // historical entries, so the next poll collects only new
                        // entries. Matches the real-time-only behaviour of SSE.
                        //
                        // We always write a sentinel (-1) when the service is
                        // empty or has no parseable numeric IDs, so the service
                        // is marked initialised in last_seen_ids. Without this,
                        // a subsequent poll would re-enter this None arm and
                        // treat the first real entry as "initial history" and
                        // discard it. -1 is safe: real Redfish IDs are ≥ 0, so
                        // the next poll's `id > anchor` filter passes everything.
                        let anchor_id =
                            initial_anchor_id(all_entries.iter().map(|e| e.base.id.as_str()));
                        state
                            .last_seen_ids
                            .insert(service.odata_id().clone(), anchor_id);
                        tracing::info!(
                            %service_id,
                            anchor_id,
                            rack_id = self.event_context.rack_id().map(tracing::field::display),
                            "skip_initial_history: anchored at current log position, \
                             skipping historical entries"
                        );
                        continue;
                    }

                    tracing::info!(
                        %service_id,
                        endpoint=?self.endpoint.addr,
                        rack_id = self.event_context.rack_id().map(tracing::field::display),
                        "Last seen id is empty, fetching all entries"
                    );
                    all_entries
                }
            };

            if entries.is_empty() {
                continue;
            }

            let mut max_id = last_seen_id.unwrap_or(0);

            for entry in &entries {
                let log_event = entry_to_log(
                    entry,
                    machine_id.as_deref(),
                    &service_id,
                    self.include_diagnostics,
                );
                if let Some(data_sink) = &self.data_sink {
                    data_sink.handle_event(&self.event_context, &log_event);
                }

                if let Ok(entry_id) = entry.base.id.parse::<i32>() {
                    max_id = max_id.max(entry_id);
                }
            }

            if max_id > last_seen_id.unwrap_or(0) {
                state
                    .last_seen_ids
                    .insert(service.odata_id().clone(), max_id);
            }
            total_log_count += entries.len();
        }

        Ok((total_log_count, fetch_failures))
    }
}

fn entry_to_log(
    entry: &nv_redfish::schema::log_entry::LogEntry,
    machine_id: Option<&str>,
    service_id: &str,
    include_diagnostics: bool,
) -> CollectorEvent {
    let redfish_severity = entry
        .severity
        .as_ref()
        .and_then(Option::as_ref)
        .map(RedfishSeverity::from_event_severity);
    // Omitted, null, and unsupported Redfish severities do
    // not imply a severity level.
    let severity = redfish_severity.unwrap_or(RedfishSeverity::Unknown).into();

    let diagnostic_data_type =
        nullable_ref(&entry.diagnostic_data_type).and_then(redfish_enum_string);
    let log_type = redfish_log_type(RedfishLogFields {
        message: nullable_str(&entry.message),
        message_args: entry.message_args.as_deref(),
        has_cper: entry.cper.is_some()
            || nullable_ref(&entry.diagnostic_data_type).is_some_and(log_entry_diagnostic_is_cper),
    });

    let diagnostic_record = include_diagnostics
        .then(|| {
            make_diagnostic_record(DiagnosticPayload {
                diagnostic_data: nullable_str(&entry.diagnostic_data),
                diagnostic_data_type,
                oem_diagnostic_data_type: nullable_str(&entry.oem_diagnostic_data_type),
                additional_data_uri: nullable_str(&entry.additional_data_uri),
                additional_data_size_bytes: nullable_ref(&entry.additional_data_size_bytes)
                    .copied(),
                message_id: entry.message_id.as_deref(),
                event_id: entry.event_id.as_deref(),
                log_entry_id: Some(entry.base.id.as_str()),
            })
        })
        .flatten();

    let mut attributes = Vec::with_capacity(14);
    if let Some(machine_id) = machine_id {
        attributes.push((Cow::Borrowed("machine_id"), machine_id.to_string()));
    }
    attributes.push((Cow::Borrowed("entry_id"), entry.base.id.clone()));
    attributes.push((Cow::Borrowed("service_id"), service_id.to_string()));
    if let Some(oem) = &entry.base.base.oem {
        attributes.push((
            Cow::Borrowed("redfish.oem"),
            oem.additional_properties.to_string(),
        ));
    }
    add_redfish_analyzer_attributes(
        &mut attributes,
        log_type,
        redfish_severity.unwrap_or(RedfishSeverity::Unknown),
        nvidia_error_id(entry.base.base.oem.as_ref()),
    );
    push_message_identity(
        &mut attributes,
        entry.message_id.as_deref(),
        nullable_str(&entry.message),
    );
    if let Some(args) = &entry.message_args {
        attributes.push((
            Cow::Borrowed("message_args"),
            serde_json::to_string(args).unwrap_or_default(),
        ));
    }
    if let Some(event_type) = redfish_event_type_string(entry.event_type.as_ref()) {
        attributes.push((Cow::Borrowed("event_type"), event_type));
    }
    if let Some(event_id) = &entry.event_id {
        attributes.push((Cow::Borrowed("event_id"), event_id.clone()));
    }
    if let Some(timestamp) = &entry.event_timestamp {
        attributes.push((Cow::Borrowed("event_timestamp"), timestamp.to_string()));
    }
    if let Some(group_id) = nullable_ref(&entry.event_group_id) {
        attributes.push((Cow::Borrowed("event_group_id"), group_id.to_string()));
    }
    if let Some(resolution) = &entry.resolution {
        attributes.push((Cow::Borrowed("resolution"), resolution.clone()));
    }
    if let Some(origin) = entry
        .links
        .as_ref()
        .and_then(|links| links.origin_of_condition.as_ref())
    {
        attributes.push((
            Cow::Borrowed("origin_of_condition"),
            origin.odata_id.to_string(),
        ));
    }

    CollectorEvent::Log(Box::new(LogRecord {
        body: nullable_str(&entry.message).unwrap_or_default().to_string(),
        severity,
        attributes,
        diagnostic_record,
    }))
}

/// Returns the highest parseable integer ID from `ids`, or -1 when `ids` is
/// empty or contains no parseable integers.
///
/// Used as the `last_seen_ids` anchor when `skip_initial_history = true`. -1
/// is a safe sentinel because real Redfish entry IDs are non-negative, so a
/// subsequent poll's `id > anchor` filter passes every entry.
fn initial_anchor_id<'a>(ids: impl Iterator<Item = &'a str>) -> i32 {
    ids.filter_map(|id| id.parse::<i32>().ok())
        .max()
        .unwrap_or(-1)
}

/// True if `service_id` contains any of the configured exclude substrings.
/// An empty `exclude_services` never excludes anything. Matching is a plain
/// (case-sensitive) substring test against the Redfish LogService odata id.
fn service_is_excluded(exclude_services: &[String], service_id: &str) -> bool {
    exclude_services
        .iter()
        .any(|pat| !pat.is_empty() && service_id.contains(pat.as_str()))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};
    use serde_json::{Value, json};

    use super::*;
    use crate::sink::LogSeverity;

    const JOURNAL_BMC: &str = "/redfish/v1/Managers/BMC_0/LogServices/Journal";
    const JOURNAL_HGX: &str = "/redfish/v1/Managers/HGX_BMC_0/LogServices/Journal";
    const EVENTLOG: &str = "/redfish/v1/Systems/System_0/LogServices/EventLog";
    const XID: &str = "/redfish/v1/Chassis/HGX_GPU_0/LogServices/XID";
    const SEL: &str = "/redfish/v1/Systems/System_0/LogServices/SEL";

    #[derive(Debug, PartialEq)]
    struct ObservedLog {
        body: String,
        log_type: Option<String>,
        event_type: Option<String>,
        redfish_severity: Option<String>,
        redfish_oem: Option<String>,
        error_id: Option<String>,
        diagnostic_data: Option<String>,
    }

    fn attribute(record: &LogRecord, key: &str) -> Option<String> {
        record
            .attributes
            .iter()
            .find(|(candidate, _)| candidate.as_ref() == key)
            .map(|(_, value)| value.clone())
    }

    fn observe_log_entry(value: Value) -> ObservedLog {
        let entry: nv_redfish::schema::log_entry::LogEntry =
            serde_json::from_value(value).expect("valid Redfish log entry");
        let event = entry_to_log(&entry, Some("machine-1"), EVENTLOG, true);
        let CollectorEvent::Log(record) = event else {
            panic!("expected log event");
        };

        ObservedLog {
            body: record.body.clone(),
            log_type: attribute(&record, "redfish.event.type"),
            event_type: attribute(&record, "event_type"),
            redfish_severity: attribute(&record, "redfish.event.severity"),
            redfish_oem: attribute(&record, "redfish.oem"),
            error_id: attribute(&record, "oem.nvidia.error_id"),
            diagnostic_data: record
                .diagnostic_record
                .as_ref()
                .map(|diagnostic| diagnostic.body.clone()),
        }
    }

    fn observe_log_severity(severity: Option<Option<&str>>) -> (LogSeverity, Option<String>) {
        let mut value = json!({
            "@odata.id": "/redfish/v1/Systems/System_0/LogServices/EventLog/Entries/1",
            "Id": "1",
            "Name": "Platform event",
            "EntryType": "Event",
            "Message": "Platform event"
        });
        if let Some(severity) = severity {
            value["Severity"] = severity.map_or(Value::Null, |severity| json!(severity));
        }

        let entry: nv_redfish::schema::log_entry::LogEntry =
            serde_json::from_value(value).expect("valid Redfish log entry");
        let CollectorEvent::Log(record) = entry_to_log(&entry, None, EVENTLOG, false) else {
            panic!("expected log event");
        };

        (
            record.severity,
            attribute(&record, "redfish.event.severity"),
        )
    }

    fn observe_message_identity(
        message_id: Option<&str>,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let mut value = json!({
            "@odata.id": "/redfish/v1/Managers/bmc/LogServices/EventLog/Entries/2656",
            "Id": "2656",
            "Name": "System Event Log Entry",
            "EntryType": "Event",
            "Severity": "OK",
            "Message": "PowerDevicePresence ( powerdevice1 chassis_SN: 613337RXX01X75101UG Assert )"
        });
        if let Some(message_id) = message_id {
            value["MessageId"] = json!(message_id);
        }
        let entry: nv_redfish::schema::log_entry::LogEntry =
            serde_json::from_value(value).expect("valid Redfish log entry");
        let CollectorEvent::Log(record) = entry_to_log(&entry, None, EVENTLOG, false) else {
            panic!("expected log event");
        };
        (
            attribute(&record, "message_id"),
            attribute(&record, "message_family"),
            attribute(&record, "redfish.component"),
        )
    }

    #[test]
    fn message_identity_attributes_only_without_message_id() {
        check_values(
            [
                Check {
                    scenario: "null MessageId derives identity from the message text",
                    input: None,
                    expect: (
                        None,
                        Some("PowerDevicePresence".to_string()),
                        Some("powerdevice1".to_string()),
                    ),
                },
                Check {
                    scenario: "populated MessageId is forwarded unchanged",
                    input: Some("ResourceEvent.1.0.ResourceStatusChangedOK"),
                    expect: (
                        Some("ResourceEvent.1.0.ResourceStatusChangedOK".to_string()),
                        None,
                        None,
                    ),
                },
            ],
            observe_message_identity,
        );
    }

    #[test]
    fn periodic_severity_matches_sse() {
        check_values(
            [
                Check {
                    scenario: "critical severity",
                    input: Some(Some("Critical")),
                    expect: (LogSeverity::Fatal, Some("Critical".to_string())),
                },
                Check {
                    scenario: "warning severity",
                    input: Some(Some("Warning")),
                    expect: (LogSeverity::Warn, Some("Warning".to_string())),
                },
                Check {
                    scenario: "OK severity",
                    input: Some(Some("OK")),
                    expect: (LogSeverity::Info, Some("OK".to_string())),
                },
                Check {
                    scenario: "omitted severity",
                    input: None,
                    expect: (LogSeverity::Unspecified, Some("Unknown".to_string())),
                },
                Check {
                    scenario: "null severity",
                    input: Some(None),
                    expect: (LogSeverity::Unspecified, Some("Unknown".to_string())),
                },
                Check {
                    scenario: "unsupported severity",
                    input: Some(Some("Meltdown")),
                    expect: (LogSeverity::Unspecified, Some("Unknown".to_string())),
                },
            ],
            observe_log_severity,
        );
    }

    #[test]
    fn periodic_entries_emit_analyzer_fields() {
        check_values(
            [
                Check {
                    scenario: "c12 platform fault with empty message",
                    input: json!({
                        "@odata.id": "/redfish/v1/Systems/System_0/LogServices/EventLog/Entries/1",
                        "Id": "1",
                        "Name": "CPLD power sequence fault",
                        "EntryType": "Event",
                        "Severity": "Critical",
                        "Message": "",
                        "MessageId": "IANA.0.1.CPLD-PSEQ-FAULT",
                        "MessageArgs": ["CPLD_0", ""],
                        "EventType": "Alert",
                        "Oem": {"Nvidia": {"ErrorId": "CPLD-PSEQ-FAULT"}},
                        "Links": {
                            "OriginOfCondition": {
                                "@odata.id": "/redfish/v1/Chassis/HGX_Baseboard_0"
                            }
                        }
                    }),
                    expect: ObservedLog {
                        body: String::new(),
                        log_type: Some("redfish_event".to_string()),
                        event_type: Some("Alert".to_string()),
                        redfish_severity: Some("Critical".to_string()),
                        redfish_oem: Some(
                            r#"{"Nvidia":{"ErrorId":"CPLD-PSEQ-FAULT"}}"#.to_string(),
                        ),
                        error_id: Some("CPLD-PSEQ-FAULT".to_string()),
                        diagnostic_data: None,
                    },
                },
                Check {
                    scenario: "xid log entry",
                    input: json!({
                        "@odata.id": "/redfish/v1/Systems/System_0/LogServices/EventLog/Entries/2",
                        "Id": "2",
                        "Name": "GPU fault",
                        "EntryType": "Event",
                        "Severity": "Warning",
                        "Message": "GPU reported Xid 79",
                        "MessageId": "Nvidia.1.0.GpuXid"
                    }),
                    expect: ObservedLog {
                        body: "GPU reported Xid 79".to_string(),
                        log_type: Some("xid".to_string()),
                        event_type: None,
                        redfish_severity: Some("Warning".to_string()),
                        redfish_oem: None,
                        error_id: None,
                        diagnostic_data: None,
                    },
                },
                Check {
                    scenario: "cper log entry",
                    input: json!({
                        "@odata.id": "/redfish/v1/Systems/System_0/LogServices/EventLog/Entries/3",
                        "Id": "3",
                        "Name": "PCIe CPER",
                        "EntryType": "Event",
                        "Severity": "Critical",
                        "Message": "PCIe error",
                        "MessageId": "ResourceEvent.1.0.ResourceErrorsDetected",
                        "DiagnosticData": "base64-cper-payload",
                        "DiagnosticDataType": "CPER",
                        "CPER": {}
                    }),
                    expect: ObservedLog {
                        body: "PCIe error".to_string(),
                        log_type: Some("cper".to_string()),
                        event_type: None,
                        redfish_severity: Some("Critical".to_string()),
                        redfish_oem: None,
                        error_id: None,
                        diagnostic_data: Some("base64-cper-payload".to_string()),
                    },
                },
            ],
            observe_log_entry,
        );
    }

    #[test]
    fn periodic_cper_diagnostics_can_be_disabled() {
        let entry: nv_redfish::schema::log_entry::LogEntry = serde_json::from_value(json!({
            "@odata.id": "/redfish/v1/Systems/System_0/LogServices/EventLog/Entries/3",
            "Id": "3",
            "Name": "PCIe CPER",
            "EntryType": "Event",
            "Severity": "Critical",
            "Message": "PCIe error",
            "MessageId": "ResourceEvent.1.0.ResourceErrorsDetected",
            "DiagnosticData": "base64-cper-payload",
            "DiagnosticDataType": "CPER",
            "CPER": {}
        }))
        .expect("valid CPER log entry");

        let event = entry_to_log(&entry, None, EVENTLOG, false);
        let CollectorEvent::Log(record) = event else {
            panic!("expected log event");
        };
        assert_eq!(record.body, "PCIe error");
        assert!(record.diagnostic_record.is_none());
    }

    #[test]
    fn service_exclusion_filter() {
        check_values(
            [
                Check {
                    scenario: "empty exclude list keeps all services",
                    input: (vec![], JOURNAL_BMC),
                    expect: false,
                },
                Check {
                    scenario: "empty string pattern never excludes",
                    input: (vec!["".to_string()], JOURNAL_BMC),
                    expect: false,
                },
                Check {
                    scenario: "substring match excludes BMC journal",
                    input: (vec!["Journal".to_string()], JOURNAL_BMC),
                    expect: true,
                },
                Check {
                    scenario: "substring match excludes HGX journal",
                    input: (vec!["Journal".to_string()], JOURNAL_HGX),
                    expect: true,
                },
                Check {
                    scenario: "non-matching service is kept",
                    input: (vec!["Journal".to_string()], EVENTLOG),
                    expect: false,
                },
                Check {
                    scenario: "any of multiple patterns excludes",
                    input: (vec!["Journal".to_string(), "Dump".to_string()], JOURNAL_BMC),
                    expect: true,
                },
                Check {
                    scenario: "second pattern in list matches",
                    input: (
                        vec!["Journal".to_string(), "Dump".to_string()],
                        "/redfish/v1/Managers/BMC_0/LogServices/Dump",
                    ),
                    expect: true,
                },
                Check {
                    scenario: "no pattern matches non-excluded services",
                    input: (vec!["Journal".to_string(), "Dump".to_string()], XID),
                    expect: false,
                },
                Check {
                    scenario: "matching is case-sensitive",
                    input: (
                        vec!["Journal".to_string()],
                        "/redfish/v1/Managers/BMC_0/LogServices/journal",
                    ),
                    expect: false,
                },
                Check {
                    scenario: "SEL service is not excluded by Journal pattern",
                    input: (vec!["Journal".to_string()], SEL),
                    expect: false,
                },
            ],
            |(patterns, service_id)| service_is_excluded(&patterns, service_id),
        );
    }

    #[test]
    fn initial_anchor_id_cases() {
        check_values(
            [
                Check {
                    scenario: "empty service yields sentinel -1",
                    input: vec![],
                    expect: -1,
                },
                Check {
                    scenario: "single numeric id is returned",
                    input: vec!["42"],
                    expect: 42,
                },
                Check {
                    scenario: "max of multiple numeric ids is returned",
                    input: vec!["1", "99", "7"],
                    expect: 99,
                },
                Check {
                    scenario: "non-parseable ids only yield sentinel -1",
                    input: vec!["abc", "xyz"],
                    expect: -1,
                },
                Check {
                    scenario: "mixed parseable and non-parseable uses numeric max",
                    input: vec!["abc", "5", "xyz", "3"],
                    expect: 5,
                },
                Check {
                    scenario: "id zero is returned (not confused with sentinel)",
                    input: vec!["0"],
                    expect: 0,
                },
            ],
            |ids: Vec<&str>| initial_anchor_id(ids.into_iter()),
        );
    }
}
