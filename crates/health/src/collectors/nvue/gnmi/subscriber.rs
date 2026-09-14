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
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use carbide_uuid::rack::RackId;
use prometheus::{Counter, Gauge, Histogram, HistogramOpts, IntGauge, Opts};
use tokio::sync::OnceCell;
use tokio_stream::{Stream, StreamExt};
use tokio_util::sync::CancellationToken;

use super::client::{
    GnmiClient, GnmiClientConfig, build_extended_subscribe_request, nvue_subscribe_paths,
    system_events_prefix, system_events_subscribe_path,
};
use super::extended_processor::{EXTENDED_GNMI_STREAM_ID, ExtendedGnmiProcessor};
use super::on_change_processor::{
    GnmiOnChangeProcessor, ON_CHANGE_STREAM_ID_SYSTEM_EVENTS, OnChangeStreamMetrics,
};
use super::proto;
use super::sample_processor::{GnmiSampleProcessor, NVUE_GNMI_SAMPLE_STREAM_ID, now_unix_secs};
use crate::HealthError;
use crate::bmc::{CREDENTIAL_REFRESH_TIMEOUT, CredentialProvider};
use crate::collectors::Collector;
use crate::collectors::runtime::{
    BackoffConfig, ExponentialBackoff, StreamingConnectionGuard, collector_metric_labels,
};
use crate::config::{MtlsProfileConfig, NvueGnmiConfig};
use crate::endpoint::{BmcAddr, BmcCredentials, BmcEndpoint};
use crate::metrics::CollectorRegistry;
use crate::sink::{CollectorEvent, DataSink, EventContext};

// gRPC ConnectivityState values for `connection_state`. 0 (UNKNOWN) is the gauge default.
const IDLE: i64 = 1;
const CONNECTING: i64 = 2;
const READY: i64 = 3;
const TRANSIENT_FAILURE: i64 = 4;
const SHUTDOWN: i64 = 5;

pub(crate) struct GnmiStreamMetrics {
    pub(crate) connection_state: IntGauge,
    /// binary "is this stream live right now?" -- guard-managed, mirrors SSE's `connected` gauge
    pub(crate) connected: IntGauge,

    /// Binary readiness signal set after the current stream completes initial synchronization.
    pub(crate) synchronized: IntGauge,
    pub(crate) reconnections_total: Counter,
    pub(crate) server_initiated_closures_total: Counter,
    pub(crate) connection_established_timestamp: Gauge,
    pub(crate) notifications_received_total: Counter,
    pub(crate) last_notification_timestamp: Gauge,
    pub(crate) notification_processing_seconds: Histogram,
    pub(crate) stream_errors_total: Counter,
    pub(crate) monitored_entities: Gauge,
}

impl GnmiStreamMetrics {
    fn new(
        registry: &prometheus::Registry,
        prefix: &str,
        stream_name: &str,
        const_labels: HashMap<String, String>,
    ) -> Result<Self, HealthError> {
        let connection_state = IntGauge::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_connection_state"),
                "gRPC connection state: 0=UNKNOWN, 1=IDLE, 2=CONNECTING, 3=READY, 4=TRANSIENT_FAILURE, 5=SHUTDOWN",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(connection_state.clone()))?;

        let connected = IntGauge::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_stream_connected"),
                "1 while the stream is connected (READY), 0 otherwise. Mirrors the SSE collector's stream_connected gauge for aggregate streaming dashboards.",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(connected.clone()))?;

        let synchronized = IntGauge::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_stream_synchronized"),
                "1 after the current stream receives sync_response=true, 0 otherwise",
            )
            .const_labels(const_labels.clone()),
        )?;

        registry.register(Box::new(synchronized.clone()))?;

        let reconnections_total = Counter::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_reconnections_total"),
                "Total reconnection attempts",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(reconnections_total.clone()))?;

        let server_initiated_closures_total = Counter::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_server_initiated_closures_total"),
                "Total times the server closed the stream cleanly",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(server_initiated_closures_total.clone()))?;

        let connection_established_timestamp = Gauge::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_connection_established_timestamp"),
                "Unix timestamp when current connection was established. Compute uptime via time() - this_metric.",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(connection_established_timestamp.clone()))?;

        let notifications_received_total = Counter::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_notifications_received_total"),
                "Total notification messages received",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(notifications_received_total.clone()))?;

        let last_notification_timestamp = Gauge::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_last_notification_timestamp"),
                "Unix timestamp of most recent notification",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(last_notification_timestamp.clone()))?;

        let notification_processing_seconds = Histogram::with_opts(
            HistogramOpts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_notification_processing_seconds"),
                "Per-notification processing time",
            )
            .const_labels(const_labels.clone())
            .buckets(vec![0.001, 0.005, 0.01, 0.05, 0.1, 0.5, 1.0]),
        )?;
        registry.register(Box::new(notification_processing_seconds.clone()))?;

        let stream_errors_total = Counter::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_stream_errors_total"),
                "Total stream errors",
            )
            .const_labels(const_labels.clone()),
        )?;
        registry.register(Box::new(stream_errors_total.clone()))?;

        let monitored_entities = Gauge::with_opts(
            Opts::new(
                format!("{prefix}_nvue_gnmi{stream_name}_monitored_entities"),
                "Unique entities in most recent notification batch",
            )
            .const_labels(const_labels),
        )?;
        registry.register(Box::new(monitored_entities.clone()))?;

        Ok(Self {
            connection_state,
            connected,
            synchronized,
            reconnections_total,
            server_initiated_closures_total,
            connection_established_timestamp,
            notifications_received_total,
            last_notification_timestamp,
            notification_processing_seconds,
            stream_errors_total,
            monitored_entities,
        })
    }
}

#[cfg(test)]
pub(super) fn test_gnmi_stream_metrics() -> GnmiStreamMetrics {
    GnmiStreamMetrics::new(
        &prometheus::Registry::new(),
        "test",
        "_extended",
        HashMap::new(),
    )
    .unwrap()
}

struct GnmiStreamConfig {
    client_provider: GnmiClientProvider,
    paths: Vec<proto::Path>,
    sample_interval_nanos: u64,
}

struct ExtendedGnmiStreamState {
    client_provider: GnmiClientProvider,
    request: proto::SubscribeRequest,
    stream_metrics: GnmiStreamMetrics,
    processor: ExtendedGnmiProcessor,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ExtendedStreamExit {
    Cancelled,
    Reconnect,
}

#[derive(Clone)]
struct GnmiClientProvider {
    switch_id: String,
    rack_id: Option<RackId>,
    switch_connect_host: String,
    port: u16,
    request_timeout: Duration,
    dangerously_skip_tls_verification: bool,

    // Streaming subscriptions build a fresh gNMI client on reconnect, so
    // rotated mTLS profile files are adopted after the stream reconnects.
    tls_config: Option<MtlsProfileConfig>,
    credentials: Arc<GnmiCredentialCache>,
}

struct GnmiCredentialCache {
    credential_provider: Arc<dyn CredentialProvider>,
    addr: BmcAddr,
    init: OnceCell<()>,
    cached: RwLock<Option<GnmiUsernamePassword>>,
    generation: AtomicU64,
}

#[derive(Clone, Debug)]
struct GnmiUsernamePassword {
    username: Option<String>,
    password: Option<String>,
}

/// Processes responses until `sync_response=true` completes initial synchronization.
///
/// The timeout covers the complete synchronization period; updates received
/// before synchronization are processed without extending it. EOF records a
/// clean server closure, while a timeout or error records a stream failure and
/// refreshes rejected credentials. Every non-ready exit returns `None` so the
/// caller can apply reconnect backoff. A returned guard keeps the synchronization
/// gauge set until that stream ends.
async fn await_stream_synchronization<S, F>(
    cancel_token: &CancellationToken,
    responses: &mut S,
    client_provider: &GnmiClientProvider,
    credential_generation: u64,
    stream_metrics: &GnmiStreamMetrics,
    mut process_response: F,
) -> Option<StreamingConnectionGuard>
where
    S: Stream<Item = Result<proto::SubscribeResponse, tonic::Status>> + Unpin,
    F: FnMut(&proto::SubscribeResponse) -> Result<(), tonic::Status>,
{
    let receive_until_synchronized = async {
        loop {
            let Some(response) = responses.next().await else {
                return Ok(false);
            };

            let response = response?;

            match response.response.as_ref() {
                Some(proto::subscribe_response::Response::SyncResponse(true)) => {
                    return Ok(true);
                }
                Some(proto::subscribe_response::Response::SyncResponse(false)) => continue,
                #[allow(deprecated, reason = "accept the legacy in-band gNMI error response")]
                Some(proto::subscribe_response::Response::Error(error)) => {
                    let code = i32::try_from(error.code)
                        .map(tonic::Code::from_i32)
                        .unwrap_or(tonic::Code::Unknown);

                    return Err(tonic::Status::new(code, error.message.clone()));
                }
                _ => process_response(&response)?,
            }
        }
    };

    let result = cancel_token
        .run_until_cancelled(tokio::time::timeout(
            client_provider.request_timeout,
            receive_until_synchronized,
        ))
        .await;

    let result = match result {
        None => {
            stream_metrics.connection_state.set(SHUTDOWN);

            tracing::info!(
                switch_id = %client_provider.switch_id,
                rack_id = client_provider.rack_id.as_ref().map(tracing::field::display),
                "gNMI stream synchronization cancelled"
            );

            return None;
        }
        Some(Ok(result)) => result,
        Some(Err(_)) => Err(tonic::Status::deadline_exceeded(format!(
            "gNMI stream did not synchronize within {:?}",
            client_provider.request_timeout
        ))),
    };

    match result {
        Ok(true) => Some(StreamingConnectionGuard::inc(
            stream_metrics.synchronized.clone(),
        )),
        Ok(false) => {
            stream_metrics.connection_state.set(IDLE);
            stream_metrics.server_initiated_closures_total.inc();
            None
        }
        Err(error) => {
            stream_metrics.connection_state.set(TRANSIENT_FAILURE);
            stream_metrics.stream_errors_total.inc();
            stream_metrics.reconnections_total.inc();

            client_provider
                .refresh_status_auth_if_needed(&error, credential_generation)
                .await;

            tracing::warn!(
                error = ?error,
                switch_id = %client_provider.switch_id,
                rack_id = client_provider.rack_id.as_ref().map(tracing::field::display),
                "gNMI stream failed before synchronization; backing off"
            );

            None
        }
    }
}

impl GnmiClientProvider {
    async fn new_client(&self) -> Result<(GnmiClient, u64), HealthError> {
        let (credentials, generation) = self.credentials.ensure().await?;
        Ok((
            GnmiClient::new(GnmiClientConfig {
                switch_id: self.switch_id.clone(),
                rack_id: self.rack_id.clone(),
                host: self.switch_connect_host.clone(),
                port: self.port,
                username: credentials.username,
                password: credentials.password,
                request_timeout: self.request_timeout,
                dangerously_skip_tls_verification: self.dangerously_skip_tls_verification,
                tls_config: self.tls_config.clone(),
            }),
            generation,
        ))
    }

    async fn refresh_auth_if_needed(&self, error: &HealthError, observed_generation: u64) {
        if is_gnmi_auth_error(error)
            && let Err(refresh_error) = self.credentials.refresh(observed_generation, error).await
        {
            tracing::error!(
                error = ?refresh_error,
                original_error = ?error,
                switch_id = %self.switch_id,
                rack_id = self.rack_id.as_ref().map(tracing::field::display),
                "Failed to refresh NVUE gNMI credentials after authentication error"
            );
        }
    }

    async fn refresh_status_auth_if_needed(
        &self,
        status: &tonic::Status,
        observed_generation: u64,
    ) {
        if is_gnmi_auth_status(status)
            && let Err(refresh_error) = self
                .credentials
                .refresh(
                    observed_generation,
                    &HealthError::GnmiStatus(status.clone()),
                )
                .await
        {
            tracing::error!(
                error = ?refresh_error,
                original_error = ?status,
                switch_id = %self.switch_id,
                rack_id = self.rack_id.as_ref().map(tracing::field::display),
                "Failed to refresh NVUE gNMI credentials after authentication stream status"
            );
        }
    }
}

fn is_gnmi_auth_status(status: &tonic::Status) -> bool {
    matches!(
        status.code(),
        tonic::Code::Unauthenticated | tonic::Code::PermissionDenied
    )
}

fn is_gnmi_auth_error(error: &HealthError) -> bool {
    matches!(error, HealthError::GnmiStatus(status) if is_gnmi_auth_status(status))
}

struct GnmiStreamOpenError {
    error: HealthError,
    credential_generation: Option<u64>,
}

async fn subscribe_sample_with_cached_credentials(
    client_provider: &GnmiClientProvider,
    paths: &[proto::Path],
    sample_interval_nanos: u64,
) -> Result<(tonic::Streaming<proto::SubscribeResponse>, u64), GnmiStreamOpenError> {
    let (client, credential_generation) =
        client_provider
            .new_client()
            .await
            .map_err(|error| GnmiStreamOpenError {
                error,
                credential_generation: None,
            })?;
    let stream = client
        .subscribe_sample(paths, sample_interval_nanos)
        .await
        .map_err(|error| GnmiStreamOpenError {
            error,
            credential_generation: Some(credential_generation),
        })?;
    Ok((stream, credential_generation))
}

async fn subscribe_on_change_with_cached_credentials(
    client_provider: &GnmiClientProvider,
    prefix: &proto::Path,
    paths: &[proto::Path],
) -> Result<(tonic::Streaming<proto::SubscribeResponse>, u64), GnmiStreamOpenError> {
    let (client, credential_generation) =
        client_provider
            .new_client()
            .await
            .map_err(|error| GnmiStreamOpenError {
                error,
                credential_generation: None,
            })?;
    let stream = client
        .subscribe_on_change(prefix, paths)
        .await
        .map_err(|error| GnmiStreamOpenError {
            error,
            credential_generation: Some(credential_generation),
        })?;
    Ok((stream, credential_generation))
}

async fn subscribe_extended_with_cached_credentials(
    client_provider: &GnmiClientProvider,
    request: proto::SubscribeRequest,
) -> Result<(tonic::Streaming<proto::SubscribeResponse>, u64), GnmiStreamOpenError> {
    let (client, credential_generation) =
        client_provider
            .new_client()
            .await
            .map_err(|error| GnmiStreamOpenError {
                error,
                credential_generation: None,
            })?;

    let stream = client
        .subscribe_request(request)
        .await
        .map_err(|error| GnmiStreamOpenError {
            error,
            credential_generation: Some(credential_generation),
        })?;

    Ok((stream, credential_generation))
}

impl GnmiCredentialCache {
    fn new(credential_provider: Arc<dyn CredentialProvider>, addr: BmcAddr) -> Self {
        Self {
            credential_provider,
            addr,
            init: OnceCell::new(),
            cached: RwLock::new(None),
            generation: AtomicU64::new(0),
        }
    }

    async fn ensure(&self) -> Result<(GnmiUsernamePassword, u64), HealthError> {
        if let Some(credentials) = self.cached_credentials()? {
            return Ok(credentials);
        }

        self.init
            .get_or_try_init(|| async {
                let credentials =
                    fetch_gnmi_username_password(self.credential_provider.clone(), &self.addr)
                        .await?;
                self.store_credentials(credentials)?;
                Ok::<_, HealthError>(())
            })
            .await?;

        let credentials = self.cached_credentials()?.ok_or_else(|| {
            HealthError::GnmiError("NVUE gNMI credential cache initialized empty".to_string())
        })?;
        Ok(credentials)
    }

    async fn refresh(
        &self,
        observed_generation: u64,
        error: &HealthError,
    ) -> Result<(), HealthError> {
        if observed_generation != self.generation.load(AtomicOrdering::Acquire) {
            return Ok(());
        }

        tracing::warn!(
            error = ?error,
            endpoint = ?self.addr,
            "NVUE gNMI authentication failed, refreshing credentials"
        );

        let credentials = fetch_gnmi_username_password(
            self.credential_provider.clone(),
            &self.addr,
        )
        .await
        .map_err(|refresh_error| {
            HealthError::GnmiError(format!(
                "Failed to refresh NVUE gNMI credentials after auth error {error}: {refresh_error}"
            ))
        })?;
        self.store_credentials_if_current(credentials, observed_generation)?;
        Ok(())
    }

    fn cached_credentials(&self) -> Result<Option<(GnmiUsernamePassword, u64)>, HealthError> {
        let cached = self.cached.read().map_err(|_| {
            HealthError::GnmiError("NVUE gNMI credential cache lock poisoned".to_string())
        })?;
        Ok(cached
            .clone()
            .map(|credentials| (credentials, self.generation.load(AtomicOrdering::Acquire))))
    }

    fn store_credentials(&self, credentials: GnmiUsernamePassword) -> Result<(), HealthError> {
        let mut cached = self.cached.write().map_err(|_| {
            HealthError::GnmiError("NVUE gNMI credential cache lock poisoned".to_string())
        })?;
        *cached = Some(credentials);
        self.generation.fetch_add(1, AtomicOrdering::AcqRel);
        Ok(())
    }

    fn store_credentials_if_current(
        &self,
        credentials: GnmiUsernamePassword,
        observed_generation: u64,
    ) -> Result<(), HealthError> {
        let mut cached = self.cached.write().map_err(|_| {
            HealthError::GnmiError("NVUE gNMI credential cache lock poisoned".to_string())
        })?;
        if observed_generation != self.generation.load(AtomicOrdering::Acquire) {
            return Ok(());
        }
        *cached = Some(credentials);
        self.generation.fetch_add(1, AtomicOrdering::AcqRel);
        Ok(())
    }
}

async fn fetch_gnmi_username_password(
    provider: Arc<dyn CredentialProvider>,
    addr: &BmcAddr,
) -> Result<GnmiUsernamePassword, HealthError> {
    let credentials =
        tokio::time::timeout(CREDENTIAL_REFRESH_TIMEOUT, provider.fetch_credentials(addr))
            .await
            .map_err(|_elapsed| {
                HealthError::GnmiError(format!(
                    "Timed out after {}s fetching NVUE gNMI credentials",
                    CREDENTIAL_REFRESH_TIMEOUT.as_secs(),
                ))
            })??;

    match credentials {
        BmcCredentials::UsernamePassword { username, password } => Ok(GnmiUsernamePassword {
            username: Some(username),
            password,
        }),
        BmcCredentials::SessionToken { .. } => Err(HealthError::GnmiError(
            "NVUE gNMI collector requires username/password credentials".to_string(),
        )),
    }
}

pub(crate) fn spawn_gnmi_collector(
    endpoint: &BmcEndpoint,
    gnmi_config: &NvueGnmiConfig,
    credential_provider: Arc<dyn CredentialProvider>,
    collector_registry: Arc<CollectorRegistry>,
    data_sink: Option<Arc<dyn DataSink>>,
    tls_config: Option<MtlsProfileConfig>,
) -> Result<Collector, HealthError> {
    let switch_id = endpoint
        .metadata
        .as_ref()
        .and_then(|m| m.serial_number().map(str::to_string))
        .unwrap_or_else(|| endpoint.addr.mac.to_string());

    let switch_connect_host = endpoint.switch_connect_host_for_uri().into_owned();
    let sample_event_context = EventContext::from_endpoint(endpoint, NVUE_GNMI_SAMPLE_STREAM_ID);

    let client_provider = GnmiClientProvider {
        switch_id: switch_id.clone(),
        rack_id: endpoint.rack_id.clone(),
        switch_connect_host,
        port: gnmi_config.gnmi_port,
        request_timeout: gnmi_config.request_timeout,
        dangerously_skip_tls_verification: gnmi_config.dangerously_skip_tls_verification,
        tls_config,
        credentials: Arc::new(GnmiCredentialCache::new(
            credential_provider,
            endpoint.addr.clone(),
        )),
    };

    let registry = collector_registry.registry();
    let prefix = collector_registry.prefix().clone();
    let collector_removed_sample_context = sample_event_context.clone();
    let mut collector_removed_on_change_context = None;
    let extended_event_context = EventContext::from_endpoint(endpoint, EXTENDED_GNMI_STREAM_ID);

    let collector_removed_extended_context =
        (!gnmi_config.additional_subscriptions.is_empty()).then(|| extended_event_context.clone());

    let sample_const_labels = collector_metric_labels(
        NVUE_GNMI_SAMPLE_STREAM_ID,
        endpoint.hash_key().into_owned(),
        endpoint,
    );

    let sample_stream_metrics = GnmiStreamMetrics::new(registry, &prefix, "", sample_const_labels)?;

    let sample_config = GnmiStreamConfig {
        client_provider: client_provider.clone(),
        paths: nvue_subscribe_paths(&gnmi_config.paths),
        sample_interval_nanos: gnmi_config.sample_interval.as_nanos() as u64,
    };

    let sample_enabled = !sample_config.paths.is_empty();

    let sample_processor = GnmiSampleProcessor {
        data_sink: data_sink.clone(),
        event_context: sample_event_context,
        switch_id: switch_id.clone(),
    };

    let mut extended_states = Vec::with_capacity(gnmi_config.additional_subscriptions.len());

    for subscription in &gnmi_config.additional_subscriptions {
        let mut const_labels = collector_metric_labels(
            EXTENDED_GNMI_STREAM_ID,
            endpoint.hash_key().into_owned(),
            endpoint,
        );

        const_labels.insert("subscription".to_string(), subscription.name.clone());

        let stream_metrics = GnmiStreamMetrics::new(registry, &prefix, "_extended", const_labels)?;

        extended_states.push(ExtendedGnmiStreamState {
            client_provider: client_provider.clone(),
            request: build_extended_subscribe_request(subscription)?,
            stream_metrics,
            processor: ExtendedGnmiProcessor::new(
                subscription,
                data_sink.clone(),
                extended_event_context.clone(),
                switch_id.clone(),
            ),
        });
    }

    let on_change_state = if gnmi_config.system_events_enabled {
        let on_change_const_labels = collector_metric_labels(
            ON_CHANGE_STREAM_ID_SYSTEM_EVENTS,
            endpoint.hash_key().into_owned(),
            endpoint,
        );

        let on_change_stream_metrics =
            GnmiStreamMetrics::new(registry, &prefix, "_events", on_change_const_labels.clone())?;
        let on_change_row_metrics = OnChangeStreamMetrics::new(
            registry,
            &prefix,
            ON_CHANGE_STREAM_ID_SYSTEM_EVENTS,
            on_change_const_labels,
        )?;
        let on_change_event_context =
            EventContext::from_endpoint(endpoint, ON_CHANGE_STREAM_ID_SYSTEM_EVENTS);
        collector_removed_on_change_context = Some(on_change_event_context.clone());
        let on_change_processor = GnmiOnChangeProcessor::new(
            ON_CHANGE_STREAM_ID_SYSTEM_EVENTS.to_string(),
            on_change_row_metrics,
            data_sink.clone(),
            on_change_event_context,
            switch_id,
        );

        Some((
            client_provider,
            on_change_stream_metrics,
            on_change_processor,
        ))
    } else {
        None
    };
    let collector_removed_data_sink = data_sink;

    Ok(Collector::spawn_task(move |cancel_token| async move {
        // Keep the subregistry registered until all gNMI stream tasks finish.
        let _collector_registry = collector_registry;

        let sample_handle = sample_enabled.then(|| {
            tokio::spawn(gnmi_sample_task(
                cancel_token.clone(),
                sample_config,
                sample_stream_metrics,
                sample_processor,
            ))
        });

        let on_change_handle =
            on_change_state.map(|(client_provider, stream_metrics, on_change_processor)| {
                tokio::spawn(gnmi_on_change_task(
                    cancel_token.clone(),
                    client_provider,
                    stream_metrics,
                    on_change_processor,
                ))
            });

        let extended_handles = extended_states
            .into_iter()
            .map(|state| tokio::spawn(gnmi_extended_task(cancel_token.clone(), state)))
            .collect::<Vec<_>>();

        if let Some(handle) = sample_handle {
            let _ = handle.await;
        }

        if let Some(handle) = on_change_handle {
            let _ = handle.await;
        }

        for handle in extended_handles {
            let _ = handle.await;
        }

        if let Some(data_sink) = collector_removed_data_sink.as_deref() {
            if sample_enabled {
                data_sink.handle_event(
                    &collector_removed_sample_context,
                    &CollectorEvent::CollectorRemoved,
                );
            }

            if let Some(event_context) = &collector_removed_on_change_context {
                data_sink.handle_event(event_context, &CollectorEvent::CollectorRemoved);
            }

            if let Some(event_context) = &collector_removed_extended_context {
                data_sink.handle_event(event_context, &CollectorEvent::CollectorRemoved);
            }
        }
    }))
}

async fn gnmi_extended_task(cancel_token: CancellationToken, mut state: ExtendedGnmiStreamState) {
    let mut backoff = ExponentialBackoff::new(&BackoffConfig {
        initial: Duration::from_secs(2),
        max: Duration::from_secs(60),
    });

    loop {
        state.stream_metrics.connection_state.set(CONNECTING);

        let Some(stream) = cancel_token
            .run_until_cancelled(subscribe_extended_with_cached_credentials(
                &state.client_provider,
                state.request.clone(),
            ))
            .await
        else {
            state.stream_metrics.connection_state.set(SHUTDOWN);
            return;
        };

        match stream {
            Err(error) => {
                state.stream_metrics.connection_state.set(TRANSIENT_FAILURE);
                state.stream_metrics.reconnections_total.inc();

                if let Some(credential_generation) = error.credential_generation {
                    state
                        .client_provider
                        .refresh_auth_if_needed(&error.error, credential_generation)
                        .await;
                }

                tracing::warn!(
                    error = ?error.error,
                    switch_id = %state.processor.switch_id,
                    subscription = %state.processor.subscription_name,
                    rack_id = state.client_provider.rack_id.as_ref().map(tracing::field::display),
                    "extended gNMI stream connection failed; backing off"
                );
            }
            Ok((mut stream, credential_generation)) => 'connected: {
                state.stream_metrics.connection_state.set(READY);
                state
                    .stream_metrics
                    .connection_established_timestamp
                    .set(now_unix_secs());

                let _connection_guard =
                    StreamingConnectionGuard::inc(state.stream_metrics.connected.clone());

                let Some(_synchronization_guard) = await_stream_synchronization(
                    &cancel_token,
                    &mut stream,
                    &state.client_provider,
                    credential_generation,
                    &state.stream_metrics,
                    |response| {
                        state
                            .processor
                            .process_subscribe_response(response, &state.stream_metrics)
                            .map(|_| ())
                    },
                )
                .await
                else {
                    break 'connected;
                };

                tracing::info!(
                    switch_id = %state.processor.switch_id,
                    subscription = %state.processor.subscription_name,
                    rack_id = state.client_provider.rack_id.as_ref().map(tracing::field::display),
                    "extended gNMI stream connected"
                );

                backoff.reset();

                if consume_extended_stream(
                    &cancel_token,
                    &mut state,
                    &mut stream,
                    credential_generation,
                    &mut backoff,
                )
                .await
                    == ExtendedStreamExit::Cancelled
                {
                    return;
                }
            }
        }

        if cancel_token
            .run_until_cancelled(tokio::time::sleep(backoff.next_delay()))
            .await
            .is_none()
        {
            state.stream_metrics.connection_state.set(SHUTDOWN);
            return;
        }
    }
}

async fn consume_extended_stream(
    cancel_token: &CancellationToken,
    state: &mut ExtendedGnmiStreamState,
    stream: &mut tonic::Streaming<proto::SubscribeResponse>,
    credential_generation: u64,
    backoff: &mut ExponentialBackoff,
) -> ExtendedStreamExit {
    loop {
        let Some(message) = cancel_token.run_until_cancelled(stream.message()).await else {
            state.stream_metrics.connection_state.set(SHUTDOWN);

            tracing::info!(
                switch_id = %state.processor.switch_id,
                subscription = %state.processor.subscription_name,
                rack_id = state.client_provider.rack_id.as_ref().map(tracing::field::display),
                "extended gNMI stream cancelled"
            );

            return ExtendedStreamExit::Cancelled;
        };

        match message {
            Ok(Some(response)) => {
                match state
                    .processor
                    .process_subscribe_response(&response, &state.stream_metrics)
                {
                    Ok(true) => backoff.reset(),
                    Ok(false) => {}
                    Err(error) => {
                        state
                            .client_provider
                            .refresh_status_auth_if_needed(&error, credential_generation)
                            .await;

                        state.stream_metrics.connection_state.set(TRANSIENT_FAILURE);

                        state.stream_metrics.reconnections_total.inc();
                        return ExtendedStreamExit::Reconnect;
                    }
                }
            }
            Ok(None) => {
                state.stream_metrics.connection_state.set(IDLE);
                state.stream_metrics.server_initiated_closures_total.inc();

                tracing::info!(
                    switch_id = %state.processor.switch_id,
                    subscription = %state.processor.subscription_name,
                    rack_id = state.client_provider.rack_id.as_ref().map(tracing::field::display),
                    "extended gNMI stream closed by server; reconnecting"
                );

                return ExtendedStreamExit::Reconnect;
            }
            Err(error) => {
                state.stream_metrics.connection_state.set(TRANSIENT_FAILURE);
                state.stream_metrics.stream_errors_total.inc();
                state.stream_metrics.reconnections_total.inc();
                state
                    .client_provider
                    .refresh_status_auth_if_needed(&error, credential_generation)
                    .await;

                tracing::warn!(
                    error = ?error,
                    switch_id = %state.processor.switch_id,
                    subscription = %state.processor.subscription_name,
                    rack_id = state.client_provider.rack_id.as_ref().map(tracing::field::display),
                    "extended gNMI stream error; reconnecting"
                );

                return ExtendedStreamExit::Reconnect;
            }
        }
    }
}

async fn gnmi_sample_task(
    cancel_token: CancellationToken,
    config: GnmiStreamConfig,
    stream_metrics: GnmiStreamMetrics,
    sample_processor: GnmiSampleProcessor,
) {
    run_gnmi_sample_task(
        &cancel_token,
        &config,
        &stream_metrics,
        &sample_processor,
        || {
            subscribe_sample_with_cached_credentials(
                &config.client_provider,
                &config.paths,
                config.sample_interval_nanos,
            )
        },
    )
    .await;
}

/// Runs the SAMPLE reconnect loop with an injectable stream opener.
///
/// Production uses the cached-credential subscriber; tests inject streams to
/// exercise timeout and retry behavior without a network server.
async fn run_gnmi_sample_task<S, F, Fut>(
    cancel_token: &CancellationToken,
    config: &GnmiStreamConfig,
    stream_metrics: &GnmiStreamMetrics,
    sample_processor: &GnmiSampleProcessor,
    mut subscribe: F,
) where
    S: Stream<Item = Result<proto::SubscribeResponse, tonic::Status>> + Send + Unpin,
    F: FnMut() -> Fut + Send,
    Fut: Future<Output = Result<(S, u64), GnmiStreamOpenError>> + Send,
{
    let mut backoff = ExponentialBackoff::new(&BackoffConfig {
        initial: Duration::from_secs(2),
        max: Duration::from_secs(60),
    });

    loop {
        stream_metrics.connection_state.set(CONNECTING);

        let Some(stream) = cancel_token.run_until_cancelled(subscribe()).await else {
            stream_metrics.connection_state.set(SHUTDOWN);
            return;
        };

        match stream {
            Err(e) => {
                stream_metrics.connection_state.set(TRANSIENT_FAILURE);
                stream_metrics.reconnections_total.inc();
                if let Some(credential_generation) = e.credential_generation {
                    config
                        .client_provider
                        .refresh_auth_if_needed(&e.error, credential_generation)
                        .await;
                }
                tracing::warn!(
                    error = ?e.error,
                    switch_id = %sample_processor.switch_id,
                    rack_id = config.client_provider.rack_id.as_ref().map(tracing::field::display),
                    "nvue_gnmi SAMPLE: connection failed, backing off"
                );
            }
            Ok((mut stream, credential_generation)) => 'connected: {
                stream_metrics.connection_state.set(READY);
                stream_metrics
                    .connection_established_timestamp
                    .set(now_unix_secs());

                let _connection_guard =
                    StreamingConnectionGuard::inc(stream_metrics.connected.clone());

                let Some(_synchronization_guard) = await_stream_synchronization(
                    cancel_token,
                    &mut stream,
                    &config.client_provider,
                    credential_generation,
                    stream_metrics,
                    |response| {
                        sample_processor.process_subscribe_response(response, stream_metrics);
                        Ok(())
                    },
                )
                .await
                else {
                    break 'connected;
                };

                backoff.reset();
                tracing::info!(
                    switch_id = %sample_processor.switch_id,
                    rack_id = config.client_provider.rack_id.as_ref().map(tracing::field::display),
                    "nvue_gnmi SAMPLE: stream connected"
                );

                loop {
                    let Some(msg) = cancel_token.run_until_cancelled(stream.next()).await else {
                        stream_metrics.connection_state.set(SHUTDOWN);
                        tracing::info!(
                            switch_id = %sample_processor.switch_id,
                            rack_id = config.client_provider.rack_id.as_ref().map(tracing::field::display),
                            "nvue_gnmi SAMPLE: cancelled, shutting down"
                        );
                        return;
                    };

                    match msg {
                        Some(Ok(resp)) => {
                            sample_processor.process_subscribe_response(&resp, stream_metrics);
                        }
                        None => {
                            stream_metrics.connection_state.set(IDLE);
                            stream_metrics.server_initiated_closures_total.inc();
                            tracing::info!(
                                switch_id = %sample_processor.switch_id,
                                rack_id = config.client_provider.rack_id.as_ref().map(tracing::field::display),
                                "nvue_gnmi SAMPLE: stream closed by server, reconnecting"
                            );
                            backoff.reset();
                            break;
                        }
                        Some(Err(e)) => {
                            stream_metrics.connection_state.set(TRANSIENT_FAILURE);
                            stream_metrics.stream_errors_total.inc();
                            stream_metrics.reconnections_total.inc();
                            config
                                .client_provider
                                .refresh_status_auth_if_needed(&e, credential_generation)
                                .await;
                            tracing::warn!(
                                error = ?e,
                                switch_id = %sample_processor.switch_id,
                                rack_id = config.client_provider.rack_id.as_ref().map(tracing::field::display),
                                "nvue_gnmi SAMPLE: stream error, reconnecting"
                            );
                            break;
                        }
                    }
                }
            }
        }

        if cancel_token
            .run_until_cancelled(tokio::time::sleep(backoff.next_delay()))
            .await
            .is_none()
        {
            stream_metrics.connection_state.set(SHUTDOWN);
            return;
        }
    }
}

async fn gnmi_on_change_task(
    cancel_token: CancellationToken,
    client_provider: GnmiClientProvider,
    stream_metrics: GnmiStreamMetrics,
    on_change_processor: GnmiOnChangeProcessor,
) {
    let mut backoff = ExponentialBackoff::new(&BackoffConfig {
        initial: Duration::from_secs(2),
        max: Duration::from_secs(60),
    });
    let prefix = system_events_prefix();
    let paths = system_events_subscribe_path();

    loop {
        stream_metrics.connection_state.set(CONNECTING);

        let Some(stream) = cancel_token
            .run_until_cancelled(subscribe_on_change_with_cached_credentials(
                &client_provider,
                &prefix,
                &paths,
            ))
            .await
        else {
            stream_metrics.connection_state.set(SHUTDOWN);
            return;
        };

        match stream {
            Err(e) => {
                stream_metrics.connection_state.set(TRANSIENT_FAILURE);
                stream_metrics.reconnections_total.inc();
                if let Some(credential_generation) = e.credential_generation {
                    client_provider
                        .refresh_auth_if_needed(&e.error, credential_generation)
                        .await;
                }
                tracing::warn!(
                    error = ?e.error,
                    switch_id = %on_change_processor.switch_id,
                    stream = %on_change_processor.collector_name,
                    rack_id = client_provider.rack_id.as_ref().map(tracing::field::display),
                    "nvue_gnmi ON_CHANGE: connection failed, backing off"
                );
            }
            Ok((mut stream, credential_generation)) => 'connected: {
                stream_metrics.connection_state.set(READY);
                stream_metrics
                    .connection_established_timestamp
                    .set(now_unix_secs());

                let _connection_guard =
                    StreamingConnectionGuard::inc(stream_metrics.connected.clone());

                let Some(_synchronization_guard) = await_stream_synchronization(
                    &cancel_token,
                    &mut stream,
                    &client_provider,
                    credential_generation,
                    &stream_metrics,
                    |response| {
                        on_change_processor.process_subscribe_response(response, &stream_metrics);
                        Ok(())
                    },
                )
                .await
                else {
                    break 'connected;
                };

                backoff.reset();
                tracing::info!(
                    switch_id = %on_change_processor.switch_id,
                    stream = %on_change_processor.collector_name,
                    rack_id = client_provider.rack_id.as_ref().map(tracing::field::display),
                    "nvue_gnmi ON_CHANGE: stream connected"
                );

                loop {
                    let Some(msg) = cancel_token.run_until_cancelled(stream.message()).await else {
                        stream_metrics.connection_state.set(SHUTDOWN);
                        tracing::info!(
                            switch_id = %on_change_processor.switch_id,
                            stream = %on_change_processor.collector_name,
                            rack_id = client_provider.rack_id.as_ref().map(tracing::field::display),
                            "nvue_gnmi ON_CHANGE: cancelled, shutting down"
                        );
                        return;
                    };

                    match msg {
                        Ok(Some(resp)) => {
                            on_change_processor.process_subscribe_response(&resp, &stream_metrics);
                        }
                        Ok(None) => {
                            stream_metrics.connection_state.set(IDLE);
                            stream_metrics.server_initiated_closures_total.inc();
                            tracing::info!(
                                switch_id = %on_change_processor.switch_id,
                                stream = %on_change_processor.collector_name,
                                rack_id = client_provider.rack_id.as_ref().map(tracing::field::display),
                                "nvue_gnmi ON_CHANGE: stream closed by server, reconnecting"
                            );
                            backoff.reset();
                            break;
                        }
                        Err(e) => {
                            stream_metrics.connection_state.set(TRANSIENT_FAILURE);
                            stream_metrics.stream_errors_total.inc();
                            stream_metrics.reconnections_total.inc();
                            client_provider
                                .refresh_status_auth_if_needed(&e, credential_generation)
                                .await;
                            tracing::warn!(
                                error = ?e,
                                switch_id = %on_change_processor.switch_id,
                                stream = %on_change_processor.collector_name,
                                rack_id = client_provider.rack_id.as_ref().map(tracing::field::display),
                                "nvue_gnmi ON_CHANGE: stream error, reconnecting"
                            );
                            break;
                        }
                    }
                }
            }
        }

        if cancel_token
            .run_until_cancelled(tokio::time::sleep(backoff.next_delay()))
            .await
            .is_none()
        {
            stream_metrics.connection_state.set(SHUTDOWN);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::net::{IpAddr, Ipv4Addr};
    use std::pin::Pin;
    use std::sync::Mutex as StdMutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use carbide_test_support::Outcome::*;
    use carbide_test_support::{Case, check_cases_async};
    use mac_address::MacAddress;

    use super::*;
    use crate::bmc::{BoxFuture, CredentialProvider};
    use crate::endpoint::{BmcAddr, BmcCredentials};

    enum ProviderResponse {
        Credentials(BmcCredentials),
        Error(&'static str),
        Pending,
    }

    enum RefreshInput {
        ReconnectError(HealthError),
        StreamStatus(tonic::Status),
    }

    struct RecordingProvider {
        calls: AtomicUsize,
        observed_addrs: StdMutex<Vec<BmcAddr>>,
        response: ProviderResponse,
    }

    impl RecordingProvider {
        fn new(credentials: BmcCredentials) -> Arc<Self> {
            Self::responding_with(ProviderResponse::Credentials(credentials))
        }

        fn responding_with(response: ProviderResponse) -> Arc<Self> {
            Arc::new(Self {
                calls: AtomicUsize::new(0),
                observed_addrs: StdMutex::new(Vec::new()),
                response,
            })
        }
    }

    impl CredentialProvider for RecordingProvider {
        fn fetch_credentials<'a>(
            &'a self,
            endpoint: &'a BmcAddr,
        ) -> BoxFuture<'a, Result<BmcCredentials, HealthError>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.observed_addrs.lock().unwrap().push(endpoint.clone());
            let response = match &self.response {
                ProviderResponse::Credentials(credentials) => Ok(credentials.clone()),
                ProviderResponse::Error(message) => {
                    Err(HealthError::GenericError((*message).to_string()))
                }
                ProviderResponse::Pending => return Box::pin(std::future::pending()),
            };
            Box::pin(async move { response })
        }
    }

    #[test]
    fn extended_stream_metrics_share_families_by_subscription_label() {
        let registry = prometheus::Registry::new();

        for subscription in ["first", "second"] {
            GnmiStreamMetrics::new(
                &registry,
                "test",
                "_extended",
                HashMap::from([("subscription".to_string(), subscription.to_string())]),
            )
            .expect("distinct subscription labels should share metric families");
        }

        let families = registry.gather();

        let connection = families
            .iter()
            .find(|family| family.name() == "test_nvue_gnmi_extended_connection_state")
            .expect("extended connection metric should be registered");

        assert_eq!(connection.get_metric().len(), 2);
    }

    fn test_addr() -> BmcAddr {
        BmcAddr {
            ip: IpAddr::V4(Ipv4Addr::new(10, 0, 0, 9)),
            port: Some(443),
            mac: "55:66:77:88:99:cc"
                .parse::<MacAddress>()
                .expect("valid mac"),
        }
    }

    #[derive(Debug, PartialEq)]
    struct CredentialFetchProjection {
        result: Result<(Option<String>, Option<String>), String>,
        calls: usize,
        observed_addrs: Vec<(IpAddr, Option<u16>, String)>,
    }

    fn expected_credential_fetch(
        result: Result<(Option<String>, Option<String>), String>,
    ) -> CredentialFetchProjection {
        let addr = test_addr();
        CredentialFetchProjection {
            result,
            calls: 1,
            observed_addrs: vec![(addr.ip, addr.port, addr.mac.to_string())],
        }
    }

    fn project_credential_fetch(
        provider: &RecordingProvider,
        result: Result<GnmiUsernamePassword, HealthError>,
    ) -> CredentialFetchProjection {
        let result = result
            .map(|credentials| (credentials.username, credentials.password))
            .map_err(|error| error.to_string());
        let observed_addrs = provider
            .observed_addrs
            .lock()
            .unwrap()
            .iter()
            .map(|addr| (addr.ip, addr.port, addr.mac.to_string()))
            .collect();

        CredentialFetchProjection {
            result,
            calls: provider.calls.load(Ordering::SeqCst),
            observed_addrs,
        }
    }

    fn test_client_provider(provider: Arc<dyn CredentialProvider>) -> GnmiClientProvider {
        let addr = test_addr();
        GnmiClientProvider {
            switch_id: "switch-1".to_string(),
            rack_id: None,
            switch_connect_host: addr.ip.to_string(),
            port: 9339,
            request_timeout: Duration::from_secs(1),
            dangerously_skip_tls_verification: false,
            tls_config: None,
            credentials: Arc::new(GnmiCredentialCache::new(provider, addr)),
        }
    }

    fn test_labels() -> HashMap<String, String> {
        HashMap::from([
            ("switch_id".to_string(), "test-switch".to_string()),
            ("switch_ip".to_string(), "10.0.0.1".to_string()),
        ])
    }

    #[tokio::test(start_paused = true)]
    async fn initial_stream_without_sync_times_out() {
        let mut responses = tokio_stream::pending();
        let metrics = test_gnmi_stream_metrics();

        let client_provider = test_client_provider(RecordingProvider::responding_with(
            ProviderResponse::Pending,
        ));

        let result = await_stream_synchronization(
            &CancellationToken::new(),
            &mut responses,
            &client_provider,
            0,
            &metrics,
            |_| Ok(()),
        )
        .await;

        assert!(result.is_none());
        assert_eq!(metrics.connection_state.get(), TRANSIENT_FAILURE);
        assert_eq!(metrics.connected.get(), 0);
        assert_eq!(metrics.synchronized.get(), 0);
        assert_eq!(metrics.stream_errors_total.get(), 1.0);
        assert_eq!(metrics.reconnections_total.get(), 1.0);
    }

    #[tokio::test]
    async fn initial_stream_eof_is_recorded_as_server_closure() {
        let mut responses = tokio_stream::empty();
        let metrics = test_gnmi_stream_metrics();

        let client_provider = test_client_provider(RecordingProvider::responding_with(
            ProviderResponse::Pending,
        ));

        let result = await_stream_synchronization(
            &CancellationToken::new(),
            &mut responses,
            &client_provider,
            0,
            &metrics,
            |_| Ok(()),
        )
        .await;

        assert!(result.is_none());
        assert_eq!(metrics.connection_state.get(), IDLE);
        assert_eq!(metrics.connected.get(), 0);
        assert_eq!(metrics.synchronized.get(), 0);
        assert_eq!(metrics.server_initiated_closures_total.get(), 1.0);
        assert_eq!(metrics.stream_errors_total.get(), 0.0);
        assert_eq!(metrics.reconnections_total.get(), 0.0);
    }

    #[tokio::test]
    #[allow(deprecated, reason = "exercise legacy in-band gNMI error handling")]
    async fn initial_stream_auth_status_refreshes_credentials() {
        let provider = RecordingProvider::new(BmcCredentials::UsernamePassword {
            username: "nvos-admin".to_string(),
            password: Some("nvos-secret".to_string()),
        });

        let client_provider = test_client_provider(provider.clone());

        let (_client, credential_generation) =
            client_provider.new_client().await.expect("client builds");

        let mut responses = tokio_stream::once(Ok(proto::SubscribeResponse {
            response: Some(proto::subscribe_response::Response::Error(proto::Error {
                code: tonic::Code::Unauthenticated as u32,
                message: "expired gNMI credentials".to_string(),
                ..Default::default()
            })),
            ..Default::default()
        }));

        let metrics = test_gnmi_stream_metrics();

        let result = await_stream_synchronization(
            &CancellationToken::new(),
            &mut responses,
            &client_provider,
            credential_generation,
            &metrics,
            |_| Ok(()),
        )
        .await;

        assert!(result.is_none());
        assert_eq!(metrics.connection_state.get(), TRANSIENT_FAILURE);
        assert_eq!(metrics.connected.get(), 0);
        assert_eq!(metrics.synchronized.get(), 0);
        assert_eq!(metrics.stream_errors_total.get(), 1.0);
        assert_eq!(metrics.reconnections_total.get(), 1.0);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn initial_stream_processes_updates_until_true_sync() {
        let mut responses = tokio_stream::iter([
            Ok(proto::SubscribeResponse {
                response: Some(proto::subscribe_response::Response::Update(
                    proto::Notification::default(),
                )),
                ..Default::default()
            }),
            Ok(proto::SubscribeResponse {
                response: Some(proto::subscribe_response::Response::SyncResponse(false)),
                ..Default::default()
            }),
            Ok(proto::SubscribeResponse {
                response: Some(proto::subscribe_response::Response::SyncResponse(true)),
                ..Default::default()
            }),
        ]);

        let metrics = test_gnmi_stream_metrics();

        let client_provider = test_client_provider(RecordingProvider::responding_with(
            ProviderResponse::Pending,
        ));

        let mut processed_updates = 0;

        let result = await_stream_synchronization(
            &CancellationToken::new(),
            &mut responses,
            &client_provider,
            0,
            &metrics,
            |_| {
                processed_updates += 1;
                Ok(())
            },
        )
        .await;

        assert!(result.is_some());
        assert_eq!(processed_updates, 1);
        assert_eq!(metrics.synchronized.get(), 1);
    }

    #[tokio::test]
    async fn initial_stream_cancellation_sets_shutdown() {
        let cancel_token = CancellationToken::new();
        cancel_token.cancel();

        let mut responses = tokio_stream::pending();
        let metrics = test_gnmi_stream_metrics();

        let client_provider = test_client_provider(RecordingProvider::responding_with(
            ProviderResponse::Pending,
        ));

        let result = await_stream_synchronization(
            &cancel_token,
            &mut responses,
            &client_provider,
            0,
            &metrics,
            |_| Ok(()),
        )
        .await;

        assert!(result.is_none());
        assert_eq!(metrics.connection_state.get(), SHUTDOWN);
        assert_eq!(metrics.synchronized.get(), 0);
        assert_eq!(metrics.stream_errors_total.get(), 0.0);
        assert_eq!(metrics.reconnections_total.get(), 0.0);
    }

    #[tokio::test(start_paused = true)]
    async fn sample_task_retries_when_initial_stream_does_not_synchronize() {
        type TestStream =
            Pin<Box<dyn Stream<Item = Result<proto::SubscribeResponse, tonic::Status>> + Send>>;

        let cancel_token = CancellationToken::new();

        let client_provider = test_client_provider(RecordingProvider::responding_with(
            ProviderResponse::Pending,
        ));

        let config = GnmiStreamConfig {
            client_provider,
            paths: Vec::new(),
            sample_interval_nanos: 1,
        };

        let metrics = test_gnmi_stream_metrics();

        let processor = GnmiSampleProcessor {
            data_sink: None,
            event_context: EventContext {
                endpoint_key: "test-endpoint".to_string(),
                addr: test_addr(),
                collector_type: NVUE_GNMI_SAMPLE_STREAM_ID,
                metadata: None,
                rack_id: None,
                labels: Default::default(),
            },
            switch_id: "switch-1".to_string(),
        };

        let synchronized = proto::SubscribeResponse {
            response: Some(proto::subscribe_response::Response::SyncResponse(true)),
            ..Default::default()
        };

        let stalled: TestStream = Box::pin(tokio_stream::pending());

        let synchronized: TestStream =
            Box::pin(tokio_stream::once(Ok(synchronized)).chain(tokio_stream::pending()));

        let mut streams = VecDeque::from([stalled, synchronized]);
        let (attempt_tx, mut attempt_rx) = tokio::sync::mpsc::unbounded_channel();

        let run = run_gnmi_sample_task(&cancel_token, &config, &metrics, &processor, || {
            attempt_tx.send(()).expect("attempt receiver remains open");
            std::future::ready(Ok((
                streams.pop_front().expect("test stream is available"),
                0,
            )))
        });

        let observe = async {
            attempt_rx.recv().await.expect("first subscription attempt");

            tokio::task::yield_now().await;
            assert_eq!(metrics.connection_state.get(), READY);
            assert_eq!(metrics.connected.get(), 1);
            assert_eq!(metrics.synchronized.get(), 0);

            attempt_rx.recv().await.expect("retry subscription attempt");

            tokio::task::yield_now().await;
            assert_eq!(metrics.connection_state.get(), READY);
            assert_eq!(metrics.connected.get(), 1);
            assert_eq!(metrics.synchronized.get(), 1);
            assert_eq!(metrics.reconnections_total.get(), 1.0);

            cancel_token.cancel();
        };

        tokio::join!(run, observe);

        assert_eq!(metrics.connection_state.get(), SHUTDOWN);
        assert_eq!(metrics.connected.get(), 0);
        assert_eq!(metrics.synchronized.get(), 0);
    }

    #[test]
    fn test_stream_metrics_registers_all_counters() {
        let registry = prometheus::Registry::new();
        let metrics = GnmiStreamMetrics::new(&registry, "test", "", test_labels()).unwrap();

        metrics.reconnections_total.inc();
        assert_eq!(metrics.reconnections_total.get(), 1.0);

        metrics.server_initiated_closures_total.inc();
        assert_eq!(metrics.server_initiated_closures_total.get(), 1.0);

        metrics.stream_errors_total.inc();
        assert_eq!(metrics.stream_errors_total.get(), 1.0);
    }

    #[test]
    fn test_stream_metrics_server_closures_independent_from_reconnections() {
        let registry = prometheus::Registry::new();
        let metrics = GnmiStreamMetrics::new(&registry, "test", "", test_labels()).unwrap();

        metrics.server_initiated_closures_total.inc();
        metrics.server_initiated_closures_total.inc();
        assert_eq!(metrics.server_initiated_closures_total.get(), 2.0);
        assert_eq!(metrics.reconnections_total.get(), 0.0);

        metrics.reconnections_total.inc();
        assert_eq!(metrics.reconnections_total.get(), 1.0);
        assert_eq!(metrics.server_initiated_closures_total.get(), 2.0);
    }

    #[test]
    fn test_stream_metrics_duplicate_registration_fails() {
        let registry = prometheus::Registry::new();
        let _ = GnmiStreamMetrics::new(&registry, "test", "", test_labels()).unwrap();
        let result = GnmiStreamMetrics::new(&registry, "test", "", test_labels());
        assert!(result.is_err());
    }

    #[test]
    fn test_stream_metrics_distinct_stream_names_coexist() {
        let registry = prometheus::Registry::new();
        let sample = GnmiStreamMetrics::new(&registry, "test", "", test_labels()).unwrap();
        let events_labels = HashMap::from([
            ("switch_id".to_string(), "test-switch".to_string()),
            ("switch_ip".to_string(), "10.0.0.2".to_string()),
        ]);
        let events = GnmiStreamMetrics::new(&registry, "test", "_events", events_labels).unwrap();

        sample.server_initiated_closures_total.inc();
        assert_eq!(sample.server_initiated_closures_total.get(), 1.0);
        assert_eq!(events.server_initiated_closures_total.get(), 0.0);
    }

    #[tokio::test]
    async fn gnmi_credential_fetch_cases() {
        check_cases_async(
            [
                Case {
                    scenario: "username and password",
                    input: ProviderResponse::Credentials(BmcCredentials::UsernamePassword {
                        username: "nvos-admin".to_string(),
                        password: Some("nvos-secret".to_string()),
                    }),
                    expect: Yields(expected_credential_fetch(Ok((
                        Some("nvos-admin".to_string()),
                        Some("nvos-secret".to_string()),
                    )))),
                },
                Case {
                    scenario: "username without password",
                    input: ProviderResponse::Credentials(BmcCredentials::UsernamePassword {
                        username: "nvos-admin".to_string(),
                        password: None,
                    }),
                    expect: Yields(expected_credential_fetch(Ok((
                        Some("nvos-admin".to_string()),
                        None,
                    )))),
                },
                Case {
                    scenario: "session token",
                    input: ProviderResponse::Credentials(BmcCredentials::SessionToken {
                        token: "redfish-session-token".to_string(),
                    }),
                    expect: Yields(expected_credential_fetch(Err(
                        "gNMI error: NVUE gNMI collector requires username/password credentials"
                            .to_string(),
                    ))),
                },
                Case {
                    scenario: "credential provider error",
                    input: ProviderResponse::Error("credential provider unavailable"),
                    expect: Yields(expected_credential_fetch(Err(
                        "generic error: credential provider unavailable".to_string(),
                    ))),
                },
            ],
            |response| async move {
                let addr = test_addr();
                let provider = RecordingProvider::responding_with(response);
                let result = fetch_gnmi_username_password(provider.clone(), &addr).await;

                Ok::<_, ()>(project_credential_fetch(&provider, result))
            },
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn gnmi_credential_fetch_times_out() {
        let addr = test_addr();
        let provider = RecordingProvider::responding_with(ProviderResponse::Pending);
        let fetch_provider = provider.clone();
        let fetch_addr = addr.clone();
        let fetch =
            tokio::spawn(
                async move { fetch_gnmi_username_password(fetch_provider, &fetch_addr).await },
            );

        tokio::time::advance(CREDENTIAL_REFRESH_TIMEOUT + Duration::from_secs(1)).await;
        let result = fetch.await.expect("credential fetch task should join");

        assert_eq!(
            project_credential_fetch(&provider, result),
            expected_credential_fetch(Err(format!(
                "gNMI error: Timed out after {}s fetching NVUE gNMI credentials",
                CREDENTIAL_REFRESH_TIMEOUT.as_secs()
            )))
        );
    }

    #[tokio::test]
    async fn gnmi_auth_refresh_cases() {
        check_cases_async(
            [
                Case {
                    scenario: "reconnect unauthenticated",
                    input: RefreshInput::ReconnectError(HealthError::GnmiStatus(
                        tonic::Status::unauthenticated("expired gNMI credentials"),
                    )),
                    expect: Yields(2),
                },
                Case {
                    scenario: "reconnect permission denied",
                    input: RefreshInput::ReconnectError(HealthError::GnmiStatus(
                        tonic::Status::permission_denied("rejected gNMI credentials"),
                    )),
                    expect: Yields(2),
                },
                Case {
                    scenario: "reconnect unavailable",
                    input: RefreshInput::ReconnectError(HealthError::GnmiStatus(
                        tonic::Status::unavailable("connection timed out"),
                    )),
                    expect: Yields(1),
                },
                Case {
                    scenario: "non-gNMI error",
                    input: RefreshInput::ReconnectError(HealthError::GenericError(
                        "connection setup failed".to_string(),
                    )),
                    expect: Yields(1),
                },
                Case {
                    scenario: "stream unauthenticated",
                    input: RefreshInput::StreamStatus(tonic::Status::unauthenticated(
                        "expired gNMI credentials",
                    )),
                    expect: Yields(2),
                },
                Case {
                    scenario: "stream permission denied",
                    input: RefreshInput::StreamStatus(tonic::Status::permission_denied(
                        "rejected gNMI credentials",
                    )),
                    expect: Yields(2),
                },
                Case {
                    scenario: "stream unavailable",
                    input: RefreshInput::StreamStatus(tonic::Status::unavailable(
                        "connection timed out",
                    )),
                    expect: Yields(1),
                },
            ],
            |input| async move {
                let provider = RecordingProvider::new(BmcCredentials::UsernamePassword {
                    username: "nvos-admin".to_string(),
                    password: Some("nvos-secret".to_string()),
                });
                let client_provider = test_client_provider(provider.clone());
                let (_client, generation) =
                    client_provider.new_client().await.expect("client builds");

                match input {
                    RefreshInput::ReconnectError(error) => {
                        client_provider
                            .refresh_auth_if_needed(&error, generation)
                            .await;
                    }
                    RefreshInput::StreamStatus(status) => {
                        client_provider
                            .refresh_status_auth_if_needed(&status, generation)
                            .await;
                    }
                }
                client_provider
                    .new_client()
                    .await
                    .expect("cached credentials are reused");

                Ok::<_, ()>(provider.calls.load(Ordering::SeqCst))
            },
        )
        .await;
    }

    #[tokio::test]
    async fn gnmi_client_provider_ignores_stale_refresh_generation() {
        let provider = RecordingProvider::new(BmcCredentials::UsernamePassword {
            username: "nvos-admin".to_string(),
            password: Some("nvos-secret".to_string()),
        });
        let client_provider = test_client_provider(provider.clone());
        let (_client, generation) = client_provider.new_client().await.expect("client builds");

        client_provider
            .refresh_auth_if_needed(
                &HealthError::GnmiStatus(tonic::Status::unauthenticated(
                    "expired gNMI credentials",
                )),
                generation,
            )
            .await;
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "auth failure should refresh cached credentials"
        );

        client_provider
            .refresh_auth_if_needed(
                &HealthError::GnmiStatus(tonic::Status::unauthenticated(
                    "expired gNMI credentials",
                )),
                generation,
            )
            .await;
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "stale stream generations should not trigger duplicate credential refreshes"
        );

        client_provider
            .new_client()
            .await
            .expect("refreshed credentials are reused");
        assert_eq!(
            provider.calls.load(Ordering::SeqCst),
            2,
            "reconnect after refresh should reuse refreshed credentials"
        );
    }
}
