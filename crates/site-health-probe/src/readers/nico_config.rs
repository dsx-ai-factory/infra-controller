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
use crate::snapshot::{FailureKind, Reader, SnapshotSink, SweepMeta, unix_millis};

/// The families this reader publishes.
pub(crate) const NICO_CONFIG_FAMILIES: [Family; 1] = [Family::ConfigNicoApi];

/// One config read.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NicoConfigData {
    pub firmware_targets: Vec<FirmwareTarget>,
    pub runtime: RuntimeSettings,
}

/// Reads nico-api's config every interval.
pub(crate) struct NicoConfigReader {
    cfg: ReaderConfig,
    nico: Arc<dyn NicoConnector>,
    gate: Arc<CallGate>,
    sink: Arc<dyn SnapshotSink>,
    process: u64,
    seq: AtomicU64,
    attempt: Attempt,
}

impl NicoConfigReader {
    pub(crate) fn new(
        cfg: ReaderConfig,
        nico: Arc<dyn NicoConnector>,
        gate: Arc<CallGate>,
        sink: Arc<dyn SnapshotSink>,
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

/// One target per configured component, with its vendor and model's explicit
/// start flag. Both reads list the default versions of one firmware config,
/// so a target version the second read does not list means the config
/// changed between them: the read is unresolved and the last config stays.
fn firmware_targets(
    desired: Vec<forge::DesiredFirmwareVersionEntry>,
    available: &[forge::AvailableHostFirmware],
) -> Result<Vec<FirmwareTarget>, Failure> {
    let mut targets = Vec::new();
    for entry in desired {
        for (component_name, version) in entry.component_versions {
            let Some(listed) = available.iter().find(|a| {
                a.vendor == entry.vendor
                    && a.model == entry.model
                    && same_component(&component_name, &a.r#type)
                    && a.version == version
            }) else {
                return Err(Failure::unresolved(
                    "firmware targets: the firmware config changed between reads".to_string(),
                ));
            };
            targets.push(FirmwareTarget {
                vendor: entry.vendor.clone(),
                model: entry.model.clone(),
                component: component(&component_name),
                component_name,
                version,
                explicit_start: listed.needs_explicit_start,
            });
        }
    }
    targets.sort_by(|a, b| {
        (&a.vendor, &a.model, &a.component_name).cmp(&(&b.vendor, &b.model, &b.component_name))
    });
    Ok(targets)
}

/// Whether a desired component (a slug like `combinedbmcuefi`) and a listed
/// component (its display name like `BMC+UEFI`) are the same component. A
/// drive model is listed and desired by the same `drive-<model>` text, so it
/// matches exactly without lowercasing its suffix.
fn same_component(desired: &str, display: &str) -> bool {
    let slug = match display {
        "BMC" => "bmc",
        "UEFI" => "uefi",
        "BMC+UEFI" => "combinedbmcuefi",
        "NIC" => "nic",
        "CPLD MB" => "cpldmb",
        "CPLD PDB" => "cpldpdb",
        "CEC" => "cec",
        "GPU" => "gpu",
        "HGX BMC" => "hgxbmc",
        "CX7" => "cx7",
        "Unknown" => "unknown",
        other => other, // drive-<model>, matched exactly
    };
    slug == desired
}

/// The API component of a nico-api component name.
fn component(name: &str) -> Component {
    match name {
        "bmc" => Component::Bmc,
        "uefi" => Component::Uefi,
        "combinedbmcuefi" => Component::BmcUefi,
        _ => Component::Other,
    }
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
        let built = self.read(recorder).await;
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
mod tests {
    use super::*;
    use crate::framework::{Outcome, join_loops, start};
    use crate::gate::{CallError, MAX_RETRIES};
    use crate::nico::fake::{FakeConnector, FakeNico, FakeSite};
    use crate::snapshot::testing::FakeSink;

    fn desired(
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

    fn available(
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

    struct Harness {
        nico: Arc<FakeNico>,
        sink: Arc<FakeSink>,
        reader: NicoConfigReader,
    }

    fn harness(site: FakeSite) -> Harness {
        let nico = FakeNico::new(site);
        let sink = Arc::new(FakeSink::default());
        let reader = NicoConfigReader::new(
            ReaderConfig {
                enabled: true,
                interval: Duration::from_secs(60),
                timeout: Duration::from_secs(120),
            },
            Arc::new(FakeConnector(Arc::clone(&nico))),
            Arc::new(CallGate::new(
                "nico-api",
                1_000.0,
                Duration::from_secs(5),
                Duration::from_secs(30),
            )),
            Arc::clone(&sink) as Arc<dyn SnapshotSink>,
            SystemTime::now(),
        );
        Harness { nico, sink, reader }
    }

    impl Harness {
        async fn sweep(&self) -> eyre::Result<()> {
            self.reader.run(&ObservationRecorder::default()).await
        }

        fn latest(&self) -> Option<NicoConfigData> {
            self.sink
                .latest::<NicoConfigData>(Family::ConfigNicoApi)
                .map(|(_, data)| data)
        }

        fn errors(&self) -> Vec<(FailureKind, String)> {
            self.sink
                .failures
                .lock()
                .unwrap()
                .iter()
                .map(|(_, _, kind, error)| (*kind, error.clone()))
                .collect()
        }
    }

    fn target(model: &str, name: &str, version: &str, explicit_start: bool) -> FirmwareTarget {
        let vendor = if model == "GB200 NVL" {
            "nvidia"
        } else {
            "dell"
        };
        FirmwareTarget {
            vendor: vendor.to_string(),
            model: model.to_string(),
            component: component(name),
            component_name: name.to_string(),
            version: version.to_string(),
            explicit_start,
        }
    }

    type Edit = fn(&mut FakeSite);
    /// A name, a site edit, a hook after a call, and the outcome.
    type Case = (
        &'static str,
        Edit,
        Option<(&'static str, Edit)>,
        Result<NicoConfigData, (FailureKind, &'static str)>,
    );

    /// One sweep of a fresh reader: the config it publishes, or the failure
    /// it records with nothing published.
    #[tokio::test]
    async fn a_sweep_publishes_the_config_or_records_why_not() {
        let gb200 = |name, version| target("GB200 NVL", name, version, true);
        let targets = vec![
            target("PowerEdge R750", "bmc", "7.10", false),
            gb200("bmc", "25.06-2"),
            gb200("cx7", "28.45"),
            gb200("uefi", "1.2"),
        ];
        let runtime = RuntimeSettings {
            max_concurrent_machine_updates_absolute: Some(100),
            max_concurrent_machine_updates_percent: None,
            machine_update_run_interval_seconds: Some(300),
            requires_manual_upgrade: Some(false),
            auto_host_firmware_update: Some(true),
        };
        let data = |firmware_targets: &[FirmwareTarget], runtime: &RuntimeSettings| {
            Ok(NicoConfigData {
                firmware_targets: firmware_targets.to_vec(),
                runtime: runtime.clone(),
            })
        };
        let no_policy = RuntimeSettings {
            max_concurrent_machine_updates_absolute: None,
            machine_update_run_interval_seconds: None,
            requires_manual_upgrade: None,
            auto_host_firmware_update: Some(false),
            ..runtime.clone()
        };
        let cases: [Case; 5] = [
            (
                "every target with its explicit start",
                |_| {},
                None,
                data(&targets, &runtime),
            ),
            (
                "a nico-api without an update policy",
                |site| {
                    site.runtime_config = Some(forge::RuntimeConfig {
                        auto_host_firmware_update: false,
                        max_concurrent_machine_updates: 50,
                        ..Default::default()
                    })
                },
                None,
                data(&targets, &no_policy),
            ),
            (
                "no firmware config",
                |site| {
                    site.desired_firmware.clear();
                    site.host_firmware.clear();
                },
                None,
                data(&[], &runtime),
            ),
            (
                "a reply without runtime config",
                |site| site.runtime_config = None,
                None,
                Err((
                    FailureKind::Decode,
                    "version: the reply has no runtime config",
                )),
            ),
            // No torn mix of the old target and the new flag is published.
            (
                "a target changed between the firmware reads",
                |_| {},
                Some(("get_desired_firmware_versions", |site| {
                    site.host_firmware[0].version = "25.10-1".to_string()
                })),
                Err((
                    FailureKind::Unresolved,
                    "firmware targets: the firmware config changed between reads",
                )),
            ),
        ];
        for (name, edit, hook, want) in cases {
            let mut site = site();
            edit(&mut site);
            let h = harness(site);
            if let Some((call, hook)) = hook {
                h.nico.after(call, hook);
            }
            assert_eq!(h.sweep().await.is_ok(), want.is_ok(), "{name}");
            let want = match want {
                Ok(data) => (Some(data), vec![]),
                Err((kind, error)) => (None, vec![(kind, error.to_string())]),
            };
            assert_eq!((h.latest(), h.errors()), want, "{name}");
        }
    }

    /// A target pairs with its own component by its real spelling, not by a
    /// shared version, so a mid-read change to one of two shared-version
    /// components is unresolved and the previous publication stays.
    #[tokio::test]
    async fn firmware_targets_match_their_component() {
        // The list RPC's display names pair with the desired slugs, and two
        // drive models at a shared version pair by their exact suffix, with
        // their own explicit-start flags.
        let mut s = site();
        s.desired_firmware = vec![desired(
            "nvidia",
            "GB200 NVL",
            &[
                ("combinedbmcuefi", "2"),
                ("cpldmb", "3"),
                ("unknown", "5"),
                ("drive-AaA", "9"),
                ("drive-aaa", "9"),
            ],
        )];
        s.host_firmware = vec![
            available("nvidia", "GB200 NVL", "BMC+UEFI", "2", true),
            available("nvidia", "GB200 NVL", "CPLD MB", "3", false),
            available("nvidia", "GB200 NVL", "Unknown", "5", true),
            available("nvidia", "GB200 NVL", "drive-AaA", "9", true),
            available("nvidia", "GB200 NVL", "drive-aaa", "9", false),
        ];
        let h = harness(s);
        h.sweep().await.expect("sweep");
        assert_eq!(
            h.latest().expect("published").firmware_targets,
            vec![
                target("GB200 NVL", "combinedbmcuefi", "2", true),
                target("GB200 NVL", "cpldmb", "3", false),
                target("GB200 NVL", "drive-AaA", "9", true),
                target("GB200 NVL", "drive-aaa", "9", false),
                target("GB200 NVL", "unknown", "5", true),
            ],
        );

        // BMC and UEFI share a version. Publish once, then change only BMC
        // between the next reads: the sweep is unresolved and the earlier
        // publication is kept, not cleared.
        let mut s = site();
        s.desired_firmware = vec![desired(
            "nvidia",
            "GB200 NVL",
            &[("bmc", "1"), ("uefi", "1")],
        )];
        s.host_firmware = vec![
            available("nvidia", "GB200 NVL", "BMC", "1", true),
            available("nvidia", "GB200 NVL", "UEFI", "1", true),
        ];
        let h = harness(s);
        h.sweep().await.expect("first sweep");
        let published = h.latest().expect("published");
        assert_eq!(
            published.firmware_targets,
            vec![
                target("GB200 NVL", "bmc", "1", true),
                target("GB200 NVL", "uefi", "1", true),
            ],
        );
        h.nico.after("get_desired_firmware_versions", |site| {
            site.host_firmware.retain(|a| a.r#type != "BMC");
            site.host_firmware
                .push(available("nvidia", "GB200 NVL", "BMC", "2", true));
        });
        assert!(h.sweep().await.is_err(), "want unresolved");
        assert_eq!(
            h.errors(),
            vec![(
                FailureKind::Unresolved,
                "firmware targets: the firmware config changed between reads".to_string(),
            )],
        );
        assert_eq!(h.latest(), Some(published), "the prior publication is kept");
    }

    #[test]
    fn component_of_a_nico_api_name() {
        for (name, want) in [
            ("bmc", Component::Bmc),
            ("uefi", Component::Uefi),
            ("combinedbmcuefi", Component::BmcUefi),
            ("cx7", Component::Other),
            ("drive-KCD8DPUG15T3", Component::Other),
        ] {
            assert_eq!(component(name), want, "{name}");
        }
    }

    /// Each failed call keeps the last config and records a bounded error,
    /// never the upstream text.
    #[tokio::test]
    async fn a_failed_read_keeps_the_last_config() {
        let h = harness(site());
        h.sweep().await.expect("first sweep");
        let first = h.latest().expect("published");
        for (call, err, kind) in [
            (
                "version",
                CallError::Status("PermissionDenied".to_string()),
                FailureKind::Rpc,
            ),
            (
                "get_desired_firmware_versions",
                CallError::Decode("upstream secret text".to_string()),
                FailureKind::Decode,
            ),
            (
                "list_host_firmware",
                CallError::Status("Internal".to_string()),
                FailureKind::Rpc,
            ),
        ] {
            h.nico.fail_next(call, err);
            assert!(h.sweep().await.is_err(), "{call}");
            let (got, error) = h.errors().pop().expect("failure");
            assert_eq!(got, kind, "{call}");
            assert!(error.starts_with(call), "{call}: {error}");
            assert!(!error.contains("upstream"), "{call}: {error}");
        }
        assert_eq!(h.sink.count(Family::ConfigNicoApi), 1);
        assert_eq!(h.latest(), Some(first));
        // Retryable errors are retried within the sweep.
        h.nico.fail_next("version", CallError::Timeout);
        h.sweep().await.expect("retried");
        assert_eq!(h.sink.count(Family::ConfigNicoApi), 2);
    }

    /// A read whose every retry times out before the sweep deadline keeps the
    /// last config and its read time, records a timeout family, and the
    /// scheduler counts the run as a timeout, not a failure: a slow nico-api
    /// is a different incident from a down one.
    #[tokio::test(start_paused = true)]
    async fn exhausted_call_timeouts_are_a_timeout_outcome() {
        let nico = FakeNico::new(site());
        let sink = Arc::new(FakeSink::default());
        let reader: Arc<dyn Probe> = Arc::new(NicoConfigReader::new(
            ReaderConfig {
                enabled: true,
                interval: Duration::from_secs(60),
                timeout: Duration::from_secs(120),
            },
            Arc::new(FakeConnector(Arc::clone(&nico))),
            Arc::new(CallGate::new(
                "nico-api",
                1_000.0,
                Duration::from_secs(5),
                Duration::from_secs(30),
            )),
            Arc::clone(&sink) as Arc<dyn SnapshotSink>,
            SystemTime::now(),
        ));
        let cancel = tokio_util::sync::CancellationToken::new();
        let (mut results, loops) = start(vec![reader], cancel.clone());

        // The first sweep publishes a good config.
        let first = results.recv().await.expect("first result");
        assert_eq!(first.outcome, Outcome::Success);
        let published = sink
            .latest::<NicoConfigData>(Family::ConfigNicoApi)
            .expect("published");

        // Every attempt of the next version read times out, exhausting its
        // retries before the sweep deadline.
        for _ in 0..1 + MAX_RETRIES {
            nico.fail_next("version", CallError::Timeout);
        }
        tokio::time::sleep(Duration::from_secs(70)).await;
        let second = results.recv().await.expect("second result");
        assert_eq!(second.outcome, Outcome::Timeout);

        assert_eq!(
            sink.latest::<NicoConfigData>(Family::ConfigNicoApi),
            Some(published),
            "the old config and its read time are kept",
        );
        assert_eq!(
            sink.failure_kinds(Family::ConfigNicoApi),
            vec![FailureKind::Timeout],
        );
        assert_eq!(
            sink.count(Family::ConfigNicoApi),
            1,
            "no second publication"
        );

        cancel.cancel();
        join_loops(loops).await.expect("loops end");
    }
}
