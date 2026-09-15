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
use std::sync::atomic::{AtomicU64, Ordering};

use carbide_uuid::machine::{HostMachineId, MachineId};
use opentelemetry::metrics::Meter;

pub(super) struct MachineUpdateManagerMetrics {
    pub(super) machines_in_maintenance: Arc<AtomicU64>,
    pub(super) machine_updates_started: Arc<AtomicU64>,
    pub(super) concurrent_machine_updates_available: Arc<AtomicU64>,
}

impl MachineUpdateManagerMetrics {
    pub(super) fn new() -> Self {
        MachineUpdateManagerMetrics {
            machines_in_maintenance: Arc::new(AtomicU64::new(0)),
            machine_updates_started: Arc::new(AtomicU64::new(0)),
            concurrent_machine_updates_available: Arc::new(AtomicU64::new(0)),
        }
    }

    pub(super) fn register_callbacks(&mut self, meter: &Meter) {
        let machines_in_maintenance = self.machines_in_maintenance.clone();
        let machine_updates_started = self.machine_updates_started.clone();
        let concurrent_machine_updates_available =
            self.concurrent_machine_updates_available.clone();
        meter
            .u64_observable_gauge("carbide_machines_in_maintenance_count")
            .with_description("Number of machines in the system in maintenance")
            .with_callback(move |observer| {
                observer.observe(machines_in_maintenance.load(Ordering::Relaxed), &[])
            })
            .build();
        meter
            .u64_observable_gauge("carbide_machine_updates_started_count")
            .with_description("Number of machines in the system in the process of updating")
            .with_callback(move |observer| {
                observer.observe(machine_updates_started.load(Ordering::Relaxed), &[])
            })
            .build();
        meter
            .u64_observable_gauge("carbide_concurrent_machine_updates_available")
            .with_description("Number of machines in the system that can be updated concurrently.")
            .with_callback(move |observer| {
                observer.observe(
                    concurrent_machine_updates_available.load(Ordering::Relaxed),
                    &[],
                )
            })
            .build();
    }
}

/// The device class a firmware update targets: the `target` label shared by
/// [`FirmwareUpdateProgress`] and [`FirmwareUpdateFailed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
pub(crate) enum FirmwareUpdateTarget {
    /// Host firmware, applied through host reprovisioning.
    Host,
    /// DPU NIC firmware, applied through DPU reprovisioning.
    DpuNic,
    /// SuperNIC firmware, applied through the DPA scout flow.
    SuperNic,
}

/// The phase a firmware update just reached, as the `phase` label on
/// [`FirmwareUpdateProgress`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
pub(crate) enum FirmwareUpdatePhase {
    Started,
    Completed,
}

/// A firmware update reached a phase. For the `host` target both phases
/// fire, so the started-to-completed gap is the in-flight backlog: a started
/// series that moves while completed stays flat points at updates that never
/// finish. The `dpu_nic` and `super_nic` targets emit `started` only --
/// completion shows in their own state flows -- and a SuperNIC re-counts one
/// `started` per scout pass while a device stays mismatched, since each pass
/// issues a real apply command. Both phases are at-least-once on transient
/// database errors: the update manager's batch transaction can roll back
/// after an emit, and the next pass re-counts -- the same repeat-on-retry
/// the log lines always had.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "firmware_update_progress",
    metric_name = "carbide_firmware_updates_total",
    component = "nico-api",
    log = info,
    metric = counter,
    message = "Firmware update progress",
    describe = "Number of firmware updates started and completed, by update target and phase; only the host target emits both phases"
)]
pub(crate) struct FirmwareUpdateProgress {
    #[label]
    pub(crate) target: FirmwareUpdateTarget,
    #[label]
    pub(crate) phase: FirmwareUpdatePhase,
    /// The host being reprovisioned (Host), the host whose DPUs update
    /// (DpuNic), or the machine holding the device (SuperNic).
    #[context]
    pub(crate) machine_id: HostMachineId,
    /// What the site knows beyond the machine id: the DPU list with firmware
    /// versions for DpuNic, the device identity and observed/expected
    /// versions for SuperNic. For Host it is empty on automatic starts and
    /// names the initiator on operator-initiated ones.
    #[context]
    pub(crate) detail: String,
}

/// Why a firmware update failed, as the `cause` label on
/// [`FirmwareUpdateFailed`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, carbide_instrument::LabelValue)]
pub(crate) enum FirmwareUpdateFailureCause {
    /// Triggering reprovisioning found no ready machine row to update.
    NoUpdateMatch,
    /// The update ran but the device still reports an unexpected version.
    WrongVersionAfterUpdate,
}

/// A firmware update attempt failed. The per-target, per-cause rate is the
/// alert; the log line names the machine and version involved.
#[derive(carbide_instrument::Event)]
#[event(
    event_name = "firmware_update_failed",
    metric_name = "carbide_firmware_update_failures_total",
    component = "nico-api",
    log = warn,
    metric = counter,
    message = "Firmware update failed",
    describe = "Number of firmware update failures, by update target and cause"
)]
pub(crate) struct FirmwareUpdateFailed {
    #[label]
    pub(crate) target: FirmwareUpdateTarget,
    #[label]
    pub(crate) cause: FirmwareUpdateFailureCause,
    /// The host whose update could not start (NoUpdateMatch) or the DPU
    /// reporting the wrong version (WrongVersionAfterUpdate).
    #[context]
    pub(crate) machine_id: MachineId,
    /// The DPU the reprovisioning trigger could not match (NoUpdateMatch);
    /// empty otherwise.
    #[context]
    pub(crate) unmatched_dpu_machine_id: String,
    /// The firmware version observed after the attempted update
    /// (WrongVersionAfterUpdate); empty otherwise.
    #[context]
    pub(crate) firmware_version: String,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use carbide_instrument::testing::{CapturedLog, MetricsCapture, capture_logs};
    use carbide_uuid::machine::StableHostMachineId;

    use super::*;

    fn field<'a>(log: &'a CapturedLog, name: &str) -> Option<&'a str> {
        log.fields
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn machine_id() -> StableHostMachineId {
        StableHostMachineId::from_str("fm100hseddco33hvlofuqvg543p6p9aj60g76q5cq491g9m9tgtf2dk0530")
            .expect("valid machine id")
    }

    /// One emit writes the INFO progress line (machine id plus the site's
    /// detail) and moves `carbide_firmware_updates_total` under the site's
    /// target and phase.
    #[test]
    fn firmware_update_progress_logs_info_and_counts() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            carbide_instrument::emit(FirmwareUpdateProgress {
                target: FirmwareUpdateTarget::SuperNic,
                phase: FirmwareUpdatePhase::Started,
                machine_id: machine_id().into(),
                detail: "pci_name=0000:cc:00.0 observed_fw_version=Some(\"28.39.1000\") \
                         expected_fw_version=28.39.1002"
                    .to_string(),
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].level, tracing::Level::INFO);
        assert_eq!(logs[0].message, "Firmware update progress");
        assert_eq!(field(&logs[0], "target"), Some("super_nic"));
        assert_eq!(field(&logs[0], "phase"), Some("started"));
        let id = machine_id().to_string();
        assert_eq!(field(&logs[0], "machine_id"), Some(id.as_str()));
        assert!(
            field(&logs[0], "detail")
                .expect("detail field")
                .contains("expected_fw_version=28.39.1002")
        );

        assert_eq!(
            metrics.counter_delta(
                "carbide_firmware_updates_total",
                &[("target", "super_nic"), ("phase", "started")],
            ),
            1.0
        );
    }

    /// The phase label separates the started and completed series for the
    /// same target: one update crossing both phases moves each counter once.
    #[test]
    fn firmware_update_progress_counts_each_phase_separately() {
        let metrics = MetricsCapture::start();
        capture_logs(|| {
            for phase in [FirmwareUpdatePhase::Started, FirmwareUpdatePhase::Completed] {
                carbide_instrument::emit(FirmwareUpdateProgress {
                    target: FirmwareUpdateTarget::Host,
                    phase,
                    machine_id: machine_id().into(),
                    detail: String::new(),
                });
            }
        });

        for phase in ["started", "completed"] {
            assert_eq!(
                metrics.counter_delta(
                    "carbide_firmware_updates_total",
                    &[("target", "host"), ("phase", phase)],
                ),
                1.0,
                "phase {phase}"
            );
        }
    }

    /// A failure emit writes the WARN line with its cause's context and moves
    /// `carbide_firmware_update_failures_total`; the two causes count as
    /// independent series.
    #[test]
    fn firmware_update_failed_logs_warn_and_counts_by_cause() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            carbide_instrument::emit(FirmwareUpdateFailed {
                target: FirmwareUpdateTarget::DpuNic,
                cause: FirmwareUpdateFailureCause::NoUpdateMatch,
                machine_id: machine_id().into(),
                unmatched_dpu_machine_id:
                    "fm100ptrh18t1lrjg2pqagkh3sfigr9m65dejvkq168ako07sc0uibpp5q0".to_string(),
                firmware_version: String::new(),
            });
            carbide_instrument::emit(FirmwareUpdateFailed {
                target: FirmwareUpdateTarget::DpuNic,
                cause: FirmwareUpdateFailureCause::WrongVersionAfterUpdate,
                machine_id: machine_id().into(),
                unmatched_dpu_machine_id: String::new(),
                firmware_version: "11.10.1000".to_string(),
            });
        });

        assert_eq!(logs.len(), 2);
        assert!(logs.iter().all(|log| log.level == tracing::Level::WARN));
        assert!(
            logs.iter()
                .all(|log| log.message == "Firmware update failed")
        );
        assert_eq!(field(&logs[0], "cause"), Some("no_update_match"));
        assert_eq!(
            field(&logs[0], "unmatched_dpu_machine_id"),
            Some("fm100ptrh18t1lrjg2pqagkh3sfigr9m65dejvkq168ako07sc0uibpp5q0")
        );
        assert_eq!(field(&logs[1], "cause"), Some("wrong_version_after_update"));
        assert_eq!(field(&logs[1], "firmware_version"), Some("11.10.1000"));

        for cause in ["no_update_match", "wrong_version_after_update"] {
            assert_eq!(
                metrics.counter_delta(
                    "carbide_firmware_update_failures_total",
                    &[("target", "dpu_nic"), ("cause", cause)],
                ),
                1.0,
                "cause {cause}"
            );
        }
    }
}
