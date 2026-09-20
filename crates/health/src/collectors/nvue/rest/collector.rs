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
use std::sync::Arc;

use super::client::{
    LeakageEnvironmentResponse, LeakageSensorData, OptionalNvueResponse, RebootReasonResponse,
    RestClient, UsernamePassword,
};
use crate::HealthError;
use crate::bmc::{CREDENTIAL_REFRESH_TIMEOUT, CredentialProvider, is_auth_error};
use crate::collectors::{IterationResult, PeriodicCollector};
use crate::config::NvueRestConfig;
use crate::endpoint::{BmcAddr, BmcCredentials, BmcEndpoint, EndpointMetadata};
use crate::sink::{
    Classification, CollectorEvent, DataSink, EventContext, HealthReport, HealthReportAlert,
    HealthReportSuccess, HealthReportTarget, LogRecord, LogSeverity, MetricSample, Probe,
    ReportSource,
};

const COLLECTOR_NAME: &str = "nvue_rest";

const SYSTEM_HEALTH_STATES: &[&str] = &["ok", "not_ok", "unknown"];

fn system_health_to_state(status: Option<&str>) -> &'static str {
    match status {
        Some("OK") => "ok",
        Some("Not OK") => "not_ok",
        _ => "unknown",
    }
}

const PARTITION_HEALTH_STATES: &[&str] = &[
    "healthy",
    "degraded_bandwidth",
    "degraded",
    "unhealthy",
    "unknown",
];

fn partition_health_to_state(status: Option<&str>) -> &'static str {
    match status {
        Some("healthy") => "healthy",
        Some("degraded_bandwidth") => "degraded_bandwidth",
        Some("degraded") => "degraded",
        Some("unhealthy") => "unhealthy",
        _ => "unknown",
    }
}

const APP_STATUS_STATES: &[&str] = &["ok", "not_ok", "unknown"];

fn app_status_to_state(status: Option<&str>) -> &'static str {
    match status {
        Some("ok") => "ok",
        Some("not ok") => "not_ok",
        _ => "unknown",
    }
}

/// "0" -> no issue. Any other opcode indicates a problem
fn diagnostic_opcode_to_f64(code: &str) -> f64 {
    match code {
        "0" => 0.0,
        _ => 1.0,
    }
}

/// NVUE reports fan max-speed as a string (e.g. "33000"). Parse it to RPM.
/// Returns None when the field is absent or unparseable.
fn fan_max_speed_to_f64(max_speed: Option<&str>) -> Option<f64> {
    max_speed
        .and_then(|s| s.trim().parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value >= 0.0)
}

/// Bounded StateSet domain for NVUE fan health. `Unknown` keeps an absent or
/// explicitly unavailable reading distinguishable from a reported fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FanState {
    Ok,
    NotOk,
    Unknown,
}

impl FanState {
    /// The emitted series name. `const` so [`FAN_STATE_STATES`] can be built
    /// from the variants instead of repeating the wire strings.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::NotOk => "not_ok",
            Self::Unknown => "unknown",
        }
    }
}

impl std::fmt::Display for FanState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(thiserror::Error, Debug)]
#[error("unrecognized NVUE fan state '{0}'")]
struct UnrecognizedFanState(String);

impl std::str::FromStr for FanState {
    type Err = UnrecognizedFanState;

    /// Accepts the NVUE fan-health vocabulary case-insensitively, ignoring
    /// surrounding whitespace. An empty reading is `Unknown`; a value outside
    /// the vocabulary is rejected so the caller decides what it means.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        if s.eq_ignore_ascii_case("ok") {
            Ok(Self::Ok)
        } else if s.is_empty()
            || s.eq_ignore_ascii_case("unknown")
            || s.eq_ignore_ascii_case("unavailable")
            || s.eq_ignore_ascii_case("n/a")
        {
            Ok(Self::Unknown)
        } else {
            Err(UnrecognizedFanState(s.to_string()))
        }
    }
}

const FAN_STATE_STATES: &[&str] = &[
    FanState::Ok.as_str(),
    FanState::NotOk.as_str(),
    FanState::Unknown.as_str(),
];

/// Maps NVUE fan health to the bounded StateSet domain. An absent reading is
/// unknown; a reading outside the recognized vocabulary is a fault.
fn fan_state_to_state(state: Option<&str>) -> FanState {
    match state {
        Some(s) => s.parse().unwrap_or(FanState::NotOk),
        None => FanState::Unknown,
    }
}

/// NVUE reports temps (current/max/crit) as Celsius strings (e.g. "105.00").
/// Parse to f64. Returns None when the field is absent or unparseable.
fn temp_to_f64(value: Option<&str>) -> Option<f64> {
    value.and_then(|s| s.trim().parse::<f64>().ok())
}

const LEAKAGE_STATES: &[&str] = &["ok", "leak", "unknown"];

/// Maps NVUE leakage sensor strings to the emitted StateSet domain.
///
/// NVUE OpenAPI defines populated leakage sensor states as `ok` or `leak`.
/// `unknown` is an emitted fallback for per-sensor `null`, absent state, or an
/// unrecognized value; the health report classifies that fallback as a sensor
/// failure.
fn leakage_state_to_state(state: Option<&str>) -> &'static str {
    match state.map(str::trim) {
        Some(s) if s.eq_ignore_ascii_case("ok") => "ok",
        Some(s) if s.eq_ignore_ascii_case("leak") => "leak",
        _ => "unknown",
    }
}

const TEMP_STATE_STATES: &[&str] = &["ok", "not_ok"];

/// Sensor `state` -> StateSet: "ok" (case-insensitive) => "ok", other present
/// => "not_ok", absent => None.
fn temp_state_to_state(state: Option<&str>) -> Option<&'static str> {
    state.map(|s| {
        if s.trim().eq_ignore_ascii_case("ok") {
            "ok"
        } else {
            "not_ok"
        }
    })
}

const FAN_LED_STATES: &[&str] = &["ok", "not_ok"];

/// `FAN_STATUS` LED -> StateSet: "green"/"ok" (case-insensitive) => "ok",
/// other non-empty => "not_ok", absent/empty => None.
fn fan_led_to_state(state: Option<&str>) -> Option<&'static str> {
    let s = state?.trim();
    if s.is_empty() {
        return None;
    }
    if s.eq_ignore_ascii_case("green") || s.eq_ignore_ascii_case("ok") {
        Some("ok")
    } else {
        Some("not_ok")
    }
}

pub struct NvueRestCollectorConfig {
    /// User-facing NVUE REST collector settings from the health service configuration.
    pub rest_config: NvueRestConfig,

    /// Optional sink that receives NVUE REST health reports and events.
    pub data_sink: Option<Arc<dyn DataSink>>,

    /// Whether an enabled sink consumes structured log events.
    pub log_event_sink_enabled: bool,

    /// Credential source used to authenticate to NVUE REST.
    pub credential_provider: Arc<dyn CredentialProvider>,

    /// Shared mTLS HTTP client provider used for HTTPS polling when configured.
    pub(crate) tls_http_client_provider: Option<crate::tls::MtlsHttpClientProvider>,
}

pub struct NvueRestCollector {
    client: RestClient,
    switch_id: String,
    event_context: EventContext,
    data_sink: Option<Arc<dyn DataSink>>,
    log_event_sink_enabled: bool,
    addr: BmcAddr,
    provider: Arc<dyn CredentialProvider>,
}

impl PeriodicCollector<crate::bmc::BmcClient> for NvueRestCollector {
    type Config = NvueRestCollectorConfig;

    fn new_runner(
        _bmc: Arc<crate::bmc::BmcClient>,
        endpoint: Arc<BmcEndpoint>,
        config: Self::Config,
    ) -> Result<Self, HealthError> {
        let switch_id = match &endpoint.metadata {
            Some(EndpointMetadata::Switch(s)) => s.serial.clone(),
            _ => endpoint.addr.mac.to_string(),
        };

        let event_context = EventContext::from_endpoint(endpoint.as_ref(), COLLECTOR_NAME);

        let rest_cfg = &config.rest_config;
        // self_signed_tls is always true -- TLS cert provisioning on switches is not yet implemented
        let client = RestClient::new(
            switch_id.clone(),
            endpoint.addr.ip,
            endpoint.addr.port,
            rest_cfg.request_timeout,
            true,
            config.tls_http_client_provider,
            rest_cfg.paths.clone(),
        )?;

        Ok(Self {
            client,
            switch_id,
            event_context,
            data_sink: config.data_sink,
            log_event_sink_enabled: config.log_event_sink_enabled,
            addr: endpoint.addr.clone(),
            provider: config.credential_provider,
        })
    }

    async fn run_iteration(&mut self) -> Result<IterationResult, HealthError> {
        self.client.ensure_http_client().await?;

        if !self.client.has_credentials()
            && let Err(error) = self.refresh_rest_credentials().await
        {
            tracing::warn!(
                ?error,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: skipping iteration — credential fetch failed"
            );
            return Ok(IterationResult {
                refresh_triggered: false,
                entity_count: Some(0),
                fetch_failures: 1,
            });
        }

        self.emit_event(CollectorEvent::MetricCollectionStart);
        let mut entity_count = 0usize;
        let mut fetch_failures = 0usize;
        let mut saw_auth_failure = false;

        match self.client.get_system_health().await {
            Ok(Some(health)) => {
                let current = system_health_to_state(health.status.as_deref());
                self.emit_state_set("system_health", None, current, SYSTEM_HEALTH_STATES, vec![]);
                entity_count += 1;
            }
            Ok(None) => {}
            Err(e) => {
                fetch_failures += 1;
                saw_auth_failure |= is_auth_error(&e);
                tracing::warn!(
                error = ?e,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: failed to collect system health"
                );
            }
        }

        match self.client.get_system_reboot_reason().await {
            Ok(OptionalNvueResponse::Present(reason)) => {
                self.emit_reboot_reason_data(&reason);

                entity_count += 1;
            }
            Ok(OptionalNvueResponse::Null | OptionalNvueResponse::Disabled) => {}
            Err(e) => {
                fetch_failures += 1;
                saw_auth_failure |= is_auth_error(&e);
                tracing::warn!(
                error = ?e,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: failed to collect system reboot reason"
                );
            }
        }

        match self.client.get_cluster_apps().await {
            Ok(Some(apps)) => {
                for (name, app) in &apps {
                    let current = app_status_to_state(app.status.as_deref());
                    self.emit_state_set(
                        "cluster_app",
                        Some(name),
                        current,
                        APP_STATUS_STATES,
                        vec![(Cow::Borrowed("app_name"), name.clone())],
                    );
                    entity_count += 1;
                }
            }
            Ok(None) => {}
            Err(e) => {
                fetch_failures += 1;
                saw_auth_failure |= is_auth_error(&e);
                tracing::warn!(
                error = ?e,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: failed to collect cluster apps"
                );
            }
        }

        match self.client.get_sdn_partitions().await {
            Ok(Some(partitions)) => {
                for (part_id, partition) in &partitions {
                    let part_name = partition.name.as_deref().unwrap_or(part_id);
                    let health_state = partition_health_to_state(partition.health.as_deref());
                    let gpu_count = partition.num_gpus.unwrap_or(0) as f64;

                    let partition_labels = vec![
                        (Cow::Borrowed("partition_id"), part_id.clone()),
                        (Cow::Borrowed("partition_name"), part_name.to_string()),
                    ];
                    self.emit_state_set(
                        "partition_health",
                        Some(part_id),
                        health_state,
                        PARTITION_HEALTH_STATES,
                        partition_labels.clone(),
                    );
                    self.emit_metric(
                        "partition_gpu",
                        Some(part_id),
                        gpu_count,
                        "count",
                        partition_labels,
                    );
                    entity_count += 1;
                }
            }
            Ok(None) => {}
            Err(e) => {
                fetch_failures += 1;
                saw_auth_failure |= is_auth_error(&e);
                tracing::warn!(
                error = ?e,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: failed to collect SDN partitions"
                );
            }
        }

        match self.client.get_link_diagnostics().await {
            Ok(diagnostics) => {
                for diag in &diagnostics {
                    let value = diagnostic_opcode_to_f64(&diag.code);
                    self.emit_metric(
                        "link_diagnostic",
                        Some(&format!("{}:{}", diag.interface, diag.code)),
                        value,
                        "state",
                        vec![
                            (Cow::Borrowed("interface_name"), diag.interface.clone()),
                            (Cow::Borrowed("opcode"), diag.code.clone()),
                            (Cow::Borrowed("diagnostic_status"), diag.status.clone()),
                        ],
                    );
                    entity_count += 1;
                }
            }
            Err(e) => {
                fetch_failures += 1;
                saw_auth_failure |= is_auth_error(&e);
                tracing::warn!(
                error = ?e,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: failed to collect link diagnostics"
                );
            }
        }

        match self.client.get_platform_environment_fan().await {
            Ok(Some(fans)) => {
                for (fan_name, fan) in &fans {
                    let labels = || vec![(Cow::Borrowed("fan_name"), fan_name.clone())];

                    // Only emit when max-speed parses. Absent or garbage emits nothing.
                    if let Some(value) = fan_max_speed_to_f64(fan.max_speed.as_deref()) {
                        self.emit_metric("fan_max_speed", Some(fan_name), value, "rpm", labels());
                        entity_count += 1;
                    }

                    self.emit_state_set(
                        "fan_state",
                        Some(fan_name),
                        fan_state_to_state(fan.state.as_deref()).as_str(),
                        FAN_STATE_STATES,
                        labels(),
                    );
                    entity_count += 1;
                }
            }
            Ok(None) => {}
            Err(e) => {
                fetch_failures += 1;
                saw_auth_failure |= is_auth_error(&e);
                tracing::warn!(
                error = ?e,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: failed to collect platform environment fan"
                );
            }
        }

        match self.client.get_platform_environment_temperature().await {
            Ok(Some(temps)) => {
                for (sensor_name, temp) in &temps {
                    // Each field is optional. Emit only those present and parseable.
                    let sensor_label = || vec![(Cow::Borrowed("sensor"), sensor_name.clone())];

                    if let Some(value) = temp_to_f64(temp.current.as_deref()) {
                        self.emit_metric(
                            "platform_temperature",
                            Some(sensor_name),
                            value,
                            "celsius",
                            sensor_label(),
                        );
                        entity_count += 1;
                    }
                    if let Some(value) = temp_to_f64(temp.max.as_deref()) {
                        self.emit_metric(
                            "platform_temperature_max",
                            Some(sensor_name),
                            value,
                            "celsius",
                            sensor_label(),
                        );
                        entity_count += 1;
                    }
                    if let Some(value) = temp_to_f64(temp.crit.as_deref()) {
                        self.emit_metric(
                            "platform_temperature_critical",
                            Some(sensor_name),
                            value,
                            "celsius",
                            sensor_label(),
                        );
                        entity_count += 1;
                    }
                    // Absent state emits nothing. Present state emits one 0/1 series per state.
                    if let Some(current) = temp_state_to_state(temp.state.as_deref()) {
                        self.emit_state_set(
                            "platform_temperature_state",
                            Some(sensor_name),
                            current,
                            TEMP_STATE_STATES,
                            sensor_label(),
                        );
                        entity_count += 1;
                    }
                }
            }
            Ok(None) => {}
            Err(e) => {
                fetch_failures += 1;
                saw_auth_failure |= is_auth_error(&e);
                tracing::warn!(
                error = ?e,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: failed to collect platform environment temperature"
                );
            }
        }

        match self.client.get_platform_environment_leakage().await {
            Ok(OptionalNvueResponse::Present(leakage)) => {
                entity_count += self.emit_leakage_data(&leakage);
            }
            Ok(OptionalNvueResponse::Null) => {
                self.emit_leakage_unavailable();
            }
            Ok(OptionalNvueResponse::Disabled) => {}
            Err(e) => {
                fetch_failures += 1;
                saw_auth_failure |= is_auth_error(&e);
                tracing::warn!(
                error = ?e,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: failed to collect platform environment leakage"
                );
            }
        }

        match self.client.get_platform_environment().await {
            Ok(Some(env)) => {
                // Switch-level FAN_STATUS LED. Emit only when present and mappable.
                if let Some(current) = env
                    .get("FAN_STATUS")
                    .and_then(|s| fan_led_to_state(s.state.as_deref()))
                {
                    self.emit_state_set("fan_led", None, current, FAN_LED_STATES, vec![]);
                    entity_count += 1;
                }
            }
            Ok(None) => {}
            Err(e) => {
                fetch_failures += 1;
                saw_auth_failure |= is_auth_error(&e);
                tracing::warn!(
                error = ?e,
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: failed to collect platform environment status"
                );
            }
        }

        if saw_auth_failure {
            tracing::warn!(
                switch_id = %self.switch_id,
                rack_id = self.event_context.rack_id().map(tracing::field::display),
                "nvue_rest: auth failure observed, clearing cached credentials"
            );
            self.client.clear_credentials();
        }

        self.emit_event(CollectorEvent::MetricCollectionEnd);

        tracing::debug!(
            switch_id = %self.switch_id,
            rack_id = self.event_context.rack_id().map(tracing::field::display),
            entity_count,
            "nvue_rest: collection iteration complete"
        );

        Ok(IterationResult {
            refresh_triggered: true,
            entity_count: Some(entity_count),
            fetch_failures,
        })
    }

    fn collector_type(&self) -> &'static str {
        COLLECTOR_NAME
    }

    async fn stop(&mut self) {
        self.emit_event(CollectorEvent::CollectorRemoved);
    }
}

impl NvueRestCollector {
    async fn refresh_rest_credentials(&self) -> Result<(), HealthError> {
        let creds = tokio::time::timeout(
            CREDENTIAL_REFRESH_TIMEOUT,
            self.provider.fetch_credentials(&self.addr),
        )
        .await
        .map_err(|_elapsed| {
            HealthError::GenericError(format!(
                "Timed out after {}s fetching NVUE REST credentials",
                CREDENTIAL_REFRESH_TIMEOUT.as_secs(),
            ))
        })??;
        match creds {
            BmcCredentials::UsernamePassword { username, password } => {
                self.client
                    .set_credentials(UsernamePassword { username, password });
                Ok(())
            }
            _ => Err(HealthError::GenericError(
                "NVUE REST collector requires username/password credentials".to_string(),
            )),
        }
    }

    /// Emits reboot-reason metadata through log sinks and as an info metric.
    ///
    /// `reason` is intentionally kept as the Prometheus grouping label because
    /// the metric is not useful without it. `gentime` and `user` are excluded
    /// from metric labels because they churn per event and can expose operator
    /// data, but remain available as structured log attributes. `switch_id`
    /// remains a log attribute so every sink can correlate the event.
    fn emit_reboot_reason_data(&self, reason: &RebootReasonResponse) {
        let reason_text = reason.reason.as_deref().unwrap_or("unknown");

        if self.log_event_sink_enabled {
            let gentime = reason.gentime.as_deref().unwrap_or("unknown");
            let user = reason.user.as_deref().unwrap_or("unknown");

            self.emit_event(CollectorEvent::Log(Box::new(LogRecord {
                body: "nvue_rest: collected system reboot reason".to_string(),
                severity: LogSeverity::Info,
                attributes: vec![
                    (
                        Cow::Borrowed("message_id"),
                        "NvueRest.SystemRebootReason".to_string(),
                    ),
                    (Cow::Borrowed("switch_id"), self.switch_id.clone()),
                    (Cow::Borrowed("reason"), reason_text.to_string()),
                    (Cow::Borrowed("gentime"), gentime.to_string()),
                    (Cow::Borrowed("user"), user.to_string()),
                ],
                diagnostic_record: None,
            })));
        }

        self.emit_metric(
            "reboot_reason_info",
            None,
            1.0,
            "info",
            vec![(Cow::Borrowed("reason"), reason_text.to_string())],
        );
    }

    fn emit_leakage_data(&self, leakage: &LeakageEnvironmentResponse) -> usize {
        let mut sensors = leakage.iter().collect::<Vec<_>>();
        sensors.sort_by(|left, right| left.0.cmp(right.0));

        for &(sensor_name, sensor) in &sensors {
            let current =
                leakage_state_to_state(sensor.as_ref().and_then(|sensor| sensor.state.as_deref()));

            self.emit_state_set(
                "leakage_state",
                Some(sensor_name.as_str()),
                current,
                LEAKAGE_STATES,
                vec![(Cow::Borrowed("sensor"), sensor_name.clone())],
            );
        }

        let report = self.build_leakage_report(sensors.as_slice());
        self.emit_event(CollectorEvent::HealthReport(Arc::new(report)));

        sensors.len()
    }

    /// Emits a switch-level alert when the leakage endpoint returns top-level
    /// JSON `null`.
    ///
    /// A concrete empty map means "no sensors reported" and is a source success;
    /// top-level `null` means the switch did not provide leakage data, so the
    /// previous leakage state must not be cleared as healthy.
    fn emit_leakage_unavailable(&self) {
        let report = HealthReport {
            source: ReportSource::NvueLeakage,
            target: Some(HealthReportTarget::Switch),
            observed_at: Some(chrono::Utc::now()),
            successes: Vec::new(),
            alerts: vec![HealthReportAlert {
                attribution: None,
                probe_id: Probe::NvueLeakage,
                target: None,
                message: "NVUE leakage data is unavailable".to_string(),
                classifications: vec![Classification::SensorFailure],
            }],
        };

        self.emit_event(CollectorEvent::HealthReport(Arc::new(report)));
    }

    /// Builds the switch-level health report for NVUE leakage sensors.
    ///
    /// Empty leakage data means the endpoint was reachable and no sensors were
    /// reported, so the source is healthy. Per-sensor `null` or unrecognized
    /// states alert as sensor failures; explicit `leak` states alert as leaks.
    fn build_leakage_report(
        &self,
        sensors: &[(&String, &Option<LeakageSensorData>)],
    ) -> HealthReport {
        let mut successes = Vec::new();
        let mut alerts = Vec::new();

        if sensors.is_empty() {
            successes.push(HealthReportSuccess {
                attribution: None,
                probe_id: Probe::NvueLeakage,
                target: None,
            });
        }

        for (sensor_name, sensor) in sensors {
            match leakage_state_to_state(sensor.as_ref().and_then(|sensor| sensor.state.as_deref()))
            {
                "ok" => successes.push(HealthReportSuccess {
                    attribution: None,
                    probe_id: Probe::NvueLeakage,
                    target: Some((*sensor_name).clone()),
                }),
                "leak" => alerts.push(HealthReportAlert {
                    attribution: None,
                    probe_id: Probe::NvueLeakage,
                    target: Some((*sensor_name).clone()),
                    message: format!("NVUE leakage sensor {sensor_name} reports leak"),
                    classifications: vec![Classification::Leak],
                }),
                _ => alerts.push(HealthReportAlert {
                    attribution: None,
                    probe_id: Probe::NvueLeakage,
                    target: Some((*sensor_name).clone()),
                    message: format!("NVUE leakage sensor {sensor_name} state is unknown"),
                    classifications: vec![Classification::SensorFailure],
                }),
            }
        }

        HealthReport {
            source: ReportSource::NvueLeakage,
            target: Some(HealthReportTarget::Switch),
            observed_at: Some(chrono::Utc::now()),
            successes,
            alerts,
        }
    }

    fn emit_event(&self, event: CollectorEvent) {
        if let Some(data_sink) = &self.data_sink {
            data_sink.handle_event(&self.event_context, &event);
        }
    }

    fn emit_metric(
        &self,
        metric_type: &str,
        entity_qualifier: Option<&str>,
        value: f64,
        unit: &str,
        labels: Vec<(Cow<'static, str>, String)>,
    ) {
        let key = match entity_qualifier {
            Some(q) => {
                let mut k = String::with_capacity(metric_type.len() + 1 + q.len());
                k.push_str(metric_type);
                k.push(':');
                k.push_str(q);
                k
            }
            None => metric_type.to_string(),
        };

        self.emit_event(CollectorEvent::Metric(
            MetricSample {
                key,
                name: COLLECTOR_NAME.to_string(),
                metric_type: metric_type.to_string(),
                unit: unit.to_string(),
                value,
                labels,
                context: None,
            }
            .into(),
        ));
    }

    /// Emit an OpenMetrics StateSet: one 0/1 series per state (current => 1.0),
    /// each carrying `labels` plus a `state` label. `key_base` is suffixed with
    /// the state name for a unique per-series key. Unit is always "state".
    fn emit_state_set(
        &self,
        metric_type: &str,
        key_base: Option<&str>,
        current_state: &str,
        all_states: &[&str],
        labels: Vec<(Cow<'static, str>, String)>,
    ) {
        for state in all_states {
            let mut series_labels = labels.clone();
            series_labels.push((Cow::Borrowed("state"), state.to_string()));

            // suffix state onto the qualifier for a unique per-series key
            // (switch-level series use the state name alone).
            let qualifier = match key_base {
                Some(base) => format!("{base}:{state}"),
                None => (*state).to_string(),
            };

            self.emit_metric(
                metric_type,
                Some(&qualifier),
                if *state == current_state { 1.0 } else { 0.0 },
                "state",
                series_labels,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::io::{Read, Write};
    use std::net::{IpAddr, Ipv4Addr};
    use std::str::FromStr;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread::JoinHandle;
    use std::time::Duration;

    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async, value_scenarios};
    use mac_address::MacAddress;

    use super::*;
    use crate::bmc::BoxFuture;
    use crate::config::NvueRestPaths;

    /// Assert StateSet semantics: one 0/1 series per state (current => 1.0),
    /// each with unit "state" and a `state` label. `entity` (if set) is present
    /// on every series.
    fn assert_state_set(
        samples: &[MetricSample],
        metric_type: &str,
        entity: Option<(&str, &str)>,
        all_states: &[&str],
        current: &str,
    ) {
        let series: Vec<&MetricSample> = samples
            .iter()
            .filter(|s| s.metric_type == metric_type)
            .collect();
        assert_eq!(
            series.len(),
            all_states.len(),
            "{metric_type}: expected one series per state"
        );
        for state in all_states {
            let sample = series
                .iter()
                .find(|s| s.labels.iter().any(|(k, v)| k == "state" && v == state))
                .unwrap_or_else(|| panic!("{metric_type}: missing series for state {state}"));
            assert_eq!(sample.unit, "state", "state {state}");
            assert_eq!(
                sample.value,
                if *state == current { 1.0 } else { 0.0 },
                "{metric_type} state {state}: value (current={current})"
            );
            if let Some((label, value)) = entity {
                assert!(
                    sample.labels.iter().any(|(k, v)| k == label && v == value),
                    "{metric_type} state {state}: missing entity label {label}={value}"
                );
            }
        }
    }

    #[derive(Default)]
    struct CapturingSink {
        samples: StdMutex<Vec<MetricSample>>,
        reports: StdMutex<Vec<HealthReport>>,
        logs: StdMutex<Vec<LogRecord>>,
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
            match event {
                CollectorEvent::Metric(sample) => {
                    self.samples.lock().unwrap().push((**sample).clone());
                }
                CollectorEvent::HealthReport(report) => {
                    self.reports.lock().unwrap().push((**report).clone());
                }
                CollectorEvent::Log(record) => {
                    self.logs.lock().unwrap().push((**record).clone());
                }
                CollectorEvent::MetricCollectionStart
                | CollectorEvent::MetricCollectionEnd
                | CollectorEvent::CollectorRemoved
                | CollectorEvent::Firmware(_) => {}
            }
            Ok(())
        }
    }

    #[test]
    fn test_system_health_mapping() {
        value_scenarios!(system_health_to_state:
            "known states" {
                Some("OK") => "ok",
                Some("Not OK") => "not_ok",
            }

            "unknown states" {
                None => "unknown",
                Some("unknown_value") => "unknown",
            }
        );
    }

    #[test]
    fn test_partition_health_mapping() {
        value_scenarios!(partition_health_to_state:
            "known states" {
                Some("healthy") => "healthy",
                Some("degraded_bandwidth") => "degraded_bandwidth",
                Some("degraded") => "degraded",
                Some("unhealthy") => "unhealthy",
            }

            "unknown states" {
                Some("unknown") => "unknown",
                None => "unknown",
            }
        );
    }

    #[test]
    fn test_app_status_mapping() {
        value_scenarios!(app_status_to_state:
            "known states" {
                Some("ok") => "ok",
                Some("not ok") => "not_ok",
            }

            "unknown states" {
                None => "unknown",
                Some("other") => "unknown",
            }
        );
    }

    #[test]
    fn test_diagnostic_opcode_mapping() {
        value_scenarios!(diagnostic_opcode_to_f64:
            "no issue" {
                "0" => 0.0,
            }

            "diagnostic issue" {
                "2" => 1.0,
                "1024" => 1.0,
                "57" => 1.0,
            }
        );
    }

    #[test]
    fn test_fan_max_speed_parsing() {
        value_scenarios!(fan_max_speed_to_f64:
            "valid speed" {
                Some("33000") => Some(33000.0),
                Some(" 33000 ") => Some(33000.0),
                Some("6000") => Some(6000.0),
            }

            "invalid speed" {
                Some("NaN") => None,
                Some("inf") => None,
                Some("-1") => None,
                Some("not-a-number") => None,
                Some("") => None,
                None => None,
            }
        );
    }

    #[test]
    fn test_fan_state_mapping() {
        value_scenarios!(fan_state_to_state:
            "healthy" {
                Some("ok") => FanState::Ok,
                Some(" OK ") => FanState::Ok,
            }

            "non-OK" {
                Some("warning") => FanState::NotOk,
                Some("critical") => FanState::NotOk,
            }

            "unknown or unavailable" {
                None => FanState::Unknown,
                Some("") => FanState::Unknown,
                Some("unknown") => FanState::Unknown,
                Some("unavailable") => FanState::Unknown,
                Some("N/A") => FanState::Unknown,
            }
        );
    }

    /// The emitted StateSet strings are a metric contract: pin the rendering of
    /// every variant and the domain built from them.
    #[test]
    fn test_fan_state_serializes_to_stable_names() {
        value_scenarios!(run = |state: FanState| state.to_string();
            "wire names" {
                FanState::Ok => "ok".to_string(),
                FanState::NotOk => "not_ok".to_string(),
                FanState::Unknown => "unknown".to_string(),
            }
        );

        assert_eq!(FAN_STATE_STATES, &["ok", "not_ok", "unknown"]);
    }

    #[test]
    fn test_temp_to_f64_parsing() {
        value_scenarios!(temp_to_f64:
            "valid temperature" {
                Some("105.00") => Some(105.0),
                Some(" 43 ") => Some(43.0),
                Some("120.00") => Some(120.0),
            }

            "invalid temperature" {
                Some("x") => None,
                Some("") => None,
                None => None,
            }
        );
    }

    #[test]
    fn test_leakage_state_mapping() {
        value_scenarios!(leakage_state_to_state:
            "no leak" {
                Some("ok") => "ok",
                Some("OK") => "ok",
                Some(" ok ") => "ok",
            }

            "leak detected" {
                Some("leak") => "leak",
                Some("LEAK") => "leak",
                Some(" leak ") => "leak",
            }

            "unknown state" {
                Some("missing") => "unknown",
                Some("   ") => "unknown",
                None => "unknown",
            }
        );
    }

    #[test]
    fn test_temp_state_to_state_mapping() {
        value_scenarios!(temp_state_to_state:
            "healthy state" {
                Some("ok") => Some("ok"),
                Some("OK") => Some("ok"),
                Some(" ok ") => Some("ok"),
            }

            "unhealthy state" {
                Some("warning") => Some("not_ok"),
                Some("") => Some("not_ok"),
            }

            "absent state" {
                None => None,
            }
        );
    }

    #[test]
    fn test_fan_led_to_state_mapping() {
        value_scenarios!(fan_led_to_state:
            "healthy state" {
                Some("green") => Some("ok"),
                Some("GREEN") => Some("ok"),
                Some(" green ") => Some("ok"),
                Some("ok") => Some("ok"),
                Some("OK") => Some("ok"),
            }

            "unhealthy state" {
                Some("amber") => Some("not_ok"),
                Some("red") => Some("not_ok"),
            }

            "absent state" {
                Some("") => None,
                Some("   ") => None,
                None => None,
            }
        );
    }

    #[test]
    fn test_reboot_reason_emits_log_metadata_and_limits_metric_labels_to_reason() {
        let sink = Arc::new(CapturingSink::default());
        let mut collector = collector_with_provider(ScriptedProvider::new(vec![]));
        collector.data_sink = Some(sink.clone());
        collector.log_event_sink_enabled = true;

        let reason = RebootReasonResponse {
            reason: Some("package upgrade".to_string()),
            gentime: Some("2026-07-05 12:34:56".to_string()),
            user: Some("admin".to_string()),
        };

        collector.emit_reboot_reason_data(&reason);

        let samples = sink.samples.lock().unwrap();
        let reports = sink.reports.lock().unwrap();
        let logs = sink.logs.lock().unwrap();
        let sample = samples.first().expect("reboot reason emits one sample");
        let log = logs.first().expect("reboot reason emits one log");

        assert_eq!(logs.len(), 1);
        assert_eq!(log.body, "nvue_rest: collected system reboot reason");
        assert_eq!(log.severity, LogSeverity::Info);

        assert_eq!(
            log.attributes,
            vec![
                (
                    Cow::Borrowed("message_id"),
                    "NvueRest.SystemRebootReason".to_string(),
                ),
                (Cow::Borrowed("switch_id"), "test-switch".to_string()),
                (Cow::Borrowed("reason"), "package upgrade".to_string()),
                (Cow::Borrowed("gentime"), "2026-07-05 12:34:56".to_string()),
                (Cow::Borrowed("user"), "admin".to_string()),
            ]
        );

        assert!(log.diagnostic_record.is_none());

        assert_eq!(samples.len(), 1);
        assert!(reports.is_empty());
        assert_eq!(sample.name, COLLECTOR_NAME);
        assert_eq!(sample.key, "reboot_reason_info");
        assert_eq!(sample.metric_type, "reboot_reason_info");
        assert_eq!(sample.unit, "info");
        assert_eq!(sample.value, 1.0);

        assert_eq!(
            sample.labels,
            vec![(Cow::Borrowed("reason"), "package upgrade".to_string())]
        );
    }

    #[test]
    fn test_reboot_reason_without_log_sink_emits_only_metric() {
        let sink = Arc::new(CapturingSink::default());
        let mut collector = collector_with_provider(ScriptedProvider::new(vec![]));
        collector.data_sink = Some(sink.clone());

        let reason = RebootReasonResponse {
            reason: Some("package upgrade".to_string()),
            gentime: Some("2026-07-05 12:34:56".to_string()),
            user: Some("admin".to_string()),
        };

        collector.emit_reboot_reason_data(&reason);

        let samples = sink.samples.lock().unwrap();
        let logs = sink.logs.lock().unwrap();

        assert_eq!(samples.len(), 1);
        assert!(logs.is_empty());
    }

    #[test]
    fn test_leakage_emits_metrics_and_health_report() {
        let sink = Arc::new(CapturingSink::default());
        let mut collector = collector_with_provider(ScriptedProvider::new(vec![]));
        collector.data_sink = Some(sink.clone());

        let leakage: LeakageEnvironmentResponse = serde_json::from_str(
            r#"{
                "LEAK1":{"state":"ok"},
                "LEAK2":{"state":"leak"},
                "LEAK3":{"state":"unknown"},
                "LEAK4": null
            }"#,
        )
        .expect("leakage json parses");

        let entity_count = collector.emit_leakage_data(&leakage);

        let samples = sink.samples.lock().unwrap();

        assert_eq!(entity_count, 4);
        assert_eq!(samples.len(), 12);

        let leak2_samples = samples
            .iter()
            .filter(|sample| {
                sample.metric_type == "leakage_state"
                    && sample
                        .labels
                        .iter()
                        .any(|(key, value)| key == "sensor" && value == "LEAK2")
            })
            .cloned()
            .collect::<Vec<_>>();

        assert_state_set(
            &leak2_samples,
            "leakage_state",
            Some(("sensor", "LEAK2")),
            LEAKAGE_STATES,
            "leak",
        );

        let reports = sink.reports.lock().unwrap();

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].source, ReportSource::NvueLeakage);
        assert_eq!(reports[0].target, Some(HealthReportTarget::Switch));
        assert_eq!(reports[0].successes.len(), 1);
        assert_eq!(reports[0].alerts.len(), 3);

        assert!(
            reports[0]
                .alerts
                .iter()
                .any(|alert| alert.classifications.contains(&Classification::Leak))
        );

        assert!(reports[0].alerts.iter().any(|alert| {
            alert
                .classifications
                .contains(&Classification::SensorFailure)
        }));
    }

    #[test]
    fn test_empty_leakage_emits_source_success_report() {
        let sink = Arc::new(CapturingSink::default());
        let mut collector = collector_with_provider(ScriptedProvider::new(vec![]));
        collector.data_sink = Some(sink.clone());

        let leakage: LeakageEnvironmentResponse =
            serde_json::from_str("{}").expect("empty leakage json parses");

        let entity_count = collector.emit_leakage_data(&leakage);

        let samples = sink.samples.lock().unwrap();

        assert_eq!(entity_count, 0);
        assert!(samples.is_empty());

        let reports = sink.reports.lock().unwrap();

        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].source, ReportSource::NvueLeakage);
        assert_eq!(reports[0].target, Some(HealthReportTarget::Switch));
        assert_eq!(reports[0].successes.len(), 1);
        assert_eq!(reports[0].successes[0].probe_id, Probe::NvueLeakage);
        assert_eq!(reports[0].successes[0].target, None);
        assert!(reports[0].alerts.is_empty());
    }

    struct ScriptedProvider {
        calls: AtomicUsize,
        // Each call pops the front. An empty queue yields an error. HealthError
        // isn't Clone, so we consume by value.
        responses: StdMutex<std::collections::VecDeque<Result<BmcCredentials, HealthError>>>,
    }

    impl ScriptedProvider {
        fn new(responses: Vec<Result<BmcCredentials, HealthError>>) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                responses: StdMutex::new(responses.into_iter().collect()),
            })
        }
    }

    impl CredentialProvider for ScriptedProvider {
        fn fetch_credentials<'a>(
            &'a self,
            _endpoint: &'a BmcAddr,
        ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let response = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| {
                    Err(HealthError::GenericError(
                        "scripted provider exhausted".to_string(),
                    ))
                });
            Box::pin(async move { response })
        }
    }

    fn test_addr() -> BmcAddr {
        BmcAddr {
            ip: IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            port: Some(443),
            mac: MacAddress::from_str("aa:bb:cc:dd:ee:ff").unwrap(),
        }
    }

    fn paths_all_disabled() -> NvueRestPaths {
        NvueRestPaths {
            system_health_enabled: false,
            system_reboot_reason_enabled: false,
            cluster_apps_enabled: false,
            sdn_partitions_enabled: false,
            interfaces_enabled: false,
            platform_environment_fan_enabled: false,
            platform_environment_temperature_enabled: false,
            platform_environment_leakage_enabled: false,
            platform_environment_status_enabled: false,
        }
    }

    fn collector_with_provider(provider: Arc<dyn CredentialProvider>) -> NvueRestCollector {
        let addr = test_addr();
        let client = RestClient::new(
            "test-switch".to_string(),
            addr.ip,
            addr.port,
            Duration::from_millis(10),
            true,
            None,
            paths_all_disabled(),
        )
        .expect("rest client builds");

        let event_context = EventContext {
            endpoint_key: "test-switch".to_string(),
            addr: addr.clone(),
            collector_type: COLLECTOR_NAME,
            metadata: None,
            rack_id: None,
            labels: Default::default(),
        };

        NvueRestCollector {
            client,
            switch_id: "test-switch".to_string(),
            event_context,
            data_sink: None,
            log_event_sink_enabled: false,
            addr,
            provider,
        }
    }

    #[derive(Clone, Copy)]
    enum IterationEndpoint {
        SystemHealth,
        RebootReason,
        ClusterApps,
        SdnPartitions,
        Interfaces,
        Fans,
        Temperatures,
        Leakage,
        Environment,
    }

    impl IterationEndpoint {
        fn paths(self) -> NvueRestPaths {
            let mut paths = paths_all_disabled();
            match self {
                Self::SystemHealth => paths.system_health_enabled = true,
                Self::RebootReason => paths.system_reboot_reason_enabled = true,
                Self::ClusterApps => paths.cluster_apps_enabled = true,
                Self::SdnPartitions => paths.sdn_partitions_enabled = true,
                Self::Interfaces => paths.interfaces_enabled = true,
                Self::Fans => paths.platform_environment_fan_enabled = true,
                Self::Temperatures => paths.platform_environment_temperature_enabled = true,
                Self::Leakage => paths.platform_environment_leakage_enabled = true,
                Self::Environment => paths.platform_environment_status_enabled = true,
            }
            paths
        }

        fn request_path(self) -> &'static str {
            match self {
                Self::SystemHealth => "/nvue_v1/system/health",
                Self::RebootReason => "/nvue_v1/system/reboot/reason",
                Self::ClusterApps => "/nvue_v1/cluster/apps",
                Self::SdnPartitions => "/nvue_v1/sdn/partition",
                Self::Interfaces => "/nvue_v1/interface",
                Self::Fans => "/nvue_v1/platform/environment/fan",
                Self::Temperatures => "/nvue_v1/platform/environment/temperature",
                Self::Leakage => "/nvue_v1/platform/environment/leakage",
                Self::Environment => "/nvue_v1/platform/environment",
            }
        }
    }

    fn spawn_json_response_server(
        endpoint: IterationEndpoint,
        status: u16,
        body: &'static str,
    ) -> (url::Url, JoinHandle<()>) {
        let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .expect("test server binds local port");

        let addr = listener.local_addr().expect("test server local addr");
        let base_url = url::Url::parse(&format!("http://{addr}")).expect("test server url parses");
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test server accepts request");
            let mut buffer = [0_u8; 2048];
            let bytes_read = stream.read(&mut buffer).expect("test server reads request");
            let request = String::from_utf8_lossy(&buffer[..bytes_read]);
            let request_line = request.lines().next().expect("request has a request line");
            assert!(
                request_line.starts_with(&format!("GET {}?", endpoint.request_path())),
                "expected request for {}, got {request_line}",
                endpoint.request_path(),
            );

            let reason = match status {
                200 => "OK",
                401 => "Unauthorized",
                500 => "Internal Server Error",
                _ => panic!("unsupported test response status {status}"),
            };

            let response = format!(
                "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                body.len(),
                body
            );

            stream
                .write_all(response.as_bytes())
                .expect("test server writes response");
        });

        (base_url, handle)
    }

    fn collector_with_json_response(
        endpoint: IterationEndpoint,
        status: u16,
        body: &'static str,
    ) -> (NvueRestCollector, Arc<CapturingSink>, JoinHandle<()>) {
        let (base_url, server) = spawn_json_response_server(endpoint, status, body);

        let client = RestClient::new_with_base_url_for_test(
            "test-switch".to_string(),
            base_url,
            Duration::from_secs(1),
            endpoint.paths(),
        )
        .expect("test rest client builds");

        client.set_credentials(UsernamePassword {
            username: "admin".to_string(),
            password: None,
        });

        let sink = Arc::new(CapturingSink::default());
        let mut collector = collector_with_provider(ScriptedProvider::new(vec![]));
        collector.client = client;
        collector.data_sink = Some(sink.clone());

        (collector, sink, server)
    }

    async fn collect_response(
        endpoint: IterationEndpoint,
        status: u16,
        body: &'static str,
    ) -> (IterationResult, bool, Vec<MetricSample>, Vec<HealthReport>) {
        let (mut collector, sink, server) = collector_with_json_response(endpoint, status, body);

        let result = collector
            .run_iteration()
            .await
            .expect("response iteration succeeds");

        server.join().expect("test server exits cleanly");

        let samples = sink.samples.lock().unwrap().clone();
        let reports = sink.reports.lock().unwrap().clone();
        let has_credentials = collector.client.has_credentials();

        (result, has_credentials, samples, reports)
    }

    async fn collect_null_response(
        endpoint: IterationEndpoint,
    ) -> (IterationResult, bool, Vec<MetricSample>, Vec<HealthReport>) {
        collect_response(endpoint, 200, "null").await
    }

    #[derive(Clone, Copy)]
    enum NullResponsePath {
        RebootReason,
        ClusterApps,
        Leakage,
    }

    impl NullResponsePath {
        fn endpoint(self) -> IterationEndpoint {
            match self {
                Self::RebootReason => IterationEndpoint::RebootReason,
                Self::ClusterApps => IterationEndpoint::ClusterApps,
                Self::Leakage => IterationEndpoint::Leakage,
            }
        }
    }

    #[derive(Debug, PartialEq)]
    struct NullResponseSummary {
        fetch_failures: usize,
        entity_count: Option<usize>,
        sample_count: usize,
        reports: Vec<ReportSummary>,
    }

    #[derive(Debug, PartialEq)]
    struct ReportSummary {
        source: ReportSource,
        target: Option<HealthReportTarget>,
        successes: Vec<(Probe, Option<String>)>,
        alerts: Vec<AlertSummary>,
    }

    #[derive(Debug, PartialEq)]
    struct AlertSummary {
        probe_id: Probe,
        target: Option<String>,
        classifications: Vec<Classification>,
        message: String,
    }

    #[derive(Debug, PartialEq)]
    struct SampleSummary {
        key: String,
        name: String,
        metric_type: String,
        unit: String,
        value: f64,
        labels: Vec<(String, String)>,
        has_context: bool,
    }

    #[derive(Debug, PartialEq)]
    struct IterationSummary {
        refresh_triggered: bool,
        entity_count: Option<usize>,
        fetch_failures: usize,
        has_credentials: bool,
        samples: Vec<SampleSummary>,
        reports: Vec<ReportSummary>,
    }

    #[derive(Clone, Copy)]
    struct IterationResponse {
        endpoint: IterationEndpoint,
        status: u16,
        body: &'static str,
    }

    fn sample_summary(
        metric_type: &str,
        qualifier: Option<&str>,
        value: f64,
        unit: &str,
        labels: &[(&str, &str)],
    ) -> SampleSummary {
        let key = qualifier
            .map(|qualifier| format!("{metric_type}:{qualifier}"))
            .unwrap_or_else(|| metric_type.to_string());
        let mut labels = labels
            .iter()
            .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
            .collect::<Vec<_>>();
        labels.sort();

        SampleSummary {
            key,
            name: COLLECTOR_NAME.to_string(),
            metric_type: metric_type.to_string(),
            unit: unit.to_string(),
            value,
            labels,
            has_context: false,
        }
    }

    fn state_set_summaries(
        metric_type: &str,
        key_base: Option<&str>,
        current: &str,
        states: &[&str],
        labels: &[(&str, &str)],
    ) -> Vec<SampleSummary> {
        states
            .iter()
            .map(|state| {
                let qualifier = key_base
                    .map(|base| format!("{base}:{state}"))
                    .unwrap_or_else(|| (*state).to_string());
                let mut labels = labels.to_vec();
                labels.push(("state", *state));

                sample_summary(
                    metric_type,
                    Some(&qualifier),
                    if *state == current { 1.0 } else { 0.0 },
                    "state",
                    &labels,
                )
            })
            .collect()
    }

    fn summarize_samples(samples: Vec<MetricSample>) -> Vec<SampleSummary> {
        let mut summaries = samples
            .into_iter()
            .map(|sample| {
                let mut labels = sample
                    .labels
                    .into_iter()
                    .map(|(key, value)| (key.into_owned(), value))
                    .collect::<Vec<_>>();
                labels.sort();

                SampleSummary {
                    key: sample.key,
                    name: sample.name,
                    metric_type: sample.metric_type,
                    unit: sample.unit,
                    value: sample.value,
                    labels,
                    has_context: sample.context.is_some(),
                }
            })
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.key.cmp(&right.key));
        summaries
    }

    fn summarize_reports(reports: Vec<HealthReport>) -> Vec<ReportSummary> {
        reports
            .into_iter()
            .map(|report| ReportSummary {
                source: report.source,
                target: report.target,
                successes: report
                    .successes
                    .into_iter()
                    .map(|success| (success.probe_id, success.target))
                    .collect(),
                alerts: report
                    .alerts
                    .into_iter()
                    .map(|alert| AlertSummary {
                        probe_id: alert.probe_id,
                        target: alert.target,
                        classifications: alert.classifications,
                        message: alert.message,
                    })
                    .collect(),
            })
            .collect()
    }

    async fn summarize_iteration_response(
        response: IterationResponse,
    ) -> Result<IterationSummary, Infallible> {
        let (result, has_credentials, samples, reports) =
            collect_response(response.endpoint, response.status, response.body).await;

        Ok(IterationSummary {
            refresh_triggered: result.refresh_triggered,
            entity_count: result.entity_count,
            fetch_failures: result.fetch_failures,
            has_credentials,
            samples: summarize_samples(samples),
            reports: summarize_reports(reports),
        })
    }

    fn populated_iteration_summary(endpoint: IterationEndpoint) -> IterationSummary {
        let (entity_count, mut samples, reports) = match endpoint {
            IterationEndpoint::SystemHealth => (
                1,
                state_set_summaries("system_health", None, "not_ok", SYSTEM_HEALTH_STATES, &[]),
                vec![],
            ),
            IterationEndpoint::RebootReason => (
                1,
                vec![sample_summary(
                    "reboot_reason_info",
                    None,
                    1.0,
                    "info",
                    &[("reason", "package upgrade")],
                )],
                vec![],
            ),
            IterationEndpoint::ClusterApps => {
                let mut samples = state_set_summaries(
                    "cluster_app",
                    Some("nmx-controller"),
                    "ok",
                    APP_STATUS_STATES,
                    &[("app_name", "nmx-controller")],
                );
                samples.extend(state_set_summaries(
                    "cluster_app",
                    Some("nmx-telemetry"),
                    "not_ok",
                    APP_STATUS_STATES,
                    &[("app_name", "nmx-telemetry")],
                ));
                samples.extend(state_set_summaries(
                    "cluster_app",
                    Some("unknown-app"),
                    "unknown",
                    APP_STATUS_STATES,
                    &[("app_name", "unknown-app")],
                ));
                (3, samples, vec![])
            }
            IterationEndpoint::SdnPartitions => {
                let mut samples = state_set_summaries(
                    "partition_health",
                    Some("1"),
                    "healthy",
                    PARTITION_HEALTH_STATES,
                    &[("partition_id", "1"), ("partition_name", "Partition 1")],
                );
                samples.push(sample_summary(
                    "partition_gpu",
                    Some("1"),
                    8.0,
                    "count",
                    &[("partition_id", "1"), ("partition_name", "Partition 1")],
                ));
                samples.extend(state_set_summaries(
                    "partition_health",
                    Some("2"),
                    "degraded",
                    PARTITION_HEALTH_STATES,
                    &[("partition_id", "2"), ("partition_name", "2")],
                ));
                samples.push(sample_summary(
                    "partition_gpu",
                    Some("2"),
                    0.0,
                    "count",
                    &[("partition_id", "2"), ("partition_name", "2")],
                ));
                (2, samples, vec![])
            }
            IterationEndpoint::Interfaces => (
                2,
                vec![
                    sample_summary(
                        "link_diagnostic",
                        Some("swp1:0"),
                        0.0,
                        "state",
                        &[
                            ("interface_name", "swp1"),
                            ("opcode", "0"),
                            ("diagnostic_status", "ok"),
                        ],
                    ),
                    sample_summary(
                        "link_diagnostic",
                        Some("swp1:2"),
                        1.0,
                        "state",
                        &[
                            ("interface_name", "swp1"),
                            ("opcode", "2"),
                            ("diagnostic_status", "fault"),
                        ],
                    ),
                ],
                vec![],
            ),
            IterationEndpoint::Fans => {
                let mut samples = vec![sample_summary(
                    "fan_max_speed",
                    Some("FAN1/1"),
                    33000.0,
                    "rpm",
                    &[("fan_name", "FAN1/1")],
                )];
                samples.extend(state_set_summaries(
                    "fan_state",
                    Some("FAN1/1"),
                    "ok",
                    FAN_STATE_STATES,
                    &[("fan_name", "FAN1/1")],
                ));
                samples.extend(state_set_summaries(
                    "fan_state",
                    Some("FAN1/2"),
                    "not_ok",
                    FAN_STATE_STATES,
                    &[("fan_name", "FAN1/2")],
                ));
                samples.extend(state_set_summaries(
                    "fan_state",
                    Some("FAN1/3"),
                    "unknown",
                    FAN_STATE_STATES,
                    &[("fan_name", "FAN1/3")],
                ));
                (4, samples, vec![])
            }
            IterationEndpoint::Temperatures => {
                let mut samples = vec![
                    sample_summary(
                        "platform_temperature",
                        Some("ASIC1"),
                        43.0,
                        "celsius",
                        &[("sensor", "ASIC1")],
                    ),
                    sample_summary(
                        "platform_temperature_max",
                        Some("ASIC1"),
                        105.0,
                        "celsius",
                        &[("sensor", "ASIC1")],
                    ),
                    sample_summary(
                        "platform_temperature_critical",
                        Some("ASIC1"),
                        120.0,
                        "celsius",
                        &[("sensor", "ASIC1")],
                    ),
                ];
                samples.extend(state_set_summaries(
                    "platform_temperature_state",
                    Some("ASIC1"),
                    "ok",
                    TEMP_STATE_STATES,
                    &[("sensor", "ASIC1")],
                ));
                samples.push(sample_summary(
                    "platform_temperature",
                    Some("Ambient-MNG-Temp"),
                    27.0,
                    "celsius",
                    &[("sensor", "Ambient-MNG-Temp")],
                ));
                samples.extend(state_set_summaries(
                    "platform_temperature_state",
                    Some("Ambient-MNG-Temp"),
                    "not_ok",
                    TEMP_STATE_STATES,
                    &[("sensor", "Ambient-MNG-Temp")],
                ));
                (6, samples, vec![])
            }
            IterationEndpoint::Leakage => (
                1,
                state_set_summaries(
                    "leakage_state",
                    Some("LEAK1"),
                    "ok",
                    LEAKAGE_STATES,
                    &[("sensor", "LEAK1")],
                ),
                vec![ReportSummary {
                    source: ReportSource::NvueLeakage,
                    target: Some(HealthReportTarget::Switch),
                    successes: vec![(Probe::NvueLeakage, Some("LEAK1".to_string()))],
                    alerts: vec![],
                }],
            ),
            IterationEndpoint::Environment => (
                1,
                state_set_summaries("fan_led", None, "not_ok", FAN_LED_STATES, &[]),
                vec![],
            ),
        };
        samples.sort_by(|left, right| left.key.cmp(&right.key));

        IterationSummary {
            refresh_triggered: true,
            entity_count: Some(entity_count),
            fetch_failures: 0,
            has_credentials: true,
            samples,
            reports,
        }
    }

    fn failed_iteration_summary(has_credentials: bool) -> IterationSummary {
        IterationSummary {
            refresh_triggered: true,
            entity_count: Some(0),
            fetch_failures: 1,
            has_credentials,
            samples: vec![],
            reports: vec![],
        }
    }

    fn empty_iteration_summary() -> IterationSummary {
        IterationSummary {
            refresh_triggered: true,
            entity_count: Some(0),
            fetch_failures: 0,
            has_credentials: true,
            samples: vec![],
            reports: vec![],
        }
    }

    async fn summarize_null_response(
        path: NullResponsePath,
    ) -> Result<NullResponseSummary, Infallible> {
        let (result, _has_credentials, samples, reports) =
            collect_null_response(path.endpoint()).await;

        Ok(NullResponseSummary {
            fetch_failures: result.fetch_failures,
            entity_count: result.entity_count,
            sample_count: samples.len(),
            reports: summarize_reports(reports),
        })
    }

    #[tokio::test]
    async fn iteration_responses_follow_endpoint_semantics() {
        check_cases_async(
            [
                Case {
                    scenario: "system health response emits its state set",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::SystemHealth,
                        status: 200,
                        body: r#"{"status":"Not OK"}"#,
                    },
                    expect: Yields(populated_iteration_summary(IterationEndpoint::SystemHealth)),
                },
                Case {
                    scenario: "reboot response emits reason metadata",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::RebootReason,
                        status: 200,
                        body: r#"{
                            "reason":"package upgrade",
                            "gentime":"2026-07-05 12:34:56",
                            "user":"admin"
                        }"#,
                    },
                    expect: Yields(populated_iteration_summary(IterationEndpoint::RebootReason)),
                },
                Case {
                    scenario: "application response emits every application state",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::ClusterApps,
                        status: 200,
                        body: r#"{
                            "nmx-controller":{"status":"ok"},
                            "nmx-telemetry":{"status":"not ok"},
                            "unknown-app":{}
                        }"#,
                    },
                    expect: Yields(populated_iteration_summary(IterationEndpoint::ClusterApps)),
                },
                Case {
                    scenario: "partition response emits health and GPU count",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::SdnPartitions,
                        status: 200,
                        body: r#"{
                            "1":{
                                "name":"Partition 1",
                                "health":"healthy",
                                "num-gpus":"8"
                            },
                            "2":{"health":"degraded"}
                        }"#,
                    },
                    expect: Yields(populated_iteration_summary(
                        IterationEndpoint::SdnPartitions,
                    )),
                },
                Case {
                    scenario: "interface response emits each link diagnostic",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Interfaces,
                        status: 200,
                        body: r#"{
                            "swp1":{
                                "type":"nvl",
                                "link":{
                                    "diagnostics":{
                                        "0":{"status":"ok"},
                                        "2":{"status":"fault"}
                                    }
                                }
                            },
                            "swp2":{"type":"nvl","link":{}}
                        }"#,
                    },
                    expect: Yields(populated_iteration_summary(IterationEndpoint::Interfaces)),
                },
                Case {
                    scenario: "fan response emits parseable speed and all fan states",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Fans,
                        status: 200,
                        body: r#"{
                            "FAN1/1":{"max-speed":"33000","state":"ok"},
                            "FAN1/2":{"max-speed":"bogus","state":"warning"},
                            "FAN1/3":{}
                        }"#,
                    },
                    expect: Yields(populated_iteration_summary(IterationEndpoint::Fans)),
                },
                Case {
                    scenario: "temperature response emits complete and sparse sensors",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Temperatures,
                        status: 200,
                        body: r#"{
                            "ASIC1":{
                                "crit":"120.00",
                                "current":"43.00",
                                "max":"105.00",
                                "state":"ok"
                            },
                            "Ambient-MNG-Temp":{
                                "current":"27.00",
                                "state":"warning"
                            },
                            "Invalid":{
                                "crit":"bad",
                                "current":"bad",
                                "max":"bad"
                            }
                        }"#,
                    },
                    expect: Yields(populated_iteration_summary(IterationEndpoint::Temperatures)),
                },
                Case {
                    scenario: "leakage response emits sensor state and report",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Leakage,
                        status: 200,
                        body: r#"{"LEAK1":{"state":"ok"}}"#,
                    },
                    expect: Yields(populated_iteration_summary(IterationEndpoint::Leakage)),
                },
                Case {
                    scenario: "environment response emits FAN_STATUS",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Environment,
                        status: 200,
                        body: r#"{
                            "FAN_STATUS":{"state":"amber","type":"led"},
                            "PSU_STATUS":{"state":"green","type":"led"}
                        }"#,
                    },
                    expect: Yields(populated_iteration_summary(IterationEndpoint::Environment)),
                },
                Case {
                    scenario: "environment without FAN_STATUS emits nothing",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Environment,
                        status: 200,
                        body: r#"{"PSU_STATUS":{"state":"green","type":"led"}}"#,
                    },
                    expect: Yields(empty_iteration_summary()),
                },
                Case {
                    scenario: "system health auth error clears credentials",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::SystemHealth,
                        status: 401,
                        body: r#"{"error":"unauthorized"}"#,
                    },
                    expect: Yields(failed_iteration_summary(false)),
                },
                Case {
                    scenario: "reboot fetch error increments the failure count",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::RebootReason,
                        status: 500,
                        body: r#"{"error":"unavailable"}"#,
                    },
                    expect: Yields(failed_iteration_summary(true)),
                },
                Case {
                    scenario: "application fetch error increments the failure count",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::ClusterApps,
                        status: 500,
                        body: r#"{"error":"unavailable"}"#,
                    },
                    expect: Yields(failed_iteration_summary(true)),
                },
                Case {
                    scenario: "partition fetch error increments the failure count",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::SdnPartitions,
                        status: 500,
                        body: r#"{"error":"unavailable"}"#,
                    },
                    expect: Yields(failed_iteration_summary(true)),
                },
                Case {
                    scenario: "interface fetch error increments the failure count",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Interfaces,
                        status: 500,
                        body: r#"{"error":"unavailable"}"#,
                    },
                    expect: Yields(failed_iteration_summary(true)),
                },
                Case {
                    scenario: "fan fetch error increments the failure count",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Fans,
                        status: 500,
                        body: r#"{"error":"unavailable"}"#,
                    },
                    expect: Yields(failed_iteration_summary(true)),
                },
                Case {
                    scenario: "temperature fetch error increments the failure count",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Temperatures,
                        status: 500,
                        body: r#"{"error":"unavailable"}"#,
                    },
                    expect: Yields(failed_iteration_summary(true)),
                },
                Case {
                    scenario: "leakage fetch error increments the failure count",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Leakage,
                        status: 500,
                        body: r#"{"error":"unavailable"}"#,
                    },
                    expect: Yields(failed_iteration_summary(true)),
                },
                Case {
                    scenario: "environment fetch error increments the failure count",
                    input: IterationResponse {
                        endpoint: IterationEndpoint::Environment,
                        status: 500,
                        body: r#"{"error":"unavailable"}"#,
                    },
                    expect: Yields(failed_iteration_summary(true)),
                },
            ],
            summarize_iteration_response,
        )
        .await;
    }

    #[tokio::test]
    async fn first_iteration_lazy_fetches_credentials_then_runs() {
        let provider = ScriptedProvider::new(vec![Ok(BmcCredentials::UsernamePassword {
            username: "admin".to_string(),
            password: Some("hunter2".to_string()),
        })]);
        let mut collector = collector_with_provider(provider.clone());

        assert!(
            !collector.client.has_credentials(),
            "client must start credential-less so sharded-out endpoints never trigger a fetch"
        );

        let result = collector
            .run_iteration()
            .await
            .expect("iteration returns Ok even when all paths are disabled");

        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert!(collector.client.has_credentials());
        assert_eq!(
            result.fetch_failures, 0,
            "all paths disabled → no HTTP, no failures"
        );
        // Subsequent iterations reuse the already-installed credentials.
        collector
            .run_iteration()
            .await
            .expect("second iteration ok");
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            1,
            "credential provider must not be re-hit while creds are still valid"
        );
    }

    #[tokio::test]
    async fn iteration_is_skipped_when_credential_fetch_fails_and_recovers_next_time() {
        let provider = ScriptedProvider::new(vec![
            Err(HealthError::GenericError("forge unavailable".to_string())),
            Ok(BmcCredentials::UsernamePassword {
                username: "admin".to_string(),
                password: None,
            }),
        ]);
        let mut collector = collector_with_provider(provider.clone());

        let first = collector.run_iteration().await.expect("first iteration ok");
        assert_eq!(first.fetch_failures, 1, "credential fetch failure surfaces");
        assert!(!first.refresh_triggered);
        assert!(
            !collector.client.has_credentials(),
            "failed fetch must NOT install bogus credentials"
        );

        let second = collector
            .run_iteration()
            .await
            .expect("second iteration ok");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert!(collector.client.has_credentials());
        assert_eq!(
            second.fetch_failures, 0,
            "second iteration recovers — credentials now present, no GETs to fail"
        );
    }

    #[tokio::test]
    async fn refresh_rejects_session_token_credentials() {
        let provider = ScriptedProvider::new(vec![Ok(BmcCredentials::SessionToken {
            token: "irrelevant".to_string(),
        })]);
        let collector = collector_with_provider(provider);

        let error = collector
            .refresh_rest_credentials()
            .await
            .expect_err("session-token credentials are not usable for NVUE basic auth");
        match error {
            HealthError::GenericError(msg) => assert!(
                msg.contains("requires username/password"),
                "expected explicit message, got: {msg}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[tokio::test]
    async fn null_responses_follow_endpoint_semantics() {
        check_cases_async(
            [
                Case {
                    scenario: "reboot reason null is unavailable metadata",
                    input: NullResponsePath::RebootReason,
                    expect: Yields(NullResponseSummary {
                        fetch_failures: 0,
                        entity_count: Some(0),
                        sample_count: 0,
                        reports: vec![],
                    }),
                },
                Case {
                    scenario: "metric path null counts fetch failure",
                    input: NullResponsePath::ClusterApps,
                    expect: Yields(NullResponseSummary {
                        fetch_failures: 1,
                        entity_count: Some(0),
                        sample_count: 0,
                        reports: vec![],
                    }),
                },
                Case {
                    scenario: "leakage null emits unavailable alert report",
                    input: NullResponsePath::Leakage,
                    expect: Yields(NullResponseSummary {
                        fetch_failures: 0,
                        entity_count: Some(0),
                        sample_count: 0,
                        reports: vec![ReportSummary {
                            source: ReportSource::NvueLeakage,
                            target: Some(HealthReportTarget::Switch),
                            successes: vec![],
                            alerts: vec![AlertSummary {
                                probe_id: Probe::NvueLeakage,
                                target: None,
                                classifications: vec![Classification::SensorFailure],
                                message: "NVUE leakage data is unavailable".to_string(),
                            }],
                        }],
                    }),
                },
            ],
            summarize_null_response,
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_rest_credentials_respects_timeout() {
        // Mirrors the `BmcClient::refresh_credentials_respects_timeout`
        // contract on the NVUE REST side: a hung Forge call must not block
        // the collector's iteration loop past `CREDENTIAL_REFRESH_TIMEOUT`.
        struct HangingProvider;
        impl CredentialProvider for HangingProvider {
            fn fetch_credentials<'a>(
                &'a self,
                _endpoint: &'a BmcAddr,
            ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
                Box::pin(std::future::pending())
            }
        }

        let collector = Arc::new(collector_with_provider(Arc::new(HangingProvider)));
        let refresh_collector = collector.clone();
        let refresh =
            tokio::spawn(async move { refresh_collector.refresh_rest_credentials().await });

        // Sleep just past the timeout so the tokio timer fires.
        tokio::time::advance(CREDENTIAL_REFRESH_TIMEOUT + Duration::from_secs(1)).await;
        let result = refresh.await.expect("task joined");
        let error = result.expect_err("hanging provider must surface as timeout");
        match error {
            HealthError::GenericError(msg) => assert!(
                msg.contains("Timed out"),
                "expected timeout message, got: {msg}"
            ),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn debug_redacts_password() {
        let creds = UsernamePassword {
            username: "admin".to_string(),
            password: Some("hunter2".to_string()),
        };
        let rendered = format!("{creds:?}");
        assert!(
            !rendered.contains("hunter2"),
            "Debug must not leak the password; got: {rendered}"
        );
        assert!(rendered.contains("admin"));
        assert!(rendered.contains("<redacted>"));

        let no_password = UsernamePassword {
            username: "admin".to_string(),
            password: None,
        };
        let rendered = format!("{no_password:?}");
        assert!(
            !rendered.contains("<redacted>"),
            "missing password must not show as redacted; got: {rendered}"
        );
    }
}
