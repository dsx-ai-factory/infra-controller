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

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Duration;
use std::{fmt, io};

use carbide_instrument::Event;
use carbide_utils::metrics::SharedMetricsHolder;
use carbide_utils::periodic_timer::PeriodicTimer;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use chrono::Utc;
use component_manager::component_manager::{
    ComponentManager, RackMaintenanceEligibility, RackMaintenanceRequestOutcome,
    request_rack_maintenance_via_state_controller,
};
use db::db_read::PgPoolReader;
use db::work_lock_manager::WorkLockManagerHandle;
use model::machine::LoadSnapshotOptions;
use model::machine::machine_search_config::MachineSearchConfig;
use model::nmxc::NvlinkNmxcEndpoint;
use model::rack::{MaintenanceActivity, MaintenanceScope};
use opentelemetry::KeyValue;
use opentelemetry::metrics::Meter;
use rustls::{CertificateError, ClientConfig, Error as RustlsError, RootCertStore};
use rustls_pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;
use x509_parser::prelude::{FromDer, X509Certificate};

use crate::config::NvLinkConfig;
use crate::errors::{NvLinkManagerError, NvLinkManagerResult};
use crate::{managed_host_chassis_serial, nmx_c_endpoint};

#[derive(Clone, Debug, PartialEq, Eq)]
struct CertificateInfo {
    fingerprint_sha256: String,
    not_after_timestamp: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SwitchCertificateMonitorTargetSource {
    ReadyControlPlane { switch_id: SwitchId },
    LegacyChassisMapping { chassis_serials: Vec<String> },
}

impl SwitchCertificateMonitorTargetSource {
    fn name(&self) -> &'static str {
        match self {
            Self::ReadyControlPlane { .. } => "ready_control_plane_switch",
            Self::LegacyChassisMapping { .. } => "legacy_chassis_mapping",
        }
    }

    fn switch_id(&self) -> Option<&SwitchId> {
        match self {
            Self::ReadyControlPlane { switch_id } => Some(switch_id),
            Self::LegacyChassisMapping { .. } => None,
        }
    }

    fn chassis_serials(&self) -> &[String] {
        match self {
            Self::ReadyControlPlane { .. } => &[],
            Self::LegacyChassisMapping { chassis_serials } => chassis_serials,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SwitchCertificateMonitorTarget {
    rack_id: Option<RackId>,
    endpoint_url: String,
    source: SwitchCertificateMonitorTargetSource,
}

/// Adds alert-only targets from the legacy chassis mapping, while keeping
/// ready control-plane switch discovery authoritative.
fn merge_switch_certificate_monitor_targets(
    primary_targets: Vec<SwitchCertificateMonitorTarget>,
    legacy_endpoints: Vec<NvlinkNmxcEndpoint>,
) -> Vec<SwitchCertificateMonitorTarget> {
    let claimed_endpoints = primary_targets
        .iter()
        .map(|target| target.endpoint_url.clone())
        .collect::<HashSet<_>>();
    let mut targets = primary_targets;
    targets.reserve(legacy_endpoints.len());

    let mut chassis_serials_by_endpoint: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in legacy_endpoints {
        let endpoint_url = row.endpoint.trim().to_string();
        if claimed_endpoints.contains(&endpoint_url) {
            continue;
        }
        chassis_serials_by_endpoint
            .entry(endpoint_url)
            .or_default()
            .insert(row.chassis_serial);
    }

    targets.extend(chassis_serials_by_endpoint.into_iter().map(
        |(endpoint_url, chassis_serials)| SwitchCertificateMonitorTarget {
            rack_id: None,
            endpoint_url,
            source: SwitchCertificateMonitorTargetSource::LegacyChassisMapping {
                chassis_serials: chassis_serials.into_iter().collect(),
            },
        },
    ));
    targets
}

#[derive(Debug, PartialEq, Eq)]
enum CertificateProbeOutcome {
    Observed(CertificateInfo),
    Expired,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwitchCertApplyStatus {
    NotNeeded,
    Pending,
    Error,
    Skipped,
}

impl SwitchCertApplyStatus {
    fn as_metric_label(self) -> &'static str {
        match self {
            Self::NotNeeded => "not_needed",
            Self::Pending => "pending",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Debug)]
struct ObservedSwitchCertMetrics {
    probe_success: bool,
    rotation_required: bool,
    observed_cert: Option<CertificateInfo>,
    error: String,
    apply_status: SwitchCertApplyStatus,
    apply_error: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum SwitchCertMonitorErrorKind {
    Timeout,
    Connection,
    Tls,
    CertificateFile,
    CertificateParse,
    EndpointConfig,
    ServerCertificate,
    Configuration,
    Rms,
    Other,
}

impl SwitchCertMonitorErrorKind {
    fn as_metric_label(self) -> &'static str {
        match self {
            Self::Timeout => "timeout",
            Self::Connection => "connection",
            Self::Tls => "tls",
            Self::CertificateFile => "certificate_file",
            Self::CertificateParse => "certificate_parse",
            Self::EndpointConfig => "endpoint_config",
            Self::ServerCertificate => "server_certificate",
            Self::Configuration => "configuration",
            Self::Rms => "rms",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Debug)]
struct SwitchCertMonitorMetrics {
    recording_started_at: std::time::Instant,
    observed_certs: Vec<ObservedSwitchCertMetrics>,
}

impl SwitchCertMonitorMetrics {
    fn new() -> Self {
        Self {
            recording_started_at: std::time::Instant::now(),
            observed_certs: Vec::new(),
        }
    }
}

impl fmt::Display for SwitchCertMonitorMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let successful_probes = self
            .observed_certs
            .iter()
            .filter(|cert| cert.probe_success)
            .count();
        let certificates_needing_rotation = self
            .observed_certs
            .iter()
            .filter(|cert| cert.rotation_required)
            .count();
        let pending_updates = self
            .observed_certs
            .iter()
            .filter(|cert| cert.apply_status == SwitchCertApplyStatus::Pending)
            .count();
        write!(
            f,
            "{{ observed_endpoints: {}, successful_probes: {}, certificates_needing_rotation: {}, pending_updates: {}, duration: {} }}",
            self.observed_certs.len(),
            successful_probes,
            certificates_needing_rotation,
            pending_updates,
            self.recording_started_at.elapsed().as_millis(),
        )
    }
}

/// One switch-certificate monitor pass. Both cases sample the duration; only a
/// failure logs.
#[derive(Event)]
#[event(
    event_name = "nvlink_switch_certificate_monitor_iteration_finished",
    metric_name = "carbide_nvlink_switch_cert_monitor_iteration_latency_milliseconds",
    component = "nvlink-manager",
    metric = histogram,
    describe = "Time consumed for one NMX-C switch certificate monitor iteration"
)]
enum SwitchCertificateMonitorIterationFinished {
    /// A clean pass: sampled, never logged.
    #[event(log = off)]
    Succeeded {
        #[observation]
        latency_ms: f64,
    },

    #[event(log = warn, message = "Switch certificate monitor error")]
    Failed {
        #[observation]
        latency_ms: f64,
        #[context]
        error: String,
    },
}

struct SwitchCertMonitorInstruments;

impl SwitchCertMonitorInstruments {
    fn register(meter: Meter, shared_metrics: SharedMetricsHolder<SwitchCertMonitorMetrics>) {
        {
            let metrics = shared_metrics.clone();
            meter
                .i64_observable_gauge(
                    "carbide_nvlink_switch_cert_monitor_observed_cert_expiration_time",
                )
                .with_description(
                    "Earliest expiration time (epoch seconds) for certificates served by NMX-C, by status",
                )
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        let mut expirations_by_status = BTreeMap::new();
                        for cert in &metrics.observed_certs {
                            if let Some(observed_cert) = &cert.observed_cert {
                                let entry = expirations_by_status
                                    .entry(rotation_window_status(cert))
                                    .or_insert(observed_cert.not_after_timestamp);
                                *entry = (*entry).min(observed_cert.not_after_timestamp);
                            }
                        }

                        for (status, not_after) in expirations_by_status {
                            observer.observe(
                                not_after,
                                &metric_attrs(attrs, &[KeyValue::new("status", status)]),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_switch_cert_monitor_probe_success")
                .with_description("Number of NMX-C TLS certificate probes by status")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (status, count) in
                            count_by_status(&metrics.observed_certs, probe_status)
                        {
                            observer.observe(
                                count,
                                &metric_attrs(attrs, &[KeyValue::new("status", status)]),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_switch_cert_monitor_expiring_soon")
                .with_description("Number of NMX-C certificates by rotation-window status")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (status, count) in
                            count_by_status(&metrics.observed_certs, rotation_window_status)
                        {
                            observer.observe(
                                count,
                                &metric_attrs(attrs, &[KeyValue::new("status", status)]),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_switch_cert_monitor_apply_status")
                .with_description("Number of NMX-C switch certificate apply outcomes by status")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        for (status, count) in
                            count_by_status(&metrics.observed_certs, apply_status)
                        {
                            observer.observe(
                                count,
                                &metric_attrs(attrs, &[KeyValue::new("status", status)]),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics.clone();
            meter
                .u64_observable_gauge("carbide_nvlink_switch_cert_monitor_apply_error_count")
                .with_description("Number of NMX-C switch certificate apply failures by error kind")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        let error_counts = count_errors_by_kind(
                            metrics
                                .observed_certs
                                .iter()
                                .map(|cert| cert.apply_error.as_str()),
                        );
                        for (error_kind, count) in error_counts {
                            observer.observe(
                                count,
                                &metric_attrs(
                                    attrs,
                                    &[
                                        KeyValue::new("status", "error"),
                                        KeyValue::new("error_kind", error_kind.as_metric_label()),
                                    ],
                                ),
                            );
                        }
                    })
                })
                .build();
        }

        {
            let metrics = shared_metrics;
            meter
                .u64_observable_gauge("carbide_nvlink_switch_cert_monitor_probe_error_count")
                .with_description("Number of NMX-C endpoint probe failures by error kind")
                .with_callback(move |observer| {
                    metrics.if_available(|metrics, attrs| {
                        let error_counts = count_errors_by_kind(
                            metrics
                                .observed_certs
                                .iter()
                                .map(|cert| cert.error.as_str()),
                        );
                        for (error_kind, count) in error_counts {
                            observer.observe(
                                count,
                                &metric_attrs(
                                    attrs,
                                    &[
                                        KeyValue::new("status", "error"),
                                        KeyValue::new("error_kind", error_kind.as_metric_label()),
                                    ],
                                ),
                            );
                        }
                    })
                })
                .build();
        }
    }
}

struct MetricHolder {
    last_iteration_metrics: SharedMetricsHolder<SwitchCertMonitorMetrics>,
}

impl MetricHolder {
    fn new(meter: Meter, hold_period: Duration) -> Self {
        let last_iteration_metrics = SharedMetricsHolder::with_hold_period(hold_period);
        SwitchCertMonitorInstruments::register(meter, last_iteration_metrics.clone());
        Self {
            last_iteration_metrics,
        }
    }

    fn update_metrics(&self, metrics: SwitchCertMonitorMetrics) {
        self.last_iteration_metrics.update(metrics);
    }
}

#[cfg(not(feature = "test-support"))]
pub(crate) struct SwitchCertificateMonitor {
    db_pool: PgPool,
    config: NvLinkConfig,
    component_manager: Option<Arc<ComponentManager>>,
    metric_holder: Arc<MetricHolder>,
    work_lock_manager_handle: WorkLockManagerHandle,
}

#[cfg(feature = "test-support")]
pub struct SwitchCertificateMonitor {
    db_pool: PgPool,
    config: NvLinkConfig,
    component_manager: Option<Arc<ComponentManager>>,
    metric_holder: Arc<MetricHolder>,
    work_lock_manager_handle: WorkLockManagerHandle,
}

#[cfg(feature = "test-support")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwitchCertificateMonitorIterationResult {
    pub observed_endpoints: usize,
    pub successful_probes: usize,
    pub certificates_needing_rotation: usize,
    pub probe_errors: usize,
    pub applied_updates: usize,
    pub pending_updates: usize,
    pub apply_errors: usize,
}

#[cfg(feature = "test-support")]
impl SwitchCertificateMonitorIterationResult {
    fn from_metrics(metrics: &SwitchCertMonitorMetrics) -> Self {
        Self {
            observed_endpoints: metrics.observed_certs.len(),
            successful_probes: metrics
                .observed_certs
                .iter()
                .filter(|cert| cert.probe_success)
                .count(),
            certificates_needing_rotation: metrics
                .observed_certs
                .iter()
                .filter(|cert| cert.rotation_required)
                .count(),
            probe_errors: metrics
                .observed_certs
                .iter()
                .filter(|cert| !cert.error.is_empty())
                .count(),
            applied_updates: 0,
            pending_updates: metrics
                .observed_certs
                .iter()
                .filter(|cert| cert.apply_status == SwitchCertApplyStatus::Pending)
                .count(),
            apply_errors: metrics
                .observed_certs
                .iter()
                .filter(|cert| !cert.apply_error.is_empty())
                .count(),
        }
    }
}

impl SwitchCertificateMonitor {
    const ITERATION_WORK_KEY: &'static str = "SwitchCertificateMonitor::run_single_iteration";
    const PROBE_CANCELLED_ERROR: &'static str = "NMX-C server certificate probe cancelled";
    const APPLY_CANCELLED_ERROR: &'static str = "NMX-C server certificate apply cancelled";

    #[cfg(not(feature = "test-support"))]
    pub(crate) fn new(
        db_pool: PgPool,
        meter: Meter,
        config: NvLinkConfig,
        component_manager: Option<Arc<ComponentManager>>,
        work_lock_manager_handle: WorkLockManagerHandle,
    ) -> Self {
        Self::new_inner(
            db_pool,
            meter,
            config,
            component_manager,
            work_lock_manager_handle,
        )
    }

    #[cfg(feature = "test-support")]
    pub fn new(
        db_pool: PgPool,
        meter: Meter,
        config: NvLinkConfig,
        component_manager: Option<Arc<ComponentManager>>,
        work_lock_manager_handle: WorkLockManagerHandle,
    ) -> Self {
        Self::new_inner(
            db_pool,
            meter,
            config,
            component_manager,
            work_lock_manager_handle,
        )
    }

    fn new_inner(
        db_pool: PgPool,
        meter: Meter,
        config: NvLinkConfig,
        component_manager: Option<Arc<ComponentManager>>,
        work_lock_manager_handle: WorkLockManagerHandle,
    ) -> Self {
        let hold_period = config
            .nmx_c_certificate_rotation
            .run_interval
            .saturating_add(std::time::Duration::from_secs(60));
        let metric_holder = Arc::new(MetricHolder::new(meter, hold_period));
        Self {
            db_pool,
            config,
            component_manager,
            metric_holder,
            work_lock_manager_handle,
        }
    }

    pub(crate) async fn run(&self, cancel_token: CancellationToken) {
        let timer = PeriodicTimer::new(self.config.nmx_c_certificate_rotation.run_interval);
        loop {
            let tick = timer.tick();
            // The iteration helper owns the completion event, including the
            // historical `WARN`, before it returns to this scheduling loop.
            self.run_single_iteration_impl(&cancel_token, |_| ())
                .await
                .ok();

            tokio::select! {
                _ = tick.sleep() => {},
                _ = cancel_token.cancelled() => {
                    tracing::info!("SwitchCertificateMonitor stop was requested");
                    return;
                }
            }
        }
    }

    #[cfg(feature = "test-support")]
    pub async fn run_single_iteration(
        &self,
        cancel_token: &CancellationToken,
    ) -> NvLinkManagerResult<SwitchCertificateMonitorIterationResult> {
        self.run_single_iteration_impl(
            cancel_token,
            SwitchCertificateMonitorIterationResult::from_metrics,
        )
        .await
    }

    async fn run_single_iteration_impl<T>(
        &self,
        cancel_token: &CancellationToken,
        make_result: impl FnOnce(&SwitchCertMonitorMetrics) -> T,
    ) -> NvLinkManagerResult<T> {
        let mut metrics = SwitchCertMonitorMetrics::new();
        let span_id: String = format!("{:#x}", u64::from_le_bytes(rand::random::<[u8; 8]>()));
        let switch_cert_monitor_span = tracing::span!(
            parent: None,
            tracing::Level::INFO,
            "nmx_c_switch_cert_monitor",
            span_id,
            otel.status_code = tracing::field::Empty,
            otel.status_message = tracing::field::Empty,
            metrics = tracing::field::Empty,
        );
        let result = self
            .run_single_iteration_inner(&mut metrics, cancel_token)
            .instrument(switch_cert_monitor_span.clone())
            .await;
        switch_cert_monitor_span.record(
            "otel.status_code",
            if result.is_ok() { "ok" } else { "error" },
        );
        if let Err(ref e) = result {
            switch_cert_monitor_span.record("otel.status_message", format!("{e:?}"));
        }
        switch_cert_monitor_span.record("metrics", metrics.to_string());
        let iteration_result = make_result(&metrics);
        switch_cert_monitor_span.in_scope(|| {
            carbide_instrument::emit(match result.as_ref().err() {
                None => SwitchCertificateMonitorIterationFinished::Succeeded {
                    latency_ms: metrics.recording_started_at.elapsed().as_millis() as f64,
                },
                Some(error) => SwitchCertificateMonitorIterationFinished::Failed {
                    latency_ms: metrics.recording_started_at.elapsed().as_millis() as f64,
                    error: error.to_string(),
                },
            });
        });
        self.metric_holder.update_metrics(metrics);
        result.map(|_| iteration_result)
    }

    async fn run_single_iteration_inner(
        &self,
        metrics: &mut SwitchCertMonitorMetrics,
        cancel_token: &CancellationToken,
    ) -> NvLinkManagerResult<()> {
        let _lock = match self
            .work_lock_manager_handle
            .try_acquire_lock(Self::ITERATION_WORK_KEY.into())
            .await
        {
            Ok(lock) => lock,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "SwitchCertificateMonitor failed to acquire work lock: Another instance of carbide running?",
                );
                return Ok(());
            }
        };

        let targets = self.load_switch_certificate_monitor_targets().await?;

        for target in targets {
            let rack_id_label = target
                .rack_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_else(|| "unassociated".to_string());
            let switch_id_label = target
                .source
                .switch_id()
                .map(ToString::to_string)
                .unwrap_or_else(|| "unavailable".to_string());
            let target_source = target.source.name();
            let chassis_serials = target.source.chassis_serials().join(",");

            let observed_cert = tokio::select! {
                _ = cancel_token.cancelled() => {
                    tracing::info!("SwitchCertificateMonitor stop was requested");
                    return Ok(());
                }
                observed_cert = self.probe_endpoint_certificate(&target.endpoint_url, cancel_token) => {
                    observed_cert
                }
            };

            let (observed_cert, rotation_required, probe_success, probe_error) = match observed_cert
            {
                Ok(CertificateProbeOutcome::Observed(observed_cert)) => {
                    let rotation_required = cert_expires_within(
                        &observed_cert,
                        self.config.nmx_c_certificate_rotation.rotate_before_expiry,
                    );

                    if rotation_required {
                        tracing::warn!(
                            switch_id = %switch_id_label,
                            rack_id = %rack_id_label,
                            endpoint = %target.endpoint_url,
                            target_source,
                            chassis_serials,
                            observed_fingerprint = %observed_cert.fingerprint_sha256,
                            observed_not_after = observed_cert.not_after_timestamp,
                            rotate_before_expiry_seconds = self
                                .config
                                .nmx_c_certificate_rotation
                                .rotate_before_expiry
                                .as_secs(),
                            "NMX-C server certificate is due for rotation"
                        );
                    }

                    tracing::debug!(
                        switch_id = %switch_id_label,
                        rack_id = %rack_id_label,
                        endpoint = %target.endpoint_url,
                        target_source,
                        chassis_serials,
                        observed_not_after = observed_cert.not_after_timestamp,
                        observed_fingerprint = %observed_cert.fingerprint_sha256,
                        rotation_required,
                        "Observed NMX-C server certificate"
                    );
                    (Some(observed_cert), rotation_required, true, String::new())
                }
                Ok(CertificateProbeOutcome::Expired) => {
                    tracing::warn!(
                        switch_id = %switch_id_label,
                        rack_id = %rack_id_label,
                        endpoint = %target.endpoint_url,
                        target_source,
                        chassis_serials,
                        "NMX-C server certificate is expired"
                    );
                    (
                        None,
                        true,
                        false,
                        "TLS handshake failed because the NMX-C server certificate is expired"
                            .to_string(),
                    )
                }
                Err(error) if error == Self::PROBE_CANCELLED_ERROR => {
                    tracing::info!("SwitchCertificateMonitor stop was requested");
                    return Ok(());
                }
                Err(error) => {
                    tracing::warn!(
                        switch_id = %switch_id_label,
                        rack_id = %rack_id_label,
                        endpoint = %target.endpoint_url,
                        target_source,
                        chassis_serials,
                        error = %error,
                        "Failed to probe NMX-C server certificate"
                    );
                    metrics.observed_certs.push(ObservedSwitchCertMetrics {
                        probe_success: false,
                        rotation_required: false,
                        observed_cert: None,
                        error,
                        apply_status: SwitchCertApplyStatus::Skipped,
                        apply_error: String::new(),
                    });
                    continue;
                }
            };

            let (apply_status, apply_error) = if rotation_required {
                match self
                    .request_nmx_cluster_configuration_with_rack_state_controller(
                        &target,
                        cancel_token,
                    )
                    .await
                {
                    Ok(apply_status) => (apply_status, String::new()),
                    Err(error) if error == Self::APPLY_CANCELLED_ERROR => {
                        tracing::info!("SwitchCertificateMonitor stop was requested");
                        return Ok(());
                    }
                    Err(error) => {
                        tracing::warn!(
                            switch_id = %switch_id_label,
                            rack_id = %rack_id_label,
                            endpoint = %target.endpoint_url,
                            target_source,
                            chassis_serials,
                            error = %error,
                            "Failed to request NMX-C cluster configuration via rack state machine"
                        );
                        (SwitchCertApplyStatus::Error, error)
                    }
                }
            } else {
                (SwitchCertApplyStatus::NotNeeded, String::new())
            };

            metrics.observed_certs.push(ObservedSwitchCertMetrics {
                probe_success,
                rotation_required,
                observed_cert,
                error: probe_error,
                apply_status,
                apply_error,
            });
        }

        Ok(())
    }

    async fn load_switch_certificate_monitor_targets(
        &self,
    ) -> NvLinkManagerResult<Vec<SwitchCertificateMonitorTarget>> {
        let mut db_reader = PgPoolReader::from(self.db_pool.clone());
        let endpoint_rows =
            db::switch::find_ready_control_plane_configured_switch_endpoints(&mut db_reader)
                .await
                .map_err(NvLinkManagerError::from)?;

        let primary_targets = endpoint_rows
            .into_iter()
            .map(|row| SwitchCertificateMonitorTarget {
                rack_id: Some(row.rack_id),
                endpoint_url: nmx_c_endpoint::nmx_c_endpoint_url_from_nvos_ip(
                    &row.nvos_ip,
                    None,
                    &self.config,
                ),
                source: SwitchCertificateMonitorTargetSource::ReadyControlPlane {
                    switch_id: row.switch_id,
                },
            })
            .collect::<Vec<_>>();

        let primary_rack_ids = primary_targets
            .iter()
            .filter_map(|target| target.rack_id.clone())
            .collect::<HashSet<_>>();
        let legacy_endpoints_result = async {
            let machine_ids = db::machine::find_machine_ids(
                &mut db_reader,
                MachineSearchConfig {
                    mnnvl_only: true,
                    include_predicted_host: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(NvLinkManagerError::from)?;
            let snapshots = db::managed_host::load_by_machine_ids(
                &mut db_reader,
                &machine_ids,
                LoadSnapshotOptions {
                    include_instance_data: false,
                    ..Default::default()
                },
            )
            .await
            .map_err(NvLinkManagerError::from)?;

            // Do not consult the legacy mapping for a chassis that already belongs to a rack
            // with an authoritative ready control-plane endpoint. This avoids probing a stale
            // legacy URL alongside its replacement.
            let mut chassis_serials = BTreeSet::new();
            let mut chassis_serials_with_primary_targets = HashSet::new();
            for snapshot in snapshots.values() {
                let Some(chassis_serial) = managed_host_chassis_serial(snapshot) else {
                    continue;
                };
                chassis_serials.insert(chassis_serial.clone());
                if snapshot
                    .host_snapshot
                    .rack_id
                    .as_ref()
                    .is_some_and(|rack_id| primary_rack_ids.contains(rack_id))
                {
                    chassis_serials_with_primary_targets.insert(chassis_serial);
                }
            }
            chassis_serials.retain(|serial| !chassis_serials_with_primary_targets.contains(serial));

            let chassis_serial_refs = chassis_serials
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            db::nvlink_nmxc_endpoints::find_by_chassis_serials(&mut db_reader, &chassis_serial_refs)
                .await
                .map_err(NvLinkManagerError::from)
        }
        .await;

        match legacy_endpoints_result {
            Ok(legacy_endpoints) => Ok(merge_switch_certificate_monitor_targets(
                primary_targets,
                legacy_endpoints,
            )),
            Err(error) if !primary_targets.is_empty() => {
                tracing::warn!(
                    error = %error,
                    "Failed to load legacy chassis-mapped NMX-C certificate monitor targets; continuing with ready control-plane switches"
                );
                Ok(primary_targets)
            }
            Err(error) => Err(error),
        }
    }

    async fn request_nmx_cluster_configuration_with_rack_state_controller(
        &self,
        target: &SwitchCertificateMonitorTarget,
        cancel_token: &CancellationToken,
    ) -> Result<SwitchCertApplyStatus, String> {
        let Some(rack_id) = target.rack_id.as_ref() else {
            tracing::warn!(
                endpoint = %target.endpoint_url,
                target_source = target.source.name(),
                chassis_serials = target.source.chassis_serials().join(","),
                "NMX-C certificate rotation is required, but this legacy chassis-mapped endpoint has no actionable rack; automatic rotation is unavailable"
            );
            return Ok(SwitchCertApplyStatus::Skipped);
        };
        let switch_id_label = target
            .source
            .switch_id()
            .map(ToString::to_string)
            .unwrap_or_else(|| "unavailable".to_string());

        if self.component_manager.is_none() {
            return Err(
                "component manager is not configured; cannot request NMX-C cluster configuration"
                    .to_string(),
            );
        }

        // Empty device lists intentionally select the full rack. ConfigureNmxCluster runs the
        // same certificate and fabric-manager workflow as `rack maintenance start
        // --activities configure-nmx-cluster`.
        let scope = MaintenanceScope {
            activities: vec![MaintenanceActivity::ConfigureNmxCluster],
            ..Default::default()
        };
        let outcome = tokio::select! {
            _ = cancel_token.cancelled() => {
                return Err(Self::APPLY_CANCELLED_ERROR.to_string());
            }
            outcome = request_rack_maintenance_via_state_controller(
                &self.db_pool,
                rack_id,
                scope,
                RackMaintenanceEligibility::RequireReady,
                None,
            ) => outcome.map_err(|error| {
                format!(
                    "component manager failed to request NMX-C cluster configuration: {error}"
                )
            })?,
        };

        match outcome {
            RackMaintenanceRequestOutcome::Scheduled => {
                tracing::info!(
                    switch_id = %switch_id_label,
                    rack_id = %rack_id,
                    endpoint = %target.endpoint_url,
                    "Requested full-rack NMX-C cluster configuration via component manager"
                );
                Ok(SwitchCertApplyStatus::Pending)
            }
            RackMaintenanceRequestOutcome::AlreadyPending => {
                tracing::debug!(
                    switch_id = %switch_id_label,
                    rack_id = %rack_id,
                    endpoint = %target.endpoint_url,
                    "Full-rack NMX-C cluster configuration is already pending"
                );
                Ok(SwitchCertApplyStatus::Pending)
            }
            RackMaintenanceRequestOutcome::Busy => {
                tracing::info!(
                    switch_id = %switch_id_label,
                    rack_id = %rack_id,
                    endpoint = %target.endpoint_url,
                    "Deferring NMX-C certificate rotation because different rack maintenance is pending"
                );
                Ok(SwitchCertApplyStatus::Skipped)
            }
            RackMaintenanceRequestOutcome::Deferred { state } => {
                tracing::info!(
                    switch_id = %switch_id_label,
                    rack_id = %rack_id,
                    endpoint = %target.endpoint_url,
                    ?state,
                    "Deferring NMX-C certificate rotation until the rack is ready"
                );
                Ok(SwitchCertApplyStatus::Skipped)
            }
        }
    }

    async fn probe_endpoint_certificate(
        &self,
        endpoint_url: &str,
        cancel_token: &CancellationToken,
    ) -> Result<CertificateProbeOutcome, String> {
        let uri = endpoint_url
            .parse::<http::Uri>()
            .map_err(|error| format!("invalid NMX-C endpoint URI {endpoint_url}: {error}"))?;

        let scheme = uri.scheme_str().unwrap_or("http");
        if !scheme.eq_ignore_ascii_case("https") {
            return Err(format!(
                "NMX-C endpoint {endpoint_url} is not HTTPS, so no server certificate can be probed"
            ));
        }

        let host = uri
            .host()
            .ok_or_else(|| format!("NMX-C endpoint {endpoint_url} has no host"))?
            .to_string();
        let port = uri.port_u16().unwrap_or(443);
        let tls_authority = self
            .config
            .nmx_c_tls_authority
            .clone()
            .unwrap_or_else(|| host.clone());
        let server_name = ServerName::try_from(tls_authority.clone())
            .map_err(|error| format!("invalid NMX-C TLS authority {tls_authority}: {error}"))?;

        let probe_timeout = self.config.nmx_c_certificate_rotation.probe_timeout;
        let client_config = tokio::select! {
            _ = cancel_token.cancelled() => {
                return Err(Self::PROBE_CANCELLED_ERROR.to_string());
            }
            client_config = build_tls_client_config(&self.config) => client_config?,
        };
        let connector = TlsConnector::from(Arc::new(client_config));
        let tcp_stream = tokio::select! {
            _ = cancel_token.cancelled() => {
                return Err(Self::PROBE_CANCELLED_ERROR.to_string());
            }
            tcp_stream = tokio::time::timeout(
                probe_timeout,
                TcpStream::connect((host.as_str(), port)),
            ) => tcp_stream
                .map_err(|_| {
                    format!("connection to {host}:{port} timed out after {probe_timeout:?}")
                })?
                .map_err(|error| format!("failed to connect to {host}:{port}: {error}"))?,
        };
        let tls_stream = tokio::select! {
            _ = cancel_token.cancelled() => {
                return Err(Self::PROBE_CANCELLED_ERROR.to_string());
            }
            tls_stream = tokio::time::timeout(
                probe_timeout,
                connector.connect(server_name, tcp_stream),
            ) => match tls_stream {
                Err(_) => {
                    return Err(format!(
                        "TLS handshake timed out for {endpoint_url} after {probe_timeout:?}"
                    ));
                }
                Ok(Err(error)) if tls_error_is_expired_certificate(&error) => {
                    return Ok(CertificateProbeOutcome::Expired);
                }
                Ok(Err(error)) => {
                    return Err(format!("TLS handshake failed for {endpoint_url}: {error}"));
                }
                Ok(Ok(tls_stream)) => tls_stream,
            },
        };

        let peer_certs =
            tls_stream.get_ref().1.peer_certificates().ok_or_else(|| {
                format!("NMX-C endpoint {endpoint_url} did not serve a certificate")
            })?;
        let leaf_cert = peer_certs.first().ok_or_else(|| {
            format!("NMX-C endpoint {endpoint_url} served an empty certificate chain")
        })?;
        certificate_info_from_der(leaf_cert.as_ref()).map(CertificateProbeOutcome::Observed)
    }
}

fn tls_error_is_expired_certificate(error: &io::Error) -> bool {
    // tokio-rustls retains the rustls handshake error inside io::Error.
    matches!(
        error
            .get_ref()
            .and_then(|source| source.downcast_ref::<RustlsError>()),
        Some(RustlsError::InvalidCertificate(
            CertificateError::ExpiredContext { .. }
        ))
    )
}

async fn build_tls_client_config(config: &NvLinkConfig) -> Result<ClientConfig, String> {
    let mut roots = RootCertStore::empty();
    let ca_cert_path = config
        .nmx_c_tls_ca_cert_path
        .as_ref()
        .ok_or_else(|| "nmx_c_tls_ca_cert_path is required to probe NMX-C TLS".to_string())?;
    let ca_certs = read_certs_from_pem_file(ca_cert_path).await?;
    let (added, ignored) = roots.add_parsable_certificates(ca_certs);
    if added == 0 {
        return Err(format!(
            "no CA certificates from {ca_cert_path} could be added to the NMX-C TLS root store; ignored {ignored}"
        ));
    }

    let builder = ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| format!("failed to build rustls client config: {error}"))?
    .with_root_certificates(roots);

    match (
        &config.nmx_c_tls_client_cert_path,
        &config.nmx_c_tls_client_key_path,
    ) {
        (Some(client_cert_path), Some(client_key_path)) => {
            let certs = read_certs_from_pem_file(client_cert_path).await?;
            let key = read_private_key_from_pem_file(client_key_path).await?;
            builder
                .with_client_auth_cert(certs, key)
                .map_err(|error| format!("invalid NMX-C client certificate config: {error}"))
        }
        (None, None) => Ok(builder.with_no_client_auth()),
        _ => Err(
            "nmx_c_tls_client_cert_path and nmx_c_tls_client_key_path must be configured together"
                .to_string(),
        ),
    }
}

async fn read_certs_from_pem_file(path: &str) -> Result<Vec<CertificateDer<'static>>, String> {
    let pem = tokio::fs::read(path)
        .await
        .map_err(|error| format!("failed to read {path}: {error}"))?;
    let mut cursor = io::Cursor::new(pem);
    rustls_pemfile::certs(&mut cursor)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("failed to parse certificates from {path}: {error}"))
}

async fn read_private_key_from_pem_file(path: &str) -> Result<PrivateKeyDer<'static>, String> {
    let pem = tokio::fs::read(path)
        .await
        .map_err(|error| format!("failed to read {path}: {error}"))?;
    let mut cursor = io::Cursor::new(pem);
    rustls_pemfile::private_key(&mut cursor)
        .map_err(|error| format!("failed to parse private key from {path}: {error}"))?
        .ok_or_else(|| format!("no private key found in {path}"))
}

fn certificate_info_from_der(der: &[u8]) -> Result<CertificateInfo, String> {
    let (_, cert) = X509Certificate::from_der(der)
        .map_err(|error| format!("failed to parse X.509 certificate: {error}"))?;
    let fingerprint_sha256 = hex::encode_upper(Sha256::digest(der));
    Ok(CertificateInfo {
        fingerprint_sha256,
        not_after_timestamp: cert.validity.not_after.timestamp(),
    })
}

fn cert_expires_within(cert: &CertificateInfo, window: Duration) -> bool {
    cert_expires_within_at(cert, window, Utc::now())
}

fn cert_expires_within_at(
    cert: &CertificateInfo,
    window: Duration,
    now: chrono::DateTime<Utc>,
) -> bool {
    let Some(not_after) = chrono::DateTime::from_timestamp(cert.not_after_timestamp, 0) else {
        return true;
    };
    let Ok(window) = chrono::Duration::from_std(window) else {
        return true;
    };
    let Some(rotation_threshold) = now.checked_add_signed(window) else {
        return true;
    };
    not_after <= rotation_threshold
}

fn metric_attrs(base_attrs: &[KeyValue], extra_attrs: &[KeyValue]) -> Vec<KeyValue> {
    [base_attrs, extra_attrs].concat()
}

fn count_by_status(
    certs: &[ObservedSwitchCertMetrics],
    status_for_cert: impl Fn(&ObservedSwitchCertMetrics) -> &'static str,
) -> BTreeMap<&'static str, u64> {
    let mut counts = BTreeMap::new();
    for cert in certs {
        *counts.entry(status_for_cert(cert)).or_insert(0) += 1;
    }
    counts
}

fn count_errors_by_kind<'a>(
    errors: impl Iterator<Item = &'a str>,
) -> BTreeMap<SwitchCertMonitorErrorKind, u64> {
    let mut counts = BTreeMap::new();
    for error in errors {
        if !error.is_empty() {
            *counts
                .entry(switch_cert_monitor_error_kind(error))
                .or_insert(0) += 1;
        }
    }
    counts
}

fn probe_status(cert: &ObservedSwitchCertMetrics) -> &'static str {
    if cert.probe_success { "ok" } else { "error" }
}

fn rotation_window_status(cert: &ObservedSwitchCertMetrics) -> &'static str {
    if cert.rotation_required {
        "expiring_soon"
    } else if cert.observed_cert.is_none() {
        "unknown"
    } else {
        "ok"
    }
}

fn apply_status(cert: &ObservedSwitchCertMetrics) -> &'static str {
    cert.apply_status.as_metric_label()
}

fn switch_cert_monitor_error_kind(error: &str) -> SwitchCertMonitorErrorKind {
    let error = error.to_ascii_lowercase();
    if error.contains("timed out") {
        SwitchCertMonitorErrorKind::Timeout
    } else if error.contains("failed to connect") || error.contains("connection to") {
        SwitchCertMonitorErrorKind::Connection
    } else if error.contains("tls")
        || error.contains("client certificate")
        || error.contains("root store")
    {
        SwitchCertMonitorErrorKind::Tls
    } else if error.contains("failed to read") || error.contains("no certificates found") {
        SwitchCertMonitorErrorKind::CertificateFile
    } else if error.contains("parse") || error.contains("x.509") {
        SwitchCertMonitorErrorKind::CertificateParse
    } else if error.contains("not https")
        || error.contains("invalid nmx-c endpoint uri")
        || error.contains("has no host")
    {
        SwitchCertMonitorErrorKind::EndpointConfig
    } else if error.contains("did not serve") || error.contains("empty certificate chain") {
        SwitchCertMonitorErrorKind::ServerCertificate
    } else if error.contains("not configured")
        || error.contains("required")
        || error.contains("rack profile")
        || error.contains("missing")
    {
        SwitchCertMonitorErrorKind::Configuration
    } else if error.contains("rms ") || error.contains("configurescaleupfabricmanager") {
        SwitchCertMonitorErrorKind::Rms
    } else {
        SwitchCertMonitorErrorKind::Other
    }
}

#[cfg(test)]
mod tests {
    use carbide_instrument::emit;
    use carbide_instrument::testing::{ApproxHistogramSum, MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values};
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use rustls_pki_types::UnixTime;

    use super::*;

    fn ready_control_plane_target(
        rack_id: &str,
        endpoint_url: &str,
    ) -> SwitchCertificateMonitorTarget {
        SwitchCertificateMonitorTarget {
            rack_id: Some(RackId::new(rack_id)),
            endpoint_url: endpoint_url.to_string(),
            source: SwitchCertificateMonitorTargetSource::ReadyControlPlane {
                switch_id: SwitchId::from(uuid::Uuid::new_v4()),
            },
        }
    }

    fn legacy_endpoint(chassis_serial: &str, endpoint: &str) -> NvlinkNmxcEndpoint {
        NvlinkNmxcEndpoint {
            chassis_serial: chassis_serial.to_string(),
            endpoint: endpoint.to_string(),
        }
    }

    #[test]
    fn legacy_chassis_endpoints_are_added_as_alert_only_targets() {
        let targets = merge_switch_certificate_monitor_targets(
            vec![],
            vec![
                legacy_endpoint("chassis-b", "https://10.0.0.2:9370"),
                legacy_endpoint("chassis-a", "https://10.0.0.2:9370"),
                legacy_endpoint("chassis-c", "https://10.0.0.3:9370"),
            ],
        );

        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0].rack_id, None);
        assert_eq!(targets[0].endpoint_url, "https://10.0.0.2:9370");
        assert_eq!(
            targets[0].source,
            SwitchCertificateMonitorTargetSource::LegacyChassisMapping {
                chassis_serials: vec!["chassis-a".to_string(), "chassis-b".to_string()],
            }
        );
        assert_eq!(targets[1].rack_id, None);
        assert_eq!(targets[1].endpoint_url, "https://10.0.0.3:9370");
    }

    #[test]
    fn ready_control_plane_endpoint_wins_over_legacy_mapping() {
        let primary = ready_control_plane_target("rack-a", "https://10.0.0.2:9370");
        let targets = merge_switch_certificate_monitor_targets(
            vec![primary.clone()],
            vec![
                legacy_endpoint("chassis-a", "https://10.0.0.2:9370"),
                legacy_endpoint("chassis-b", "https://10.0.0.3:9370"),
            ],
        );

        assert_eq!(targets.len(), 2);
        assert_eq!(targets[0], primary);
        assert_eq!(targets[1].endpoint_url, "https://10.0.0.3:9370");
        assert_eq!(targets[1].rack_id, None);
    }

    #[test]
    fn probe_classifies_only_expired_certificate_tls_errors_for_rotation() {
        let now = UnixTime::since_unix_epoch(Duration::from_secs(2_000_000_000));
        check_values(
            [
                Check {
                    scenario: "expired certificate",
                    input: io::Error::new(
                        io::ErrorKind::InvalidData,
                        RustlsError::InvalidCertificate(CertificateError::ExpiredContext {
                            time: now,
                            not_after: UnixTime::since_unix_epoch(Duration::from_secs(
                                1_900_000_000,
                            )),
                        }),
                    ),
                    expect: true,
                },
                Check {
                    scenario: "certificate hostname mismatch",
                    input: io::Error::new(
                        io::ErrorKind::InvalidData,
                        RustlsError::InvalidCertificate(CertificateError::NotValidForName),
                    ),
                    expect: false,
                },
                Check {
                    scenario: "non-TLS I/O error",
                    input: io::Error::new(io::ErrorKind::ConnectionReset, "connection reset"),
                    expect: false,
                },
            ],
            |error| tls_error_is_expired_certificate(&error),
        );
    }

    #[test]
    fn expired_certificate_without_metadata_is_in_rotation_window() {
        let cert = ObservedSwitchCertMetrics {
            probe_success: false,
            rotation_required: true,
            observed_cert: None,
            error: "certificate expired".to_string(),
            apply_status: SwitchCertApplyStatus::Pending,
            apply_error: String::new(),
        };

        assert_eq!(rotation_window_status(&cert), "expiring_soon");
    }

    #[test]
    fn certificate_info_from_der_returns_fingerprint_and_expiry() {
        let CertifiedKey { cert, .. } =
            generate_simple_self_signed(vec!["nmxc.example.test".to_string()]).unwrap();
        let expected_fingerprint = hex::encode_upper(Sha256::digest(cert.der().as_ref()));

        let actual = certificate_info_from_der(cert.der().as_ref()).unwrap();

        assert_eq!(actual.fingerprint_sha256, expected_fingerprint);
        assert!(actual.not_after_timestamp > Utc::now().timestamp());
    }

    #[test]
    fn certificate_rotation_window_includes_expired_and_boundary_certificates() {
        const DAY_SECONDS: i64 = 24 * 60 * 60;

        #[derive(Clone, Copy, Debug)]
        struct ExpiryCase {
            not_after_offset_seconds: i64,
            rotation_window: Duration,
        }

        let now = chrono::DateTime::from_timestamp(1_800_000_000, 0).unwrap();
        check_values(
            [
                Check {
                    scenario: "already expired",
                    input: ExpiryCase {
                        not_after_offset_seconds: -1,
                        rotation_window: Duration::ZERO,
                    },
                    expect: true,
                },
                Check {
                    scenario: "exactly at rotation boundary",
                    input: ExpiryCase {
                        not_after_offset_seconds: 7 * DAY_SECONDS,
                        rotation_window: Duration::from_secs(7 * DAY_SECONDS as u64),
                    },
                    expect: true,
                },
                Check {
                    scenario: "inside rotation window",
                    input: ExpiryCase {
                        not_after_offset_seconds: 7 * DAY_SECONDS - 1,
                        rotation_window: Duration::from_secs(7 * DAY_SECONDS as u64),
                    },
                    expect: true,
                },
                Check {
                    scenario: "outside rotation window",
                    input: ExpiryCase {
                        not_after_offset_seconds: 7 * DAY_SECONDS + 1,
                        rotation_window: Duration::from_secs(7 * DAY_SECONDS as u64),
                    },
                    expect: false,
                },
            ],
            |case| {
                cert_expires_within_at(
                    &CertificateInfo {
                        fingerprint_sha256: "test-fingerprint".to_string(),
                        not_after_timestamp: now.timestamp() + case.not_after_offset_seconds,
                    },
                    case.rotation_window,
                    now,
                )
            },
        );
    }

    #[test]
    fn switch_certificate_iteration_records_latency_and_warns_only_on_failure() {
        const METRIC_NAME: &str =
            "carbide_nvlink_switch_cert_monitor_iteration_latency_milliseconds";

        struct IterationCase {
            latency_ms: f64,
            error: &'static str,
        }

        #[derive(Debug, PartialEq)]
        struct LogObservation {
            level: tracing::Level,
            metadata_name: String,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            error: Option<String>,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            log_count: usize,
            log: Option<LogObservation>,
            histogram_count_delta: u64,
            histogram_sum_delta: ApproxHistogramSum,
        }

        check_values(
            [
                Check {
                    scenario: "successful iteration",
                    input: IterationCase {
                        latency_ms: 225.0,
                        error: "",
                    },
                    expect: Observation {
                        log_count: 0,
                        log: None,
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(225.0),
                    },
                },
                Check {
                    scenario: "failed iteration",
                    input: IterationCase {
                        latency_ms: 425.0,
                        error: "certificate query failed",
                    },
                    expect: Observation {
                        log_count: 1,
                        log: Some(LogObservation {
                            level: tracing::Level::WARN,
                            metadata_name: "nvlink_switch_certificate_monitor_iteration_finished"
                                .to_string(),
                            message: "Switch certificate monitor error".to_string(),
                            event_name: Some(
                                "nvlink_switch_certificate_monitor_iteration_finished".to_string(),
                            ),
                            metric_name: Some(METRIC_NAME.to_string()),
                            error: Some("certificate query failed".to_string()),
                        }),
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(425.0),
                    },
                },
            ],
            |IterationCase { latency_ms, error }| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| {
                    emit(if error.is_empty() {
                        SwitchCertificateMonitorIterationFinished::Succeeded { latency_ms }
                    } else {
                        SwitchCertificateMonitorIterationFinished::Failed {
                            latency_ms,
                            error: error.to_string(),
                        }
                    });
                });
                let log = logs.first().map(|log| LogObservation {
                    level: log.level,
                    metadata_name: log.metadata_name.clone(),
                    message: log.message.clone(),
                    event_name: log.field("event_name").map(str::to_string),
                    metric_name: log.field("metric_name").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                });

                Observation {
                    log_count: logs.len(),
                    log,
                    histogram_count_delta: metrics.histogram_count_delta(METRIC_NAME, &[]),
                    histogram_sum_delta: metrics.histogram_sum_delta(METRIC_NAME, &[]),
                }
            },
        );
    }

    #[test]
    fn switch_certificate_iteration_histogram_exposition_stays_stable() {
        const METRIC_NAME: &str =
            "carbide_nvlink_switch_cert_monitor_iteration_latency_milliseconds";

        let metrics = MetricsCapture::start();
        emit(SwitchCertificateMonitorIterationFinished::Succeeded { latency_ms: 225.0 });

        let encoded = metrics.render();
        assert!(
            encoded.contains(&format!(
                "# HELP {METRIC_NAME} Time consumed for one NMX-C switch certificate monitor iteration\n"
            )),
            "description or exposed family changed:\n{encoded}"
        );
        assert!(
            encoded.contains(&format!("# TYPE {METRIC_NAME} histogram\n")),
            "expected the millisecond family to remain a histogram:\n{encoded}"
        );
        assert!(
            !encoded.contains(
                "carbide_nvlink_switch_cert_monitor_iteration_latency_milliseconds_milliseconds"
            ),
            "the unit suffix must be applied exactly once:\n{encoded}"
        );
        for suffix in ["count", "sum"] {
            let prefix = format!("{METRIC_NAME}_{suffix} ");
            let sample = encoded
                .lines()
                .find(|line| line.starts_with(&prefix))
                .unwrap_or_else(|| panic!("missing {prefix} sample:\n{encoded}"));
            assert!(
                !sample.contains('{'),
                "iteration latency must remain label-free: {sample}"
            );
        }
    }
}
