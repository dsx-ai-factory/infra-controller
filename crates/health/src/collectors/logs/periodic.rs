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

//! Periodic collection of retained Redfish logs using one anchor per service.
//!
//! Numeric entry IDs must increase between resets, and existing entries must be
//! immutable. Each poll reads the saved anchor; a missing or changed mapped record
//! triggers replay of retained history. Changes below an unchanged anchor are
//! outside the recovery contract.
//!
//! Scans retain member URIs and numeric IDs temporarily, process one entry body at
//! a time, and persist only the highest entry's identity. Failed scans preserve
//! the saved anchor, but records already accepted by a sink can replay on retry.

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::io::{BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use nv_redfish::ServiceRoot;
use nv_redfish::core::{Bmc, EntityTypeRef, FilterQuery, ODataETag, ODataId, ReferenceLeaf};
use nv_redfish::log_service::LogService;
use nv_redfish::schema::log_entry::LogEntry;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use url::Url;

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
use crate::limiter::BucketLimiter;
use crate::sink::{CollectorEvent, DataSink, EventContext, LogRecord};

/// SHA-256 identity of the mapped log content, independent of attribute order.
type Fingerprint = [u8; 32];

/// Settings for a periodic log collector attached to one BMC endpoint.
pub struct LogsCollectorConfig {
    /// Checkpoint path for this endpoint. Its parent directory must exist.
    pub state_file_path: PathBuf,

    /// Interval between discovery refreshes; anchor checks run on every poll.
    pub service_refresh_interval: Duration,

    /// Destination for collected records. `None` still advances collection state.
    pub data_sink: Option<Arc<dyn DataSink>>,

    /// Cursor transferred from SSE when auto mode downgrades. `Some`, including
    /// an empty map, replaces persisted periodic state on initial startup.
    pub initial_last_seen_ids: Option<HashMap<ODataId, i32>>,

    /// Attach Redfish diagnostic payloads to emitted log records.
    pub include_diagnostics: bool,

    /// Case-sensitive substrings matched against discovered LogService IDs.
    /// Empty patterns are ignored; an empty list collects from every service.
    pub exclude_services: Vec<String>,

    /// Skip only the first successful baseline of a service without saved state.
    /// Failed baselines and recovery checkpoints replay retained history.
    pub skip_initial_history: bool,
}

/// Identity of the highest successfully scanned entry, including reused-ID content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LogAnchor {
    id: i64,
    uri: ODataId,
    fingerprint: Fingerprint,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ServiceLogState {
    /// `None` represents an empty source or a service that needs a full scan.
    anchor: Option<LogAnchor>,

    /// Remembers that a nonempty initial scan skipped history but found no numeric ID.
    /// Without this flag, `anchor: None` would replay the skipped records on later
    /// polls or after restart. An empty initial scan leaves the flag unset so its
    /// first future entry is collected. Saving a numeric anchor clears the flag
    /// so reset recovery can replay all retained records, including nonnumeric IDs.
    #[serde(default)]
    skip_nonnumeric: bool,

    /// Reprobe filter support after restart; this hint is not collection progress.
    #[serde(skip)]
    filter_disabled: bool,
}

/// Checkpoint shape; required fields prevent incompatible state from being trusted.
#[derive(Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct PersistentState {
    services: HashMap<ODataId, ServiceLogState>,

    /// Invalid checkpoints replay history even when initial-history skipping is enabled.
    replay_new_services: bool,
}

/// Numeric-only checkpoints identify services but cannot prove entry continuity.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyState {
    last_seen_ids: HashMap<ODataId, i32>,
}

/// Navigation data for one response in a paginated LogEntry collection.
///
/// nv-redfish's `LogEntryCollection` omits `Members@odata.count` and
/// `Members@odata.nextLink`. Its `NavProperty<LogEntry>` members also deserialize
/// expanded entry bodies. `ReferenceLeaf` keeps only IDs, so a scan can validate
/// the entire page chain before reading individual entries.
#[derive(Deserialize)]
struct LogEntryPage {
    #[serde(rename = "@odata.id")]
    odata_id: ODataId,
    #[serde(rename = "Members")]
    members: Vec<ReferenceLeaf>,
    #[serde(rename = "Members@odata.count")]
    member_count: Option<usize>,
    #[serde(rename = "Members@odata.nextLink")]
    next_link: Option<ODataId>,
}

impl EntityTypeRef for LogEntryPage {
    fn odata_id(&self) -> &ODataId {
        &self.odata_id
    }

    fn etag(&self) -> Option<&ODataETag> {
        None
    }
}

/// Discovery cache containing only SDK navigation IDs, not expanded log bodies.
#[derive(Clone)]
struct ServiceRef {
    id: ODataId,
    entries: ODataId,
}

struct LogsCollectorState {
    discovered_services: Vec<ServiceRef>,
    last_service_refresh: Instant,
    persistent: PersistentState,
}

/// Collects retained logs from a single BMC endpoint and checkpoints each service.
///
/// Page, entry, continuity, and sink errors leave that service's anchor unchanged.
/// Checkpoint write errors preserve live progress and are returned to the caller.
pub struct LogsCollector<B: Bmc> {
    endpoint: Arc<BmcEndpoint>,
    bmc: Arc<B>,
    event_context: EventContext,
    state_file_path: PathBuf,
    state: Option<LogsCollectorState>,
    service_refresh_interval: Duration,
    data_sink: Option<Arc<dyn DataSink>>,
    initial_last_seen_ids: Option<HashMap<ODataId, i32>>,
    include_diagnostics: bool,
    exclude_services: Vec<String>,
    skip_initial_history: bool,
    read_limiter: BucketLimiter,
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
            initial_last_seen_ids: config.initial_last_seen_ids,
            include_diagnostics: config.include_diagnostics,
            exclude_services: config.exclude_services,
            skip_initial_history: config.skip_initial_history,
            // Page and entry reads share pacing; SDK retries remain transport-owned.
            read_limiter: BucketLimiter::new(1, Duration::from_millis(200), Duration::ZERO),
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
    /// Loads anchors or marks numeric/SSE state for replay; absent files allow baselining.
    async fn load_persistent_state(&mut self) -> Result<PersistentState, HealthError> {
        let mut state = PersistentState::default();

        let ids = if let Some(ids) = self.initial_last_seen_ids.take() {
            ids
        } else {
            let bytes = match tokio::fs::read(&self.state_file_path).await {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(state),
                Err(error) => {
                    return Err(HealthError::GenericError(format!(
                        "failed to read log state: {error}"
                    )));
                }
            };

            if let Ok(saved) = serde_json::from_slice::<PersistentState>(&bytes) {
                state = saved;
                HashMap::new()
            } else if let Ok(legacy) = serde_json::from_slice::<LegacyState>(&bytes) {
                legacy.last_seen_ids
            } else {
                tracing::warn!("Invalid log checkpoint; replaying retained history");
                state.replay_new_services = true;
                HashMap::new()
            }
        };

        state
            .services
            .extend(ids.into_keys().map(|id| (id, ServiceLogState::default())));

        Ok(state)
    }

    /// Replaces the checkpoint with a complete file without changing live progress.
    async fn save_persistent_state(&self) -> Result<(), HealthError> {
        let Some(state) = &self.state else {
            return Ok(());
        };

        // The worker owns its snapshot and temporary file, so cancellation drops
        // unfinished output. The same-directory rename replaces the complete file.
        let persistent = state.persistent.clone();
        let state_path = self.state_file_path.clone();

        let temp_file = tokio::task::spawn_blocking(move || -> std::io::Result<_> {
            let parent = state_path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));

            let mut file = tempfile::NamedTempFile::new_in(parent)?;

            {
                let mut writer = BufWriter::new(file.as_file_mut());
                serde_json::to_writer(&mut writer, &persistent)?;
                writer.flush()?;
            }

            Ok(file)
        })
        .await?
        .map_err(|error| {
            HealthError::GenericError(format!("failed to write log state: {error}"))
        })?;

        tokio::fs::rename(temp_file.path(), &self.state_file_path)
            .await
            .map_err(|error| {
                HealthError::GenericError(format!("failed to replace log state: {error}"))
            })?;

        Ok(())
    }

    /// True if this service's odata id matches any configured exclude substring.
    fn is_excluded(&self, service_id: &str) -> bool {
        service_is_excluded(&self.exclude_services, service_id)
    }

    async fn discover_log_services(&self) -> Result<Vec<ServiceRef>, HealthError> {
        let service_root = ServiceRoot::new(self.bmc.clone()).await?;

        let mut services = Vec::new();
        let mut seen_ids = HashSet::new();
        let mut excluded_count = 0usize;

        let consider = |service: LogService<B>,
                        services: &mut Vec<ServiceRef>,
                        seen_ids: &mut HashSet<ODataId>,
                        excluded_count: &mut usize| {
            let raw = service.raw();

            if self.is_excluded(&raw.odata_id.to_string()) {
                *excluded_count += 1;
                return;
            }

            if seen_ids.insert(raw.odata_id.clone())
                && let Some(entries) = &raw.entries
            {
                services.push(ServiceRef {
                    id: raw.odata_id.clone(),
                    entries: entries.id().clone(),
                });
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

                    if let Some(state) = &mut self.state {
                        state.discovered_services = services;
                        state.last_service_refresh = Instant::now();
                    } else {
                        let persistent = self.load_persistent_state().await?;

                        self.state = Some(LogsCollectorState {
                            discovered_services: services,
                            last_service_refresh: Instant::now(),
                            persistent,
                        });
                    }

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

        let Some(state) = self.state.as_ref() else {
            return Ok((0, 0));
        };

        let services = state.discovered_services.clone();
        let mut counts = (0, 0);

        for service in services {
            let Some(state) = self.state.as_mut() else {
                break;
            };

            let baseline = !state.persistent.services.contains_key(&service.id)
                && self.skip_initial_history
                && !state.persistent.replay_new_services;

            if baseline {
                // Persist service identity before skipping: an interrupted baseline must replay.
                state
                    .persistent
                    .services
                    .insert(service.id.clone(), ServiceLogState::default());

                self.save_persistent_state().await?;
            }

            let Some(state) = self.state.as_ref() else {
                break;
            };

            let empty = ServiceLogState::default();
            let previous = state.persistent.services.get(&service.id).unwrap_or(&empty);
            let mut filter_disabled = previous.filter_disabled;

            let result = self
                .scan_service(&service, previous, baseline, &mut filter_disabled)
                .await;

            let Some(state) = self.state.as_mut() else {
                break;
            };

            match result {
                Ok((saved, emitted)) => {
                    state.persistent.services.insert(service.id, saved);
                    counts.0 += emitted;
                }
                Err(error) => {
                    // Filter support is independent of checkpoint progress.
                    state
                        .persistent
                        .services
                        .entry(service.id.clone())
                        .or_default()
                        .filter_disabled = filter_disabled;

                    counts.1 += 1;
                    tracing::warn!(endpoint = ?self.endpoint.addr, service_id = %service.id, %error, "Log scan failed; retaining previous checkpoint");
                }
            }
        }

        Ok(counts)
    }

    /// Maps one SDK entry and hashes its content without retaining the entry body.
    /// Nonnumeric entries have no anchor and are emitted only during full scans.
    async fn read_entry(
        &self,
        service: &ServiceRef,
        uri: &ODataId,
    ) -> Result<(Option<LogAnchor>, Box<LogRecord>), HealthError> {
        self.read_limiter.acquire().await;

        let entry = self
            .bmc
            .get::<LogEntry>(uri)
            .await
            .map_err(|error| HealthError::BmcError(Box::new(error)))?;

        let machine_id = self.event_context.machine_id().map(|id| id.to_string());

        let CollectorEvent::Log(record) = entry_to_log(
            &entry,
            machine_id.as_deref(),
            &service.id.to_string(),
            self.include_diagnostics,
        ) else {
            return Err(HealthError::GenericError(
                "expected mapped log record".to_string(),
            ));
        };

        let anchor = match entry.id.parse::<i64>() {
            Ok(id) => Some(LogAnchor {
                id,
                uri: uri.clone(),
                fingerprint: entry_fingerprint(&record)?,
            }),
            Err(_) => None,
        };

        Ok((anchor, record))
    }

    async fn verify_anchor(
        &self,
        service: &ServiceRef,
        anchor: &LogAnchor,
    ) -> Result<(), HealthError> {
        let (current, _) = self.read_entry(service, &anchor.uri).await?;

        if current.as_ref() != Some(anchor) {
            return Err(HealthError::GenericError(format!(
                "log entry {} changed during collection",
                anchor.uri
            )));
        }

        Ok(())
    }

    /// Collects navigation IDs, following even empty pages with continuations.
    /// Paginated responses require a stable count that matches the complete walk.
    async fn collect_entry_uris(
        &self,
        base: &Url,
        entries: &ODataId,
        cursor: Option<i64>,
        filter_disabled: &mut bool,
    ) -> Result<Vec<ODataId>, HealthError> {
        let mut uri = resolve_log_uri(base, "/", &entries.to_string())?;
        let mut visited = HashSet::new();
        let mut member_count = None;
        // Gather navigation IDs before reading entries; evicted members fail the scan.
        let mut members = Vec::new();

        loop {
            if !visited.insert(uri.clone()) {
                return Err(HealthError::GenericError(
                    "log collection repeated a continuation".to_string(),
                ));
            }

            self.read_limiter.acquire().await;

            let result = if visited.len() == 1
                && !*filter_disabled
                && let Some(cursor) = cursor
            {
                self.bmc
                    .filter::<LogEntryPage>(&uri, FilterQuery::gt(&"Id", cursor))
                    .await
            } else {
                self.bmc.get::<LogEntryPage>(&uri).await
            };

            let page = match result {
                Err(error)
                    if cursor.is_some() && visited.len() == 1 && unsupported_filter(&error) =>
                {
                    *filter_disabled = true;
                    self.read_limiter.acquire().await;
                    self.bmc.get::<LogEntryPage>(&uri).await
                }
                result => result,
            }
            .map_err(|error| HealthError::BmcError(Box::new(error)))?;

            member_count = member_count.or(page.member_count);

            if page.member_count != member_count
                || (page.next_link.is_some() && member_count.is_none())
            {
                return Err(HealthError::GenericError(
                    "log collection member count changed or is missing during pagination"
                        .to_string(),
                ));
            }

            let current_uri = uri.to_string();

            for member in &page.members {
                members.push(resolve_log_uri(
                    base,
                    &current_uri,
                    &member.odata_id.to_string(),
                )?);
            }

            let Some(next) = &page.next_link else {
                break;
            };

            uri = resolve_log_uri(base, &current_uri, &next.to_string())?;
        }

        if member_count.is_some_and(|count| count != members.len()) {
            // Some BMC filters report the unfiltered count; retry without filtering.
            *filter_disabled |= cursor.is_some();

            return Err(HealthError::GenericError(
                "log collection member count does not match returned entries".to_string(),
            ));
        }

        Ok(members)
    }

    /// Returns a candidate anchor only after the complete scan and continuity checks.
    /// Sink handoff precedes commit, so a failed scan can replay an accepted prefix.
    async fn scan_service(
        &self,
        service: &ServiceRef,
        previous: &ServiceLogState,
        baseline: bool,
        filter_disabled: &mut bool,
    ) -> Result<(ServiceLogState, usize), HealthError> {
        let base = self
            .endpoint
            .addr
            .to_url()
            .map_err(|e| HealthError::GenericError(format!("invalid BMC endpoint: {e}")))?;

        let mut original = None;

        // Missing or changed anchors require replay. Other read errors preserve
        // progress because authentication or transport failures do not prove a reset.
        if let Some(anchor) = &previous.anchor {
            let saved_uri = resolve_log_uri(&base, "/", &anchor.uri.to_string())?;

            match self.read_entry(service, &saved_uri).await {
                Ok((Some(current), _)) if current == *anchor => original = Some(anchor),
                Ok(_) => {}
                Err(error) if log_response_status(&error) == Some(http::StatusCode::NOT_FOUND) => {}
                Err(error) => return Err(error),
            }
        }

        // Freeze eligibility before traversal; page order must not advance the cursor.
        let cursor = original.map(|anchor| anchor.id);

        let members = self
            .collect_entry_uris(&base, &service.entries, cursor, filter_disabled)
            .await?;

        // An empty baseline must still collect its first future entry.
        let skip_nonnumeric = previous.skip_nonnumeric || (baseline && !members.is_empty());

        let mut latest = original.cloned();
        let mut first = None;
        let mut emitted = 0;
        let mut entry_ids = HashSet::new();

        for member_uri in members {
            let (anchor, mut record) = self.read_entry(service, &member_uri).await?;

            if let Some(anchor) = &anchor
                && !entry_ids.insert(anchor.id)
            {
                return Err(HealthError::GenericError(
                    "log collection repeated an entry".to_string(),
                ));
            }

            if cursor.is_some_and(|cursor| anchor.as_ref().is_none_or(|anchor| anchor.id <= cursor))
                || (skip_nonnumeric && anchor.is_none())
            {
                *filter_disabled |= anchor.is_some();
                continue;
            }

            if first.is_none() {
                first = anchor.clone();
            }

            if !baseline {
                let fingerprint = match &anchor {
                    Some(anchor) => anchor.fingerprint,
                    None => entry_fingerprint(&record)?,
                };

                record.attributes.push((
                    Cow::Borrowed("entry_fingerprint"),
                    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(fingerprint),
                ));

                self.data_sink.as_ref().map_or(Ok(()), |sink| {
                    sink.try_handle_event(&self.event_context, &CollectorEvent::Log(record))
                })?;

                emitted += 1;
            }

            if let Some(anchor) = anchor
                && latest.as_ref().is_none_or(|latest| anchor.id > latest.id)
            {
                latest = Some(anchor);
            }
        }

        // Redfish pages are not an atomic snapshot. Check the highest entry first,
        // then the continuity witness so a reset between the checks fails the scan.
        let witness = original.or(first.as_ref());

        if let Some(latest) = &latest
            && Some(latest) != witness
        {
            self.verify_anchor(service, latest).await?;
        }

        if let Some(witness) = witness {
            self.verify_anchor(service, witness).await?;
        }

        let skip_nonnumeric = skip_nonnumeric && latest.is_none();

        let saved = ServiceLogState {
            anchor: latest,
            skip_nonnumeric,
            filter_disabled: *filter_disabled,
        };

        Ok((saved, emitted))
    }
}

fn log_response_status(error: &(dyn std::error::Error + 'static)) -> Option<http::StatusCode> {
    std::iter::successors(Some(error), |error| error.source()).find_map(|error| {
        match error.downcast_ref::<nv_redfish::bmc_http::reqwest::BmcError>() {
            Some(nv_redfish::bmc_http::reqwest::BmcError::InvalidResponse { status, .. }) => {
                Some(*status)
            }
            _ => None,
        }
    })
}

fn unsupported_filter(error: &(dyn std::error::Error + 'static)) -> bool {
    matches!(
        log_response_status(error),
        Some(http::StatusCode::BAD_REQUEST | http::StatusCode::NOT_IMPLEMENTED)
    )
}

/// Hashes mapped content, including diagnostics, before adding the fingerprint attribute.
/// Wire fields omitted by the mapper do not affect continuity or queue identity.
fn entry_fingerprint(record: &LogRecord) -> Result<Fingerprint, HealthError> {
    let mut attributes: Vec<_> = record.attributes.iter().collect();
    attributes.sort_unstable();

    let diagnostic = record.diagnostic_record.as_ref().map(|d| {
        let mut attributes: Vec<_> = d.attributes.iter().collect();
        attributes.sort_unstable();
        (&d.body, attributes)
    });

    let bytes = serde_json::to_vec(&(
        &record.body,
        record.severity.as_str(),
        attributes,
        diagnostic,
    ))?;

    Ok(Sha256::digest(bytes).into())
}

/// Resolves relative links and returns a same-origin path and query as an SDK ID.
/// SDK IDs are opaque strings; origin and fragment validation belongs here.
fn resolve_log_uri(base_url: &Url, current_uri: &str, link: &str) -> Result<ODataId, HealthError> {
    let next = base_url
        .join(current_uri)
        .and_then(|current| current.join(link))
        .map_err(|e| HealthError::GenericError(format!("invalid log URI: {e}")))?;

    if next.origin() != base_url.origin() || next.fragment().is_some() {
        return Err(HealthError::GenericError(format!(
            "log URI changes origin or has a fragment: {link}"
        )));
    }

    Ok(ODataId::from(next.query().map_or_else(
        || next.path().to_string(),
        |query| format!("{}?{query}", next.path()),
    )))
}

/// Maps SDK entries through the shared Redfish policy used by SSE collection.
fn entry_to_log(
    entry: &LogEntry,
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
                log_entry_id: Some(entry.id.as_str()),
            })
        })
        .flatten();

    let mut attributes = Vec::with_capacity(14);
    if let Some(machine_id) = machine_id {
        attributes.push((Cow::Borrowed("machine_id"), machine_id.to_string()));
    }
    attributes.push((Cow::Borrowed("entry_id"), entry.id.clone()));
    attributes.push((Cow::Borrowed("service_id"), service_id.to_string()));
    if let Some(oem) = &entry.oem {
        attributes.push((
            Cow::Borrowed("redfish.oem"),
            oem.additional_properties.to_string(),
        ));
    }
    add_redfish_analyzer_attributes(
        &mut attributes,
        log_type,
        redfish_severity.unwrap_or(RedfishSeverity::Unknown),
        nvidia_error_id(entry.oem.as_ref()),
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
    // Some events omit EventTimestamp and expose Created.
    if let Some(timestamp) = entry.event_timestamp.as_ref().or(entry.created.as_ref()) {
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
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, Ordering};

    use axum::extract::State;
    use axum::http::{StatusCode, Uri};
    use axum::response::{IntoResponse, Response};
    use axum::routing::get;
    use axum::{Json, Router};
    use carbide_test_support::{Check, check_values};
    use nv_redfish::bmc_http::{BmcCredentials, CacheSettings, HttpBmc};
    use serde_json::{Value, json};

    use super::*;
    use crate::endpoint::test_support::{mac, test_endpoint};
    use crate::endpoint::{EndpointMetadata, PowerShelfData};
    use crate::sink::LogSeverity;

    const JOURNAL_BMC: &str = "/redfish/v1/Managers/BMC_0/LogServices/Journal";
    const JOURNAL_HGX: &str = "/redfish/v1/Managers/HGX_BMC_0/LogServices/Journal";
    const EVENTLOG: &str = "/redfish/v1/Systems/System_0/LogServices/EventLog";
    const XID: &str = "/redfish/v1/Chassis/HGX_GPU_0/LogServices/XID";
    const SEL: &str = "/redfish/v1/Systems/System_0/LogServices/SEL";

    #[tokio::test]
    async fn downgrade_handoff_replaces_persisted_periodic_cursor()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let state_dir = tempfile::tempdir()?;
        let state_file_path = state_dir.path().join("state.json");
        let service = ODataId::from(EVENTLOG.to_string());
        let stale_service = ODataId::from(SEL.to_string());

        let persisted =
            json!({"last_seen_ids": {service.to_string():100,stale_service.to_string():200}});

        tokio::fs::write(&state_file_path, serde_json::to_vec(&persisted)?).await?;

        for (name, handoff) in [
            ("current SSE cursor", HashMap::from([(service.clone(), 7)])),
            ("empty SSE cursor", HashMap::new()),
        ] {
            let endpoint = Arc::new(test_endpoint(mac("00:11:22:33:44:88")));

            let mut collector = LogsCollector::new_runner(
                Arc::clone(endpoint.bmc()),
                endpoint,
                LogsCollectorConfig {
                    state_file_path: state_file_path.clone(),
                    service_refresh_interval: Duration::from_secs(60),
                    data_sink: None,
                    initial_last_seen_ids: Some(handoff.clone()),
                    include_diagnostics: false,
                    exclude_services: Vec::new(),
                    skip_initial_history: false,
                },
            )?;

            let loaded = collector.load_persistent_state().await?;

            assert_eq!(
                loaded.services.into_keys().collect::<HashSet<_>>(),
                handoff.into_keys().collect::<HashSet<_>>(),
                "{name}"
            );
        }

        Ok(())
    }

    #[derive(Default)]
    struct Source {
        entries: Vec<Value>,
        reads: usize,
        fail: Option<&'static str>,
        ignore_filter: bool,
        empty_first: bool,
        next_link: Option<String>,
        page_size: usize,
        reset_on_read: Option<(String, usize, Vec<Value>)>,
        reset_before_read: bool,
    }

    fn respond_entries(source: &mut Source, uri: &Uri) -> Response {
        let query: HashMap<_, _> =
            url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
                .into_owned()
                .collect();

        let filter = query
            .get("$filter")
            .and_then(|f| f.rsplit(' ').next()?.parse::<i64>().ok());

        let skip = query
            .get("$skip")
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);

        source.reads += 1;

        if (source.fail == Some("page") && skip == 1)
            || (source.fail == Some("filter") && filter.is_some())
        {
            return StatusCode::NOT_IMPLEMENTED.into_response();
        }

        let entries: Vec<_> = source
            .entries
            .iter()
            .filter(|e| {
                source.ignore_filter
                    || filter.is_none_or(|hint| {
                        e["Id"]
                            .as_str()
                            .unwrap()
                            .parse::<i64>()
                            .is_ok_and(|id| id > hint)
                    })
            })
            .collect();

        let page_size = source.page_size.max(1);

        let members: Vec<_> = entries
            .iter()
            .skip(skip)
            .take(page_size)
            .map(|e| json!({"@odata.id":format!("/entries/{}", e["Id"].as_str().unwrap())}))
            .collect();

        let mut page = json!({"@odata.id":"/entries", "Members":members,
            "Members@odata.count": entries.len()});

        match source.fail {
            Some("count_changed") if skip > 0 => {
                page["Members@odata.count"] = json!(entries.len() + 1);
            }
            Some("unfiltered_count") => {
                page["Members@odata.count"] = json!(source.entries.len());
            }
            Some("missing_count") => {
                page.as_object_mut().unwrap().remove("Members@odata.count");
            }
            Some("duplicate") if skip == 1 => {
                page["Members"] = json!([{"@odata.id":"/entries/1"}]);
            }
            _ => {}
        }

        if source.empty_first && uri.query().is_none() {
            page["Members"] = json!([]);
            page["Members@odata.nextLink"] = json!("?%24skip=0");
        } else if source.fail == Some("cycle") {
            page["Members@odata.nextLink"] = json!(format!("?%24skip={}", 1 - skip.min(1)));
        } else if skip + page_size < entries.len() && source.fail != Some("truncated") {
            page["Members@odata.nextLink"] = json!(source.next_link.clone().unwrap_or_else(|| {
                format!(
                    "?%24skip={}{}",
                    skip + page_size,
                    filter
                        .map(|h| format!("&%24filter=Id%20gt%20{h}"))
                        .unwrap_or_default()
                )
            }));
        }

        Json(page).into_response()
    }

    async fn respond(State(source): State<Arc<Mutex<Source>>>, uri: Uri) -> Response {
        let mut source = source.lock().unwrap();

        if source.reset_before_read
            && let Some((reset_path, remaining, _)) = &mut source.reset_on_read
            && reset_path == uri.path()
        {
            *remaining -= 1;

            if *remaining == 0 {
                source.entries = source.reset_on_read.take().unwrap().2;
            }
        }

        let response = match uri.path() {
            "/redfish/v1" => Json(json!({
                "@odata.id": "/redfish/v1", "Id": "Root", "Name": "Root",
                "Links": {"Sessions": {"@odata.id": "/sessions"}},
                "Managers": {
                    "@odata.id": "/managers",
                    "@odata.type": "#ManagerCollection.ManagerCollection", "Name": "Managers",
                    "Members": [{
                        "@odata.id": "/manager", "Id": "BMC", "Name": "BMC",
                        "LogServices": {"@odata.id": "/services"},
                    }],
                },
            }))
            .into_response(),
            "/services" => Json(json!({
                "@odata.id": "/services",
                "@odata.type": "#LogServiceCollection.LogServiceCollection", "Name": "Logs",
                "Members": [{
                    "@odata.id": EVENTLOG, "Id": "EventLog", "Name": "Events",
                    "Entries": {"@odata.id": "/entries"},
                }],
            }))
            .into_response(),
            "/entries" => respond_entries(&mut source, &uri),
            path => {
                if source.fail == Some("member") && path.ends_with("/2") {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }

                if path.ends_with("/9") && source.fail == Some("anchor_auth") {
                    return StatusCode::UNAUTHORIZED.into_response();
                }

                if path.ends_with("/9") && source.fail == Some("anchor_server") {
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }

                let indexed = path
                    .strip_prefix("/entries/")
                    .and_then(|id| id.parse::<usize>().ok())
                    .and_then(|id| source.entries.get(id))
                    .filter(|entry| path == format!("/entries/{}", entry["Id"].as_str().unwrap()));

                let response = indexed
                    .or_else(|| {
                        source.entries.iter().find(|entry| {
                            path == format!("/entries/{}", entry["Id"].as_str().unwrap())
                        })
                    })
                    .cloned();

                response
                    .map(|entry| Json(entry).into_response())
                    .unwrap_or_else(|| StatusCode::NOT_FOUND.into_response())
            }
        };

        if !source.reset_before_read
            && let Some((reset_path, remaining, _)) = &mut source.reset_on_read
            && reset_path == uri.path()
        {
            *remaining -= 1;

            if *remaining == 0 {
                source.entries = source.reset_on_read.take().unwrap().2;
            }
        }

        response
    }

    #[derive(Default)]
    struct Sink(Mutex<Vec<String>>, AtomicBool);

    impl DataSink for Sink {
        fn sink_type(&self) -> &'static str {
            "test"
        }

        fn try_handle_event(
            &self,
            _: &EventContext,
            event: &CollectorEvent,
        ) -> Result<(), HealthError> {
            if self.1.swap(false, Ordering::SeqCst) {
                return Err(HealthError::GenericError(
                    "sink rejected record".to_string(),
                ));
            }

            if let CollectorEvent::Log(record) = event {
                self.0.lock().unwrap().push(record.body.clone());
            }

            Ok(())
        }
    }

    struct Rig {
        collector: LogsCollector<HttpBmc<nv_redfish::bmc_http::reqwest::Client>>,
        server: tokio::task::JoinHandle<()>,
        source: Arc<Mutex<Source>>,
        sink: Arc<Sink>,
        _directory: tempfile::TempDir,
    }

    impl Rig {
        fn new(skip_initial_history: bool) -> Self {
            let source = Arc::new(Mutex::new(Source::default()));

            // Use the production HTTP transport so status classification matches SDK errors.
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let address = listener.local_addr().unwrap();

            listener.set_nonblocking(true).unwrap();
            let listener = tokio::net::TcpListener::from_std(listener).unwrap();

            let router = Router::new()
                .fallback(get(respond))
                .with_state(source.clone());

            let server = tokio::spawn(async move {
                axum::serve(listener, router).await.unwrap();
            });

            let bmc = Arc::new(HttpBmc::new(
                crate::endpoint::test_support::reqwest(),
                Url::parse(&format!("http://{address}")).unwrap(),
                BmcCredentials::new("test".into(), "test".into()),
                CacheSettings::with_capacity(0),
            ));

            let mut endpoint = test_endpoint(mac("00:11:22:33:44:88"));
            endpoint.addr.ip = address.ip();
            endpoint.addr.port = Some(address.port());

            endpoint.metadata = Some(EndpointMetadata::PowerShelf(PowerShelfData {
                id: None,
                serial: None,
                nvlink_domain_uuid: None,
            }));

            let directory = tempfile::tempdir().unwrap();
            let sink = Arc::new(Sink::default());

            let collector = LogsCollector::new_runner(
                bmc,
                Arc::new(endpoint),
                LogsCollectorConfig {
                    state_file_path: directory.path().join("state.json"),
                    service_refresh_interval: Duration::from_secs(1800),
                    data_sink: Some(sink.clone()),
                    initial_last_seen_ids: None,
                    include_diagnostics: false,
                    exclude_services: Vec::new(),
                    skip_initial_history,
                },
            )
            .unwrap();

            Self {
                collector,
                server,
                source,
                sink,
                _directory: directory,
            }
        }

        /// Checks one poll's delivery delta and reloaded checkpoint anchor.
        async fn collect<Id: std::fmt::Display + std::fmt::Debug + Copy>(
            &mut self,
            entries: &[(Id, &str)],
            expected_anchor: Option<i64>,
            emitted: &[&str],
        ) -> IterationResult {
            self.source.lock().unwrap().entries =
                entries.iter().map(|&(id, body)| entry(id, body)).collect();

            let before = self.sink.0.lock().unwrap().len();
            let result = self.collector.run_iteration().await.unwrap();

            assert_eq!(result.fetch_failures, 0, "{entries:?}");

            let saved = self.collector.load_persistent_state().await.unwrap();

            assert_eq!(
                saved.services[&ODataId::from(EVENTLOG.to_string())]
                    .anchor
                    .as_ref()
                    .map(|anchor| anchor.id),
                expected_anchor,
                "{entries:?}"
            );

            assert_eq!(
                &self.sink.0.lock().unwrap()[before..],
                emitted,
                "{entries:?}"
            );

            result
        }

        /// Checks that a service failure leaves the checkpoint byte-for-byte intact.
        async fn fail_scan(&mut self, scenario: &str) {
            let before = std::fs::read(&self.collector.state_file_path).unwrap();
            let result = self.collector.run_iteration().await.unwrap();

            assert_eq!(result.fetch_failures, 1, "{scenario}");

            assert_eq!(
                std::fs::read(&self.collector.state_file_path).unwrap(),
                before,
                "{scenario}"
            );
        }
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            self.server.abort();
        }
    }

    fn entry(id: impl std::fmt::Display, body: &str) -> Value {
        json!({"@odata.id":format!("/entries/{id}"), "Id":id.to_string(), "Name":"Entry", "EntryType":"Event", "Message":body})
    }

    #[tokio::test]
    async fn anchors_recover_resets_and_keep_unordered_append_eligibility() {
        let mut rig = Rig::new(false);
        let started = Instant::now();

        rig.collect(&[(1, "one"), (9, "nine")], Some(9), &["one", "nine"])
            .await;

        assert!(started.elapsed() >= Duration::from_millis(500));

        rig.collect(
            &[(1, "one"), (9, "nine"), (100, "jump"), (10, "unordered")],
            Some(100),
            &["jump", "unordered"],
        )
        .await;

        for (entries, anchor, emitted) in [
            (
                [(1, "reset"), (120, "tail")].as_slice(),
                Some(120),
                ["reset", "tail"].as_slice(),
            ),
            (
                &[(1, "changed"), (120, "reused")],
                Some(120),
                &["changed", "reused"],
            ),
            (&[(2, "lower")], Some(2), &["lower"]),
            (&[], None, &[]),
            (&[(1, "refill")], Some(1), &["refill"]),
        ] {
            rig.collect(entries, anchor, emitted).await;
        }

        // A valid persisted anchor survives restart without replaying old entries.
        rig.collector.state = None;
        rig.collect(&[(1, "refill")], Some(1), &[]).await;
    }

    #[tokio::test]
    async fn moving_pages_preserve_cursor_and_retry_appends() {
        let mut rig = Rig::new(false);
        rig.collect(&[(9, "old")], Some(9), &["old"]).await;

        let replacement = [
            (9, "old"),
            (12, "twelve"),
            (10, "ten"),
            (11, "eleven"),
            (13, "thirteen"),
        ];

        {
            let mut source = rig.source.lock().unwrap();
            source.entries = vec![entry(9, "old"), entry(10, "ten"), entry(11, "eleven")];

            source.reset_on_read = Some((
                "/entries".into(),
                1,
                replacement
                    .iter()
                    .map(|&(id, body)| entry(id, body))
                    .collect(),
            ));
        }

        rig.fail_scan("moving pages").await;

        // Restart from the unchanged checkpoint, then retry the complete collection.
        rig.collector.state = None;

        rig.collect(
            &replacement,
            Some(13),
            &["twelve", "ten", "eleven", "thirteen"],
        )
        .await;
    }

    #[tokio::test]
    async fn appends_during_entry_reads_wait_for_the_next_scan() {
        let mut rig = Rig::new(false);
        rig.collect(&[(9, "old")], Some(9), &["old"]).await;

        let replacement = [
            (12, "twelve"),
            (9, "old"),
            (10, "ten"),
            (11, "eleven"),
            (13, "thirteen"),
        ];

        {
            let mut source = rig.source.lock().unwrap();
            source.ignore_filter = true;
            source.page_size = 2;

            source.reset_on_read = Some((
                "/entries/8".into(),
                1,
                replacement
                    .iter()
                    .map(|&(id, body)| entry(id, body))
                    .collect(),
            ));
        }

        rig.collect(
            &[
                (7, "seven"),
                (8, "eight"),
                (9, "old"),
                (10, "ten"),
                (11, "eleven"),
            ],
            Some(11),
            &["ten", "eleven"],
        )
        .await;

        rig.collect(&replacement, Some(13), &["twelve", "thirteen"])
            .await;
    }

    #[tokio::test]
    async fn incomplete_pages_preserve_progress_until_repaired() {
        for failure in ["count_changed", "missing_count", "duplicate", "truncated"] {
            let mut rig = Rig::new(false);
            rig.collect(&[(0, "old")], Some(0), &["old"]).await;

            {
                let mut source = rig.source.lock().unwrap();
                source.entries = vec![entry(1, "one"), entry(2, "two")];
                source.fail = Some(failure);
            }

            rig.fail_scan(failure).await;

            rig.source.lock().unwrap().fail = None;
            rig.collect(&[(1, "one"), (2, "two")], Some(2), &["one", "two"])
                .await;
        }
    }

    #[tokio::test]
    async fn unpaginated_collection_can_omit_member_count() {
        let mut rig = Rig::new(false);
        rig.source.lock().unwrap().fail = Some("missing_count");

        rig.collect(&[(1, "one")], Some(1), &["one"]).await;
    }

    #[tokio::test]
    async fn filtered_unfiltered_count_retries_without_filtering() {
        let mut rig = Rig::new(false);
        rig.collect(&[(0, "old")], Some(0), &["old"]).await;

        {
            let mut source = rig.source.lock().unwrap();
            source.entries.push(entry(1, "new"));

            source.fail = Some("unfiltered_count");
        }

        rig.fail_scan("unfiltered count on filtered response").await;

        // Leave the server unchanged; the next iteration must use an unfiltered read.
        rig.collect(&[(0, "old"), (1, "new")], Some(1), &["new"])
            .await;
    }

    #[tokio::test]
    async fn failed_scans_preserve_state_and_retry_retained_records() {
        for failure in ["page", "member", "sink"] {
            let mut rig = Rig::new(false);
            rig.collect(&[(0, "old")], Some(0), &["old"]).await;

            {
                let mut source = rig.source.lock().unwrap();
                source.entries = vec![entry(1, "one"), entry(2, "two")];
                source.fail = Some(failure);
            }

            rig.sink.1.store(failure == "sink", Ordering::SeqCst);
            rig.fail_scan(failure).await;

            rig.source.lock().unwrap().fail = None;
            rig.collect(&[(1, "one"), (2, "two")], Some(2), &["one", "two"])
                .await;
        }
    }

    #[tokio::test]
    async fn initial_failure_restarts_with_replay_and_empty_baseline_collects_first_arrival() {
        let mut rig = Rig::new(true);

        {
            let mut source = rig.source.lock().unwrap();
            source.entries = vec![entry(1, "one"), entry(2, "two")];
            source.fail = Some("member");
        }

        assert_eq!(
            rig.collector.run_iteration().await.unwrap().fetch_failures,
            1
        );

        assert!(rig.sink.0.lock().unwrap().is_empty());

        rig.collector.state = None;
        rig.source.lock().unwrap().fail = None;
        rig.collect(&[(1, "one"), (2, "two")], Some(2), &["one", "two"])
            .await;

        for (id, anchor) in [("1", Some(1)), ("opaque", None)] {
            let mut empty = Rig::new(true);
            empty.collect::<i64>(&[], None, &[]).await;
            empty.collect(&[(id, "first")], anchor, &["first"]).await;
        }
    }

    #[tokio::test]
    async fn unsupported_and_ignored_filters_use_local_cursor_comparison() {
        for failure in ["filter", "ignored"] {
            let mut rig = Rig::new(false);
            rig.source.lock().unwrap().empty_first = true;
            rig.collect(&[(9, "old")], Some(9), &["old"]).await;

            {
                let mut source = rig.source.lock().unwrap();
                source.fail = Some(failure);
                source.ignore_filter = failure == "ignored";
            }

            rig.collect(&[(9, "old"), (10, "new")], Some(10), &["new"])
                .await;

            assert!(
                rig.collector.state.as_ref().unwrap().persistent.services
                    [&ODataId::from(EVENTLOG.to_string())]
                    .filter_disabled,
                "{failure}"
            );

            rig.collect(
                &[(9, "old"), (10, "new"), (11, "later")],
                Some(11),
                &["later"],
            )
            .await;
        }
    }

    #[tokio::test]
    async fn invalid_links_and_anchor_errors_preserve_checkpoint() {
        let service = ODataId::from(EVENTLOG.to_string());

        for failure in ["anchor_auth", "anchor_server", "continuation", "saved_uri"] {
            let mut rig = Rig::new(false);
            rig.collect(&[(9, "old")], Some(9), &["old"]).await;

            {
                let mut source = rig.source.lock().unwrap();
                source.fail = Some(failure);
                source.entries.push(entry(10, "new"));

                if failure == "continuation" {
                    source.ignore_filter = true;
                    source.next_link = Some("https://other-bmc/entries".into());
                }
            }

            if failure == "saved_uri" {
                let state = rig.collector.state.as_mut().unwrap();
                state
                    .persistent
                    .services
                    .get_mut(&service)
                    .unwrap()
                    .anchor
                    .as_mut()
                    .unwrap()
                    .uri = ODataId::from("https://other-bmc/entries/9".to_string());

                rig.collector.save_persistent_state().await.unwrap();
            }

            rig.fail_scan(failure).await;

            assert_eq!(*rig.sink.0.lock().unwrap(), ["old"], "{failure}");

            {
                let mut source = rig.source.lock().unwrap();
                source.fail = None;
                source.next_link = None;
            }

            if failure == "saved_uri" {
                let state = rig.collector.state.as_mut().unwrap();
                state
                    .persistent
                    .services
                    .get_mut(&service)
                    .unwrap()
                    .anchor
                    .as_mut()
                    .unwrap()
                    .uri = ODataId::from("/entries/9".to_string());
            }

            rig.collect(&[(9, "old"), (10, "new")], Some(10), &["new"])
                .await;
        }
    }

    #[tokio::test]
    async fn resets_during_collection_preserve_progress_and_retry() {
        for (incremental, (path, nth, before), replacement, anchor) in [
            (
                true,
                ("/entries/12", 1, false),
                [(1, "reset"), (20, "tail")],
                20,
            ),
            (
                false,
                ("/entries/1", 1, false),
                [(1, "reset"), (120, "tail")],
                120,
            ),
            (
                false,
                ("/entries/2", 1, false),
                [(1, "one"), (2, "changed")],
                2,
            ),
            (
                false,
                ("/entries/2", 2, false),
                [(1, "changed"), (2, "two")],
                2,
            ),
            // The reset removes saved ID 9 while candidate ID 12 stays identical.
            (
                true,
                ("/entries/12", 2, true),
                [(1, "reset"), (12, "new")],
                12,
            ),
        ] {
            let mut rig = Rig::new(false);

            if incremental {
                rig.collect(&[(9, "old")], Some(9), &["old"]).await;
                rig.source.lock().unwrap().entries.push(entry(12, "new"));
            } else {
                rig.collect::<i64>(&[], None, &[]).await;
                rig.source.lock().unwrap().entries = vec![entry(1, "one"), entry(2, "two")];
            }

            rig.source.lock().unwrap().reset_on_read = Some((
                path.into(),
                nth,
                replacement
                    .iter()
                    .map(|&(id, body)| entry(id, body))
                    .collect(),
            ));

            rig.source.lock().unwrap().reset_before_read = before;

            rig.fail_scan(&format!("{path}:{nth}")).await;

            rig.collect(
                &replacement,
                Some(anchor),
                &replacement.map(|(_, body)| body),
            )
            .await;
        }
    }

    #[tokio::test]
    async fn mixed_entry_ids_preserve_numeric_progress() {
        for skip_initial_history in [false, true] {
            let mut rig = Rig::new(skip_initial_history);
            rig.source.lock().unwrap().ignore_filter = true;

            rig.collect(
                &[("1", "one"), ("opaque", "opaque")],
                Some(1),
                if skip_initial_history {
                    &[]
                } else {
                    &["one", "opaque"]
                },
            )
            .await;

            rig.collect(
                &[("1", "one"), ("opaque", "opaque"), ("2", "two")],
                Some(2),
                &["two"],
            )
            .await;
        }
    }

    #[tokio::test]
    async fn opaque_baselines_stay_skipped_until_numeric_progress() {
        let mut rig = Rig::new(true);
        rig.source.lock().unwrap().ignore_filter = true;

        rig.collect(&[("opaque", "skipped-history")], None, &[])
            .await;

        rig.collector.state = None;
        rig.collect(&[("opaque", "skipped-history")], None, &[])
            .await;

        rig.collect(
            &[("opaque", "skipped-history"), ("1", "new-numeric")],
            Some(1),
            &["new-numeric"],
        )
        .await;

        // A numeric anchor restores reset recovery for all retained entries.
        rig.collect(
            &[("opaque", "reset-history"), ("2", "reset-numeric")],
            Some(2),
            &["reset-history", "reset-numeric"],
        )
        .await;
    }

    #[tokio::test]
    async fn failed_checkpoint_replace_preserves_live_state() {
        let mut rig = Rig::new(false);
        rig.collect::<i64>(&[], None, &[]).await;

        {
            let mut source = rig.source.lock().unwrap();
            source.entries = vec![entry(1, "one"), entry(2, "two")];
            source.empty_first = true;
        }

        rig.collector.state_file_path = rig._directory.path().to_path_buf();

        assert!(rig.collector.run_iteration().await.is_err());
        assert_eq!(*rig.sink.0.lock().unwrap(), ["one", "two"]);

        let state = rig.collector.state.as_ref().unwrap();

        assert_eq!(
            state.persistent.services[&ODataId::from(EVENTLOG.to_string())]
                .anchor
                .as_ref()
                .unwrap()
                .id,
            2
        );

        rig.collector.state_file_path = rig._directory.path().join("state.json");
        rig.collect(&[(1, "one"), (2, "two")], Some(2), &[]).await;
    }

    #[tokio::test]
    async fn legacy_and_damaged_checkpoints_replay_retained_history() {
        for bytes in [
            serde_json::to_vec(&json!({"last_seen_ids":{EVENTLOG:100}})).unwrap(),
            b"broken".to_vec(),
            serde_json::to_vec(&json!({"version":2,"services":{},"replay_new_services":false})).unwrap(),
            serde_json::to_vec(&json!({"services":{EVENTLOG:{"seen":[],"read_hint":100}},"replay_new_services":false})).unwrap(),
            serde_json::to_vec(&json!({"services":{EVENTLOG:{"anchor":{"id":100,"uri":"/entries/100"}}},"replay_new_services":false})).unwrap(),
        ] {
            let mut rig = Rig::new(true);
            std::fs::write(&rig.collector.state_file_path, bytes).unwrap();

            rig.collect(&[(1, "recovered")], Some(1), &["recovered"]).await;
        }
    }

    #[tokio::test]
    async fn thirty_thousand_entries_persist_only_one_anchor() {
        let mut rig = Rig::new(false);
        let count = 30_000;

        {
            let mut source = rig.source.lock().unwrap();
            source.entries = (0..count).map(|id| entry(id, "retained body")).collect();
            source.page_size = 1000;
        }

        // The fixture bypasses pacing; the small append test exercises production timing.
        rig.collector.read_limiter =
            BucketLimiter::new(count * 2 + 10, Duration::from_millis(200), Duration::ZERO);

        let result = rig.collector.run_iteration().await.unwrap();

        assert_eq!(result.fetch_failures, 0);
        assert_eq!(result.entity_count, Some(count));
        assert_eq!(rig.source.lock().unwrap().reads, 30);
        assert_eq!(rig.sink.0.lock().unwrap().len(), count);

        let checkpoint = std::fs::read(&rig.collector.state_file_path).unwrap();

        assert!(checkpoint.len() < 1024);

        assert!(
            !String::from_utf8(checkpoint)
                .unwrap()
                .contains("retained body")
        );

        let loaded = rig.collector.load_persistent_state().await.unwrap();

        assert_eq!(loaded.services.len(), 1);

        assert_eq!(
            loaded.services[&ODataId::from(EVENTLOG.to_string())]
                .anchor
                .as_ref()
                .unwrap()
                .id,
            29_999
        );

        rig.collector.state = None;
        rig.source
            .lock()
            .unwrap()
            .entries
            .push(entry(count, "append"));

        rig.collector.run_iteration().await.unwrap();

        assert_eq!(rig.sink.0.lock().unwrap().len(), count + 1);
    }

    #[tokio::test]
    async fn cyclic_continuation_preserves_checkpoint_and_retries() {
        let mut rig = Rig::new(false);
        rig.collect(&[(0, "old")], Some(0), &["old"]).await;

        {
            let mut source = rig.source.lock().unwrap();
            source.entries = vec![entry(1, "one"), entry(2, "two")];
            source.fail = Some("cycle");
            source.reads = 0;
        }

        rig.collector.read_limiter =
            BucketLimiter::new(16, Duration::from_millis(200), Duration::ZERO);

        rig.fail_scan("continuation cycle").await;

        assert_eq!(rig.source.lock().unwrap().reads, 3);

        rig.source.lock().unwrap().fail = None;

        let result = rig
            .collect(&[(1, "one"), (2, "two")], Some(2), &["one", "two"])
            .await;

        assert_eq!(result.entity_count, Some(2));
    }

    #[test]
    fn unsupported_filters_preserve_error_classification() {
        for (status, expected) in [
            (StatusCode::BAD_REQUEST, true),
            (StatusCode::NOT_IMPLEMENTED, true),
            (StatusCode::UNAUTHORIZED, false),
            (StatusCode::SERVICE_UNAVAILABLE, false),
        ] {
            let error =
                HealthError::from(nv_redfish::bmc_http::reqwest::BmcError::InvalidResponse {
                    url: Url::parse("https://bmc.test/entries").unwrap(),
                    status,
                    text: "failure".to_string(),
                });

            assert_eq!(unsupported_filter(&error), expected, "{status}");
        }
    }

    #[test]
    fn cancelled_checkpoint_write_removes_temporary_file()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()?;

        let rig = runtime.block_on(async {
            let mut rig = Rig::new(false);
            rig.collector.run_iteration().await?;

            let (release, wait) = std::sync::mpsc::channel::<()>();
            let (ready, started) = tokio::sync::oneshot::channel();

            tokio::task::spawn_blocking(move || {
                let _ = ready.send(());
                let _ = wait.recv();
            });

            started.await?;

            // Queue the filesystem write, then cancel before the worker starts.
            let mut saving = Box::pin(rig.collector.save_persistent_state());

            std::future::poll_fn(|context| {
                assert!(saving.as_mut().poll(context).is_pending());
                std::task::Poll::Ready(())
            })
            .await;

            drop(saving);
            drop(release);

            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(rig)
        })?;

        // Runtime shutdown waits for the detached filesystem work to finish.
        drop(runtime);

        let files: Vec<_> = std::fs::read_dir(rig._directory.path())?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<_, _>>()?;

        assert_eq!(
            files.as_slice(),
            std::slice::from_ref(&rig.collector.state_file_path)
        );

        Ok(())
    }

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

    fn observe_event_timestamp((event_timestamp, created): (Option<&str>, &str)) -> Option<String> {
        let mut value = json!({
            "@odata.id": "/redfish/v1/Chassis/powershelf/LogServices/EventLog/Entries/1",
            "Id": "1",
            "Name": "Power shelf event",
            "EntryType": "Event",
            "Message": "PSU fault",
            "Created": created
        });
        if let Some(event_timestamp) = event_timestamp {
            value["EventTimestamp"] = json!(event_timestamp);
        }
        let entry: nv_redfish::schema::log_entry::LogEntry =
            serde_json::from_value(value).expect("valid Redfish log entry");
        let CollectorEvent::Log(record) = entry_to_log(&entry, None, EVENTLOG, false) else {
            panic!("expected log event");
        };
        attribute(&record, "event_timestamp")
    }

    #[test]
    fn event_timestamp_falls_back_to_created() {
        check_values(
            [
                Check {
                    scenario: "Created stands in for an absent EventTimestamp",
                    input: (None, "2026-05-14T10:00:00Z"),
                    expect: Some("2026-05-14T10:00:00Z".to_string()),
                },
                Check {
                    scenario: "EventTimestamp wins over Created",
                    input: (Some("2026-09-01T12:00:00Z"), "2026-05-14T10:00:00Z"),
                    expect: Some("2026-09-01T12:00:00Z".to_string()),
                },
            ],
            observe_event_timestamp,
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
}
