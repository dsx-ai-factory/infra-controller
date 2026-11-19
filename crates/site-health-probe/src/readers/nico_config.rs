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

//! The nico-api config reader: every interval, the machine update manager's
//! caps and pass interval (`Version` with its config), and the firmware
//! target of every configured component (`GetDesiredFirmwareVersions`) with
//! its explicit start flag (`ListHostFirmware`). A failed read keeps the
//! last config, which `/v1/config` serves with its age and the error.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use carbide_site_health_probe_api::{Component, Family, FirmwareTarget, RuntimeSettings};
use rpc::forge;

use crate::config::ReaderConfig;
use crate::framework::{ObservationRecorder, Probe};
use crate::gate::CallGate;
use crate::nico::NicoConnector;
use crate::readers::{Attempt, Failure, Incomplete};
use crate::snapshot::{FailureKind, Reader, SweepMeta, unix_millis};

/// The families this reader publishes.
pub(crate) const NICO_CONFIG_FAMILIES: [Family; 1] = [Family::ConfigNicoApi];

/// One config read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NicoConfigData {
    pub revision: u64,
    pub interval: Duration,
    pub firmware_targets: Vec<FirmwareTarget>,
    pub runtime: RuntimeSettings,
}

/// Reads nico-api's config every interval.
pub(crate) struct NicoConfigReader {
    cfg: ReaderConfig,
    nico: Arc<dyn NicoConnector>,
    gate: Arc<CallGate>,
    sink: Arc<crate::state_metrics::CountingSink>,
    process: u64,
    seq: AtomicU64,
    attempt: Attempt,
}

impl NicoConfigReader {
    pub(crate) fn new(
        cfg: ReaderConfig,
        nico: Arc<dyn NicoConnector>,
        gate: Arc<CallGate>,
        sink: Arc<crate::state_metrics::CountingSink>,
        process_start: SystemTime,
    ) -> Self {
        Self {
            cfg,
            nico,
            gate,
            sink,
            process: unix_millis(process_start),
            seq: AtomicU64::new(0),
            attempt: Attempt::new(Reader::NicoConfig, &NICO_CONFIG_FAMILIES),
        }
    }

    async fn read(&self, rec: &ObservationRecorder) -> Result<NicoConfigData, Failure> {
        let nico = self
            .nico
            .connect()
            .await
            .map_err(|e| Failure::call("connect", &e))?;
        let config = self
            .gate
            .call_with_retries(rec, "version", || nico.runtime_config())
            .await
            .map_err(|e| Failure::call("version", &e))?
            .ok_or_else(|| Failure {
                kind: FailureKind::Decode,
                error: "version: the reply has no runtime config".to_string(),
            })?;
        let desired = self
            .gate
            .call_with_retries(rec, "get_desired_firmware_versions", || {
                nico.desired_firmware_versions()
            })
            .await
            .map_err(|e| Failure::call("get_desired_firmware_versions", &e))?;
        let available = self
            .gate
            .call_with_retries(rec, "list_host_firmware", || nico.host_firmware())
            .await
            .map_err(|e| Failure::call("list_host_firmware", &e))?;
        Ok(NicoConfigData {
            revision: 0,
            interval: self.cfg.interval,
            firmware_targets: firmware_targets(desired, &available)?,
            runtime: runtime_settings(&config),
        })
    }
}

/// The update manager's settings. Without an update policy in the reply, as
/// from a nico-api that predates it, every policy field is unknown.
fn runtime_settings(config: &forge::RuntimeConfig) -> RuntimeSettings {
    let policy = config.machine_update_policy.as_ref();
    RuntimeSettings {
        max_concurrent_machine_updates_absolute: policy
            .and_then(|p| p.max_concurrent_updates_absolute)
            .map(i64::from),
        max_concurrent_machine_updates_percent: policy
            .and_then(|p| p.max_concurrent_updates_percent)
            .map(i64::from),
        machine_update_run_interval_seconds: policy.map(|p| p.effective_run_interval_seconds),
        requires_manual_upgrade: policy.map(|p| p.requires_manual_upgrade),
        auto_host_firmware_update: Some(config.auto_host_firmware_update),
    }
}

/// Refuse malformed or contradictory catalogs without exposing upstream data.
fn invalid(error: &'static str) -> Failure {
    Failure {
        kind: FailureKind::Decode,
        error: error.to_string(),
    }
}

fn text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4096
}

/// Core's display names and serde names, with drive suffixes preserved.
pub(crate) fn component_name(name: &str) -> Option<&str> {
    Some(match name {
        "BMC" | "bmc" => "bmc",
        "UEFI" | "uefi" => "uefi",
        "BMC+UEFI" | "combinedbmcuefi" => "combinedbmcuefi",
        "NIC" | "nic" => "nic",
        "CPLD MB" | "cpldmb" => "cpldmb",
        "CPLD PDB" | "cpldpdb" => "cpldpdb",
        "CEC" | "cec" => "cec",
        "GPU" | "gpu" => "gpu",
        "HGX BMC" | "hgxbmc" => "hgxbmc",
        "CX7" | "cx7" => "cx7",
        "CX8" | "cx8" => "cx8",
        drive
            if drive
                .strip_prefix("drive-")
                .is_some_and(|model| text(model) && model == model.trim()) =>
        {
            drive
        }
        _ => return None,
    })
}

fn component(name: &str) -> Component {
    match name {
        "bmc" => Component::Bmc,
        "uefi" => Component::Uefi,
        "combinedbmcuefi" => Component::BmcUefi,
        _ => Component::Other,
    }
}

pub(super) fn firmware_targets(
    desired: Vec<forge::DesiredFirmwareVersionEntry>,
    available: &[forge::AvailableHostFirmware],
) -> Result<Vec<FirmwareTarget>, Failure> {
    if desired.len() > 4096 || available.len() > 4096 {
        return Err(invalid("firmware catalog exceeds its row bound"));
    }
    let mut models = BTreeSet::new();
    let mut wanted = BTreeMap::new();
    for row in desired {
        let vendor = row.vendor.to_ascii_lowercase();
        if !text(&vendor)
            || !text(&row.model)
            || !models.insert((vendor.clone(), row.model.clone()))
            || row.component_versions.is_empty()
        {
            return Err(invalid(
                "firmware catalog has an invalid or duplicate model",
            ));
        }
        for (name, version) in row.component_versions {
            let name = component_name(&name)
                .ok_or_else(|| invalid("firmware catalog has an unknown component"))?
                .to_string();
            if !text(&version)
                || wanted
                    .insert((vendor.clone(), row.model.clone(), name), version)
                    .is_some()
                || wanted.len() > 4096
            {
                return Err(invalid(
                    "firmware catalog has an invalid or duplicate target",
                ));
            }
        }
    }
    // Alias collisions are ambiguous for that model, not unrelated models.
    let mut folded = BTreeSet::new();
    let mut ambiguous = BTreeSet::new();
    for (vendor, model) in models {
        let key = (
            carbide_site_health_probe_api::metrics::normalize_vendor(&vendor),
            model,
        );
        if !folded.insert(key.clone()) {
            ambiguous.insert(key);
        }
    }
    let mut defaults = BTreeMap::new();
    let mut explicit = BTreeMap::new();
    for row in available {
        let name = component_name(&row.r#type)
            .ok_or_else(|| invalid("firmware defaults have an unknown component"))?;
        let vendor = row.vendor.to_ascii_lowercase();
        if !text(&vendor)
            || !text(&row.model)
            || !text(&row.version)
            || !text(&row.inventory_name_regex)
        {
            return Err(invalid("firmware default has a blank or oversized field"));
        }
        let model = (vendor.clone(), row.model.clone());
        if explicit
            .insert(model, row.needs_explicit_start)
            .is_some_and(|old| old != row.needs_explicit_start)
            || defaults
                .insert((vendor, row.model.clone(), name.to_string()), row)
                .is_some()
        {
            return Err(invalid("firmware defaults conflict or repeat"));
        }
    }
    if defaults.len() != wanted.len() {
        return Err(invalid(
            "firmware catalogs do not describe the same targets",
        ));
    }
    let mut targets = wanted
        .into_iter()
        .map(|((vendor, model, name), version)| {
            let row = defaults
                .get(&(vendor.clone(), model.clone(), name.clone()))
                .ok_or_else(|| invalid("firmware default is missing"))?;
            if row.version != version {
                return Err(invalid("firmware target versions disagree"));
            }
            Ok(FirmwareTarget {
                vendor: carbide_site_health_probe_api::metrics::normalize_vendor(&vendor),
                model,
                component: component(&name),
                component_name: Some(name),
                inventory_name_regex: Some(row.inventory_name_regex.clone()),
                version,
                explicit_start: row.needs_explicit_start,
            })
        })
        .filter(|target| match target {
            Ok(t) => !ambiguous.contains(&(t.vendor.clone(), t.model.clone())),
            Err(_) => true,
        })
        .collect::<Result<Vec<_>, _>>()?;
    targets.sort_by(|a, b| {
        (&a.vendor, &a.model, &a.component_name).cmp(&(&b.vendor, &b.model, &b.component_name))
    });
    Ok(targets)
}

impl NicoConfigData {
    fn next_revision(&mut self, previous: Option<&Self>) -> Result<(), Failure> {
        self.revision = match previous {
            Some(old)
                if old.runtime == self.runtime && old.firmware_targets == self.firmware_targets =>
            {
                old.revision
            }
            old => old
                .map_or(Some(1), |old| old.revision.checked_add(1))
                .filter(|n| (1..carbide_site_health_probe_api::MAX_COUNT).contains(n))
                .ok_or_else(|| invalid("firmware config revision exhausted"))?,
        };
        Ok(())
    }

    pub(crate) fn target_count(&self, vendor: &str, model: &str) -> usize {
        let key = (vendor, model);
        self.firmware_targets
            .partition_point(|t| (t.vendor.as_str(), t.model.as_str()) <= key)
            - self
                .firmware_targets
                .partition_point(|t| (t.vendor.as_str(), t.model.as_str()) < key)
    }

    pub(crate) fn target(&self, vendor: &str, model: &str, name: &str) -> Option<&FirmwareTarget> {
        self.firmware_targets
            .binary_search_by(|t| {
                (
                    t.vendor.as_str(),
                    t.model.as_str(),
                    t.component_name.as_deref().unwrap_or(""),
                )
                    .cmp(&(vendor, model, name))
            })
            .ok()
            .map(|index| &self.firmware_targets[index])
    }
}

pub(crate) fn data(snap: &dyn crate::snapshot::AnySnapshot) -> Option<&NicoConfigData> {
    snap.as_any()
        .downcast_ref::<crate::snapshot::FamilySnapshot<NicoConfigData>>()
        .map(|s| &s.data)
}

/// Bound the age of a completed source read; the reader timeout bounds duration.
pub(crate) fn fresh(meta: &SweepMeta, interval: Duration) -> bool {
    let now = crate::clock::now();
    meta.start <= meta.end
        && meta.end <= now
        && now
            .duration_since(meta.end)
            .is_ok_and(|age| age <= interval.saturating_mul(2))
}

/// Config and its latest attempt are observed atomically. Each reader keeps its own clock.
pub(crate) fn current(
    store: &crate::state::store::SnapshotStore,
) -> Option<Arc<dyn crate::snapshot::AnySnapshot>> {
    let (snap, attempt) = store.current_attempt(Family::ConfigNicoApi);
    let snap = snap?;
    let meta = snap.meta();
    let config = data(snap.as_ref())?;
    let attempt = attempt?;
    if meta.reader != Reader::NicoConfig
        || attempt.failure.is_some()
        || attempt.reader != Reader::NicoConfig
        || !fresh(meta, config.interval)
        || !(1..carbide_site_health_probe_api::MAX_COUNT).contains(&config.revision)
    {
        return None;
    }
    Some(snap)
}

#[async_trait::async_trait]
impl Probe for NicoConfigReader {
    fn name(&self) -> &'static str {
        Reader::NicoConfig.as_str()
    }

    fn api(&self) -> &'static str {
        self.gate.api()
    }

    fn interval(&self) -> Duration {
        self.cfg.interval
    }

    fn timeout(&self) -> Duration {
        self.cfg.timeout
    }

    fn timed_out(&self) {
        self.attempt.timed_out(self.sink.as_ref());
    }

    async fn run(&self, recorder: &ObservationRecorder) -> eyre::Result<()> {
        let start = crate::clock::now();
        let seq = self.seq.fetch_add(1, Ordering::SeqCst) + 1;
        self.attempt.begin();
        let mut built = self.read(recorder).await;
        if let Ok(config) = &mut built {
            let previous = self.sink.store.current(Family::ConfigNicoApi);
            if let Err(error) = config.next_revision(
                previous
                    .as_deref()
                    .filter(|s| s.meta().process == self.process)
                    .and_then(data),
            ) {
                built = Err(error);
            }
        }
        // The read's failure kind, kept before `built` is delivered and moved.
        let failed_kind = built.as_ref().err().map(|f| f.kind);
        let meta = SweepMeta {
            process: self.process,
            seq,
            reader: Reader::NicoConfig,
            start,
            end: crate::clock::now(),
        };
        let mut failed = Incomplete::default();
        if !self
            .attempt
            .deliver(self.sink.as_ref(), &meta, Family::ConfigNicoApi, built)
        {
            failed.record(Family::ConfigNicoApi, failed_kind);
        }
        // `Incomplete` reports a read cut short by a call timeout as a timeout,
        // not a failure, so the scheduler counts a slow nico-api apart from a
        // down one.
        failed.finish()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::nico::fake::{FakeConnector, FakeNico, FakeSite};
    use crate::snapshot::{FamilySnapshot, SnapshotSink};
    pub(crate) fn desired(
        vendor: &str,
        model: &str,
        versions: &[(&str, &str)],
    ) -> forge::DesiredFirmwareVersionEntry {
        forge::DesiredFirmwareVersionEntry {
            vendor: vendor.to_string(),
            model: model.to_string(),
            component_versions: versions
                .iter()
                .map(|(c, v)| (c.to_string(), v.to_string()))
                .collect(),
        }
    }

    pub(crate) fn available(
        vendor: &str,
        model: &str,
        kind: &str,
        version: &str,
        explicit: bool,
    ) -> forge::AvailableHostFirmware {
        forge::AvailableHostFirmware {
            vendor: vendor.to_string(),
            model: model.to_string(),
            r#type: kind.to_string(),
            inventory_name_regex: ".*".to_string(),
            version: version.to_string(),
            needs_explicit_start: explicit,
        }
    }

    /// GB200 hosts update their BMC, UEFI and CX7 only inside a window;
    /// Dell hosts update their BMC at any time.
    fn site() -> FakeSite {
        FakeSite {
            runtime_config: Some(forge::RuntimeConfig {
                auto_host_firmware_update: true,
                machine_update_policy: Some(forge::MachineUpdatePolicy {
                    effective_run_interval_seconds: 300,
                    requires_manual_upgrade: false,
                    max_concurrent_updates_absolute: Some(100),
                    max_concurrent_updates_percent: None,
                }),
                ..Default::default()
            }),
            desired_firmware: vec![
                desired("dell", "PowerEdge R750", &[("bmc", "7.10")]),
                desired(
                    "nvidia",
                    "GB200 NVL",
                    &[("uefi", "1.2"), ("bmc", "25.06-2"), ("cx7", "28.45")],
                ),
            ],
            host_firmware: vec![
                available("nvidia", "GB200 NVL", "BMC", "25.06-2", true),
                available("nvidia", "GB200 NVL", "UEFI", "1.2", true),
                available("nvidia", "GB200 NVL", "CX7", "28.45", true),
                available("dell", "PowerEdge R750", "BMC", "7.10", false),
            ],
            ..FakeSite::default()
        }
    }

    pub(crate) fn content() -> NicoConfigData {
        let site = site();
        NicoConfigData {
            revision: 1,
            interval: Duration::from_secs(60),
            firmware_targets: firmware_targets(site.desired_firmware, &site.host_firmware).unwrap(),
            runtime: runtime_settings(site.runtime_config.as_ref().unwrap()),
        }
    }

    #[test]
    fn config_targets_are_unique_and_complete() {
        type Case = (&'static str, fn(&mut FakeSite));
        let cases: &[Case] = &[
            ("duplicate desired", |s| {
                s.desired_firmware.push(s.desired_firmware[0].clone())
            }),
            ("duplicate default", |s| {
                s.host_firmware.push(s.host_firmware[0].clone())
            }),
            ("missing default", |s| {
                s.host_firmware.pop();
            }),
            ("extra default", |s| {
                let mut x = s.host_firmware[0].clone();
                x.model = "other".into();
                s.host_firmware.push(x);
            }),
            ("version disagreement", |s| {
                s.host_firmware[0].version = "other".into()
            }),
            ("blank regex", |s| {
                s.host_firmware[0].inventory_name_regex = " ".into()
            }),
            ("explicit disagreement", |s| {
                s.host_firmware[0].needs_explicit_start = false
            }),
            ("unknown component", |s| {
                s.host_firmware[0].r#type = "future".into()
            }),
            ("blank model", |s| s.desired_firmware[0].model.clear()),
        ];
        assert_eq!(content().firmware_targets.len(), 4);
        for (name, edit) in cases {
            let mut site = site();
            edit(&mut site);
            assert!(
                firmware_targets(site.desired_firmware, &site.host_firmware).is_err(),
                "{name}"
            );
        }
        assert!(firmware_targets(vec![], &[]).unwrap().is_empty());
        for name in [
            "bmc",
            "uefi",
            "combinedbmcuefi",
            "cx7",
            "cpldmb",
            "drive-exact MODEL",
        ] {
            assert_eq!(component_name(name), Some(name));
        }
    }

    #[test]
    fn config_policy_unknown_is_not_disabled() {
        let mut runtime = forge::RuntimeConfig::default();
        let unknown = runtime_settings(&runtime);
        assert_eq!(unknown.requires_manual_upgrade, None);
        assert_eq!(unknown.max_concurrent_machine_updates_absolute, None);
        runtime.machine_update_policy = Some(forge::MachineUpdatePolicy {
            effective_run_interval_seconds: 300,
            requires_manual_upgrade: true,
            max_concurrent_updates_absolute: Some(0),
            max_concurrent_updates_percent: None,
        });
        let known = runtime_settings(&runtime);
        assert_eq!(known.requires_manual_upgrade, Some(true));
        assert_eq!(known.machine_update_run_interval_seconds, Some(300));
        assert_eq!(known.max_concurrent_machine_updates_absolute, Some(0));
        runtime
            .machine_update_policy
            .as_mut()
            .unwrap()
            .effective_run_interval_seconds = 0;
        assert_eq!(
            runtime_settings(&runtime).machine_update_run_interval_seconds,
            Some(0)
        );
    }

    #[test]
    fn config_revision_tracks_content_only() {
        let old = content();
        let mut new = old.clone();
        new.interval *= 2;
        new.next_revision(Some(&old)).unwrap();
        assert_eq!(new.revision, 1);
        for change in 0..3 {
            let mut new = old.clone();
            match change {
                0 => new.firmware_targets[0].inventory_name_regex = Some("new".into()),
                1 => {
                    new.firmware_targets[0].explicit_start = !new.firmware_targets[0].explicit_start
                }
                _ => new.runtime.requires_manual_upgrade = Some(true),
            }
            new.next_revision(Some(&old)).unwrap();
            assert_eq!(new.revision, 2);
        }
        new.next_revision(None).unwrap();
        assert_eq!(new.revision, 1);
        let mut max = old.clone();
        max.revision = carbide_site_health_probe_api::MAX_COUNT - 1;
        new.runtime.requires_manual_upgrade = Some(true);
        assert!(new.next_revision(Some(&max)).is_err());
    }

    fn harness() -> (
        Arc<FakeNico>,
        Arc<crate::state::store::SnapshotStore>,
        NicoConfigReader,
    ) {
        let nico = FakeNico::new(site());
        let store = Arc::new(crate::state::store::SnapshotStore::new(
            &crate::config::Paging::default(),
        ));
        let metrics = metrics_endpoint::new_metrics_setup("probe", "test", false).unwrap();
        let sink = Arc::new(crate::state_metrics::CountingSink::new(
            store.clone(),
            &metrics.meter,
        ));
        let reader = NicoConfigReader::new(
            ReaderConfig {
                enabled: true,
                interval: Duration::from_secs(60),
                timeout: Duration::from_secs(1),
            },
            Arc::new(FakeConnector(nico.clone())),
            Arc::new(CallGate::new(
                "nico-api",
                1000.0,
                Duration::from_secs(5),
                Duration::from_secs(30),
            )),
            sink,
            crate::clock::now(),
        );
        (nico, store, reader)
    }

    #[tokio::test(start_paused = true)]
    async fn config_attempts_preserve_last_success() {
        use crate::gate::{MAX_RETRIES, classify_grpc};
        let (nico, store, reader) = harness();
        reader.run(&ObservationRecorder::default()).await.unwrap();
        let first = store.current(Family::ConfigNicoApi).unwrap();
        nico.fail_next(
            "version",
            classify_grpc(&tonic::Status::unavailable("private upstream text")),
        );
        reader.run(&ObservationRecorder::default()).await.unwrap();
        assert_eq!(nico.calls("version"), 3);
        assert_eq!(
            data(store.current(Family::ConfigNicoApi).unwrap().as_ref())
                .unwrap()
                .revision,
            1
        );
        for code in [
            tonic::Code::PermissionDenied,
            tonic::Code::InvalidArgument,
            tonic::Code::Unauthenticated,
        ] {
            nico.fail_next(
                "version",
                classify_grpc(&tonic::Status::new(code, "private upstream text")),
            );
            assert!(reader.run(&ObservationRecorder::default()).await.is_err());
            assert!(current(&store).is_none());
            let error = store
                .last_attempt(Family::ConfigNicoApi)
                .unwrap()
                .failure
                .unwrap()
                .1;
            assert!(!error.contains("private"));
        }
        reader.run(&ObservationRecorder::default()).await.unwrap();
        let process = store.current(Family::ConfigNicoApi).unwrap().meta().process;
        let mut config = content();
        config.revision = 9;
        store.publish(
            Family::ConfigNicoApi,
            Arc::new(FamilySnapshot {
                meta: SweepMeta {
                    process: process + 1,
                    ..first.meta().clone()
                },
                data: config,
            }),
        );
        reader.run(&ObservationRecorder::default()).await.unwrap();
        assert_eq!(
            data(store.current(Family::ConfigNicoApi).unwrap().as_ref())
                .unwrap()
                .revision,
            1
        );
        for _ in 0..=MAX_RETRIES {
            nico.fail_next(
                "version",
                classify_grpc(&tonic::Status::unavailable("private")),
            );
        }
        assert!(reader.run(&ObservationRecorder::default()).await.is_err());
        assert!(store.current(Family::ConfigNicoApi).is_some());
        reader.run(&ObservationRecorder::default()).await.unwrap();
        tokio::time::advance(Duration::from_secs(121)).await;
        assert!(current(&store).is_none());
    }
    #[tokio::test(start_paused = true)]
    async fn config_watchdog_preserves_last_success() {
        let (nico, store, reader) = harness();
        reader.run(&ObservationRecorder::default()).await.unwrap();
        let before = store.current(Family::ConfigNicoApi).unwrap();
        nico.set_delay(Duration::from_secs(10));
        let cancel = tokio_util::sync::CancellationToken::new();
        let (mut results, loops) = crate::framework::start(vec![Arc::new(reader)], cancel.clone());
        assert_eq!(
            results.recv().await.unwrap().outcome,
            crate::framework::Outcome::Timeout
        );
        assert_eq!(
            store
                .current(Family::ConfigNicoApi)
                .unwrap()
                .meta()
                .snapshot_id(),
            before.meta().snapshot_id()
        );
        assert_eq!(
            store
                .last_attempt(Family::ConfigNicoApi)
                .unwrap()
                .failure
                .unwrap()
                .0,
            FailureKind::Timeout
        );
        cancel.cancel();
        crate::framework::join_loops(loops).await.unwrap();
    }
    #[tokio::test(start_paused = true)]
    async fn config_freshness_uses_completed_read() {
        let (_, store, reader) = harness();
        reader.run(&ObservationRecorder::default()).await.unwrap();
        let original = store.current(Family::ConfigNicoApi).unwrap();
        tokio::time::advance(Duration::from_secs(600)).await;
        let now = crate::clock::now();
        for (name, start, end, expected) in [
            ("fresh", now, now, true),
            ("300s sweep", now - Duration::from_secs(300), now, true),
            (
                "end boundary",
                now - Duration::from_secs(540),
                now - Duration::from_secs(240),
                true,
            ),
            (
                "stale end",
                now - Duration::from_secs(541),
                now - Duration::from_secs(241),
                false,
            ),
            ("future end", now, now + Duration::from_secs(1), false),
            ("inverted", now, now - Duration::from_secs(1), false),
        ] {
            store.publish(
                Family::ConfigNicoApi,
                Arc::new(FamilySnapshot {
                    meta: SweepMeta {
                        start,
                        end,
                        ..original.meta().clone()
                    },
                    data: NicoConfigData {
                        interval: Duration::from_secs(120),
                        ..content()
                    },
                }),
            );
            assert_eq!(current(&store).is_some(), expected, "{name}");
        }
    }
}
