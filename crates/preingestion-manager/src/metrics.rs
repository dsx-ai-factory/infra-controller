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

use std::fmt;
use std::net::IpAddr;
use std::time::Duration;

use ::carbide_utils::metrics::SharedMetricsHolder;
use carbide_instrument::{
    DynamicLog, DynamicMessage, Event, LabelValue, LogAt, MetricFamily, Outcome, emit,
};
use libredfish::model::task::TaskState;
use libredfish::{RedfishError, SystemPowerControl};
use model::firmware::FirmwareComponentType;
use opentelemetry::StringValue;
use opentelemetry::metrics::Meter;

#[derive(Clone, Debug)]
pub(super) struct PreingestionMetrics {
    pub(super) machines_in_preingestion: usize,
    pub(super) waiting_for_installation: usize,
    pub(super) delayed_uploading: u64,
}

impl PreingestionMetrics {
    pub(super) fn new() -> Self {
        Self {
            machines_in_preingestion: 0,
            waiting_for_installation: 0,
            delayed_uploading: 0,
        }
    }
}
fn hydrate_meter(meter: Meter, shared_metrics: SharedMetricsHolder<PreingestionMetrics>) {
    {
        let metrics = shared_metrics.clone();
        meter
            .u64_observable_gauge("carbide_preingestion_total")
            .with_description(
                "Number of known machines currently being evaluated prior to ingestion",
            )
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    observer.observe(metrics.machines_in_preingestion as u64, attrs);
                });
            })
            .build();
    }

    {
        let metrics = shared_metrics.clone();
        meter
                .u64_observable_gauge("carbide_preingestion_waiting_installation")
                .with_description(
                    "Number of machines which have had firmware uploaded to them and are currently in the process of installing that firmware"
                ).with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    observer.observe(metrics.waiting_for_installation as u64, attrs)
                });
            }).build();
    }

    {
        let metrics = shared_metrics;
        meter
            .u64_observable_gauge("carbide_preingestion_waiting_download")
            .with_description("Number of machines that are waiting for firmware downloads on other machines to complete before doing their own")
            .with_callback(move |observer| {
                metrics.if_available(|metrics, attrs| {
                    observer.observe(
                        metrics.delayed_uploading,
                        attrs,
                    );
                });
            })
            .build();
    }
}

pub(super) struct MetricHolder {
    last_iteration_metrics: SharedMetricsHolder<PreingestionMetrics>,
}

impl MetricHolder {
    pub(super) fn new(meter: Meter, hold_period: std::time::Duration) -> Self {
        let last_iteration_metrics = SharedMetricsHolder::with_hold_period(hold_period);
        hydrate_meter(meter, last_iteration_metrics.clone());
        Self {
            last_iteration_metrics,
        }
    }

    /// Updates the most recent metrics
    pub(super) fn update_metrics(&self, metrics: PreingestionMetrics) {
        self.last_iteration_metrics.update(metrics);
    }
}

// ---------------------------------------------------------------------------
// Occurrence events (the instrumentation framework). These land on the global
// meter -- carbide-api's meter provider exposes them on /metrics -- and are
// separate from the point-in-time gauges above, which stay on the
// `SharedMetricsHolder` pattern and the `Meter` passed into `MetricHolder`.
// ---------------------------------------------------------------------------

/// How a BFB copy ended, as a bounded metric label. `Ok` and `Error` are the
/// spawned copy task's own result; `Timeout` is the state machine giving up
/// on a copy whose task died without ever reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum BfbCopyOutcome {
    Ok,
    Error,
    Timeout,
}

/// One BFB copy to a DPU. Every attempt records its duration.
#[derive(Event)]
#[event(
    event_name = "bfb_copy_finished",
    metric_name = "carbide_preingestion_bfb_copy_duration_seconds",
    component = "preingestion-manager",
    metric = histogram,
    describe = "Duration of preingestion BFB copies to a DPU rshim, by outcome; the _count \
                series, split by outcome, is the copy and failure rate.",
    labels(outcome: BfbCopyOutcome),
)]
pub(crate) enum BfbCopyFinished {
    #[event(
        labels(outcome = BfbCopyOutcome::Ok),
        log = info,
        message = "BFB copy finished"
    )]
    Ok {
        #[observation]
        took: Duration,
        #[context]
        address: IpAddr,
    },

    #[event(
        labels(outcome = BfbCopyOutcome::Error),
        log = error,
        message = "BFB copy finished"
    )]
    Error {
        #[observation]
        took: Duration,
        #[context]
        address: IpAddr,
        #[context]
        error: String,
    },

    #[event(
        labels(outcome = BfbCopyOutcome::Timeout),
        log = error,
        message = "BFB copy finished"
    )]
    Timeout {
        #[observation]
        took: Duration,
        #[context]
        address: IpAddr,
        #[context]
        error: String,
    },
}
/// The Redfish route a preingestion firmware upload went through, as a
/// bounded metric label: `SimpleUpdate` is the BFB image-URI path,
/// `Multipart` the standard file push, and `HttpPush` the fallback when a
/// BMC does not support multipart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum FirmwareUploadMethod {
    SimpleUpdate,
    Multipart,
    HttpPush,
}

/// One firmware upload attempt. A success is counted and stays quiet for
/// every method; each failure keeps the level and wording its method had.
#[derive(Event)]
#[event(
    event_name = "preingestion_firmware_upload_finished",
    metric_name = "carbide_preingestion_firmware_upload_total",
    component = "preingestion-manager",
    metric = counter,
    describe = "Number of preingestion firmware uploads to a BMC, by upload method and outcome.",
    labels(outcome: Outcome, method: FirmwareUploadMethod),
)]
pub(crate) enum FirmwareUploadFinished {
    /// Counted, never logged, so it holds only what the metric needs.
    #[event(labels(outcome = Outcome::Ok), log = off)]
    Succeeded {
        #[label]
        method: FirmwareUploadMethod,
    },

    #[event(
        labels(outcome = Outcome::Error, method = FirmwareUploadMethod::SimpleUpdate),
        log = error,
        message = "Simple firmware update failed"
    )]
    SimpleUpdateFailed {
        #[context]
        bmc_ip_address: IpAddr,
        #[context]
        error: String,
    },

    #[event(
        labels(outcome = Outcome::Error, method = FirmwareUploadMethod::Multipart),
        log = warn,
        message = "Failed to upload firmware via multipart update"
    )]
    MultipartFailed {
        #[context]
        bmc_ip_address: IpAddr,
        #[context]
        error: String,
    },

    #[event(
        labels(outcome = Outcome::Error, method = FirmwareUploadMethod::HttpPush),
        log = error,
        message = "Failed to upload firmware via HttpPushUri"
    )]
    HttpPushFailed {
        #[context]
        bmc_ip_address: IpAddr,
        #[context]
        error: String,
    },
}
/// A BMC declined the multipart route, so the upload falls back to
/// `HttpPushUri`. Declining a route is not an upload result: the upload has not
/// failed and its real result arrives on the following `http_push`
/// [`FirmwareUploadFinished`]. This Event is therefore the WARN alone, so a
/// fallback never counts against the multipart failure series.
#[derive(Event)]
#[event(
    event_name = "preingestion_firmware_upload_multipart_unsupported",
    component = "preingestion-manager",
    log = warn,
    metric = none,
    message = "Multipart firmware update is not supported; trying HttpPushUri"
)]
pub(crate) struct MultipartFirmwareUploadUnsupported {
    /// The BMC whose multipart route rejected the upload.
    #[context]
    pub(crate) bmc_ip_address: IpAddr,
    /// The Redfish unsupported-route error that triggered fallback.
    #[context]
    pub(crate) error: String,
}

/// `FirmwareComponentType` as a bounded metric label. The manual impl is the
/// framework's reviewed escape hatch: the type is a fieldless enum in
/// `model::firmware` (bounded by construction), and the orphan rule keeps the
/// derive out of reach from here. The rendering mirrors what
/// `#[derive(LabelValue)]` would produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FirmwareComponentLabel(pub(crate) FirmwareComponentType);

impl LabelValue for FirmwareComponentLabel {
    fn label_value(&self) -> StringValue {
        StringValue::from(match self.0 {
            FirmwareComponentType::Bmc => "bmc",
            FirmwareComponentType::Cec => "cec",
            FirmwareComponentType::Uefi => "uefi",
            FirmwareComponentType::Nic => "nic",
            FirmwareComponentType::CpldMb => "cpld_mb",
            FirmwareComponentType::CpldPdb => "cpld_pdb",
            FirmwareComponentType::HGXBmc => "hgx_bmc",
            FirmwareComponentType::CombinedBmcUefi => "combined_bmc_uefi",
            FirmwareComponentType::Gpu => "gpu",
            FirmwareComponentType::Cx7 => "cx7",
            FirmwareComponentType::Unknown => "unknown",
        })
    }
}

/// The terminal state a firmware upgrade's Redfish task reported, as a
/// bounded metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum UpgradeTaskFinalState {
    Completed,
    Exception,
    Interrupted,
    Killed,
    Cancelled,
}

impl UpgradeTaskFinalState {
    /// Maps a failed Redfish task state onto the label. `Killed` doubles as
    /// the fallback for anything outside the failure states the caller
    /// matches -- the same fallback the site's failure text has always used
    /// for an absent state.
    pub(crate) fn from_failed_task_state(state: TaskState) -> Self {
        match state {
            TaskState::Exception => Self::Exception,
            TaskState::Interrupted => Self::Interrupted,
            TaskState::Cancelled => Self::Cancelled,
            _ => Self::Killed,
        }
    }
}

/// One firmware upgrade task reaching a final state.
#[derive(Event)]
#[event(
    event_name = "preingestion_firmware_upgrade_task_finished",
    metric_name = "carbide_preingestion_firmware_upgrade_tasks_total",
    component = "preingestion-manager",
    metric = counter,
    describe = "Number of preingestion firmware upgrade Redfish tasks reaching a terminal \
                state, by firmware component, final task state, and outcome.",
    labels(outcome: Outcome, firmware: FirmwareComponentLabel, final_state: UpgradeTaskFinalState),
)]
pub(crate) enum FirmwareUpgradeTaskFinished {
    /// Counted, never logged, so it holds only what the metric needs.
    #[event(labels(outcome = Outcome::Ok), log = off)]
    Ok {
        #[label]
        firmware: FirmwareComponentLabel,
        #[label]
        final_state: UpgradeTaskFinalState,
    },

    #[event(
        labels(outcome = Outcome::Error),
        log = warn,
        message = "Firmware upgrade task finished"
    )]
    Error {
        #[label]
        firmware: FirmwareComponentLabel,
        #[label]
        final_state: UpgradeTaskFinalState,
        #[context]
        address: IpAddr,
        #[context]
        error: String,
    },
}
/// The Redfish power operation performed, as a bounded metric label. Host
/// power controls mirror `SystemPowerControl` variant for variant; the BMC
/// and chassis resets are the two reset calls preingestion also issues.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(crate) enum PowerOperation {
    On,
    GracefulShutdown,
    ForceOff,
    GracefulRestart,
    ForceRestart,
    AcPowercycle,
    PowerCycle,
    BmcReset,
    ChassisReset,
}

impl From<SystemPowerControl> for PowerOperation {
    fn from(control: SystemPowerControl) -> Self {
        match control {
            SystemPowerControl::On => Self::On,
            SystemPowerControl::GracefulShutdown => Self::GracefulShutdown,
            SystemPowerControl::ForceOff => Self::ForceOff,
            SystemPowerControl::GracefulRestart => Self::GracefulRestart,
            SystemPowerControl::ForceRestart => Self::ForceRestart,
            SystemPowerControl::ACPowercycle => Self::AcPowercycle,
            SystemPowerControl::PowerCycle => Self::PowerCycle,
        }
    }
}

impl PowerOperation {
    /// Whether `RedfishError::UnnecessaryOperation` means this operation's
    /// goal already held. libredfish maps every HTTP 409 onto that error: for
    /// an operation that targets a power state a 409 means "already in the
    /// requested state", which is success in all but name. Restarts and
    /// powercycles are transitions with no requested state to already be in
    /// -- a 409 is the BMC refusing the operation (a powercycle on a chassis
    /// that must first be off, a restart of a host that is not running) --
    /// and the BMC and chassis resets likewise, so for all of those it stays
    /// an error.
    fn treats_unnecessary_as_ok(self) -> bool {
        matches!(self, Self::On | Self::GracefulShutdown | Self::ForceOff)
    }
}

/// Which preingestion step requested a power operation. This stays in log
/// context -- `operation` is the bounded metric label, while `power_step`
/// preserves the caller-specific diagnostic when the same operation appears
/// in several workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerControlStep {
    PowerOff,
    AcPowercyclePrerequisite,
    AcPowercycle,
    PowerOn,
    UefiReboot,
    BmcReboot,
    CecChassisReset,
    CecChassisResetUnsupported,
    RecoveryPowerOff,
    RecoveryPowerOffNotNeeded,
    RecoveryBmcReset,
    RecoveryPowerOn,
    RecoveryPowerOnNotNeeded,
    RecoveryPowerControl,
}

impl fmt::Display for PowerControlStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::PowerOff => "power_off",
            Self::AcPowercyclePrerequisite => "ac_powercycle_prerequisite",
            Self::AcPowercycle => "ac_powercycle",
            Self::PowerOn => "power_on",
            Self::UefiReboot => "uefi_reboot",
            Self::BmcReboot => "bmc_reboot",
            Self::CecChassisReset => "cec_chassis_reset",
            Self::CecChassisResetUnsupported => "cec_chassis_reset_unsupported",
            Self::RecoveryPowerOff => "recovery_power_off",
            Self::RecoveryPowerOffNotNeeded => "recovery_power_off_not_needed",
            Self::RecoveryBmcReset => "recovery_bmc_reset",
            Self::RecoveryPowerOn => "recovery_power_on",
            Self::RecoveryPowerOnNotNeeded => "recovery_power_on_not_needed",
            Self::RecoveryPowerControl => "recovery_power_control",
        })
    }
}

/// Log context for one wrapped power operation. Keeping this selection at the
/// call site lets the Event retain each workflow's existing message and fields
/// without turning BMC addresses, retry counts, or workflow names into metric
/// labels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PowerControlLog {
    Step {
        bmc_ip_address: IpAddr,
        step: PowerControlStep,
    },
    InitialBmcReset {
        bmc_ip_address: IpAddr,
        attempt: u32,
        max_attempts: u32,
    },
    RecoverySequence {
        bmc_ip_address: IpAddr,
    },
    BfbPlatformPowercycle {
        dpu_bmc_ip_address: IpAddr,
        host_bmc_ip_address: IpAddr,
        post_install: bool,
    },
}

/// The one metric the Events below record.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_preingestion_power_control_total",
    kind = counter,
    component = "preingestion-manager",
    describe = "Number of preingestion Redfish power operations (host power control, BMC and \
    chassis resets), by operation and outcome."
)]
pub(crate) struct PreingestionPowerControl {
    operation: PowerOperation,
    outcome: Outcome,
}

/// A preingestion Redfish power operation completed. Every call updates the
/// existing counter; failures also retain the caller's terminal record. A
/// successful operation stays silent, while an already-satisfied recovery
/// step keeps its historical `DEBUG` record.
#[derive(Event)]
#[event(
    event_name = "preingestion_power_control_finished",
    metric_family = PreingestionPowerControl,
    log = dynamic,
    message = dynamic
)]
pub(crate) struct PowerControlFinished {
    #[label]
    pub(crate) operation: PowerOperation,
    #[label]
    pub(crate) outcome: Outcome,
    #[context]
    pub(crate) bmc_ip_address: IpAddr,
    #[context]
    pub(crate) power_step: PowerControlStep,
    /// The Redfish failure; empty when the operation returned `Ok`.
    #[context]
    pub(crate) error: String,
}

impl DynamicLog for PowerControlFinished {
    fn log_at(&self) -> LogAt {
        if self.error.is_empty() {
            return LogAt::Off;
        }

        match self.power_step {
            PowerControlStep::RecoveryPowerOffNotNeeded
            | PowerControlStep::RecoveryPowerOnNotNeeded => LogAt::Level(tracing::Level::DEBUG),
            PowerControlStep::RecoveryPowerOff
            | PowerControlStep::RecoveryBmcReset
            | PowerControlStep::RecoveryPowerOn
            | PowerControlStep::RecoveryPowerControl => LogAt::Level(tracing::Level::WARN),
            _ => LogAt::Level(tracing::Level::ERROR),
        }
    }
}

impl DynamicMessage for PowerControlFinished {
    fn message(&self) -> &'static str {
        match self.power_step {
            PowerControlStep::PowerOff => "Failed to power off",
            PowerControlStep::AcPowercyclePrerequisite => "Failed to force off",
            PowerControlStep::AcPowercycle => "Failed to power cycle",
            PowerControlStep::PowerOn => "Failed to power on",
            PowerControlStep::UefiReboot => "Failed to reboot",
            PowerControlStep::BmcReboot => "Failed to reboot BMC",
            PowerControlStep::CecChassisReset => "Failed to call chassis reset",
            PowerControlStep::CecChassisResetUnsupported => {
                "Chassis reset is not supported by current CEC firmware; host power cycle required"
            }
            PowerControlStep::RecoveryPowerOff => "Could not turn off power",
            PowerControlStep::RecoveryPowerOffNotNeeded => "Power off not needed",
            PowerControlStep::RecoveryBmcReset => "Could not reset BMC",
            PowerControlStep::RecoveryPowerOn => "Could not turn on power",
            PowerControlStep::RecoveryPowerOnNotNeeded => "Power on not needed",
            PowerControlStep::RecoveryPowerControl => "Power control failed",
        }
    }
}

/// An initial BMC reset attempt completed. This sibling Event shares the
/// power-control counter while retaining the retry fields and the distinct
/// final-attempt message used by the state machine.
#[derive(Event)]
#[event(
    event_name = "preingestion_initial_bmc_reset_finished",
    metric_family = PreingestionPowerControl,
    log = dynamic,
    message = dynamic
)]
struct InitialBmcResetFinished {
    #[label]
    operation: PowerOperation,
    #[label]
    outcome: Outcome,
    #[context]
    bmc_ip_address: IpAddr,
    #[context(value)]
    attempt: i64,
    #[context(value)]
    max_attempts: i64,
    #[context]
    error: String,
}

impl DynamicLog for InitialBmcResetFinished {
    fn log_at(&self) -> LogAt {
        if self.error.is_empty() {
            LogAt::Off
        } else {
            LogAt::Level(tracing::Level::WARN)
        }
    }
}

impl DynamicMessage for InitialBmcResetFinished {
    fn message(&self) -> &'static str {
        if self.attempt >= self.max_attempts {
            "Initial BMC reset failed; proceeding with preingestion without it"
        } else {
            "Initial BMC reset failed; will retry"
        }
    }
}

/// One host power step of a DPU's BFB platform powercycle. Only `ForceOff` and
/// `On` reach this path. A success is counted and stays quiet; each failure
/// keeps the wording for the step that failed, and both endpoints plus
/// `post_install` ride the record.
#[derive(Event)]
#[event(
    event_name = "preingestion_bfb_platform_power_control_finished",
    metric_family = PreingestionPowerControl
)]
enum BfbPlatformPowerControlFinished {
    /// Counted, never logged, so it holds only what the metric needs.
    #[event(labels(outcome = Outcome::Ok), log = off)]
    Succeeded {
        #[label]
        operation: PowerOperation,
    },

    #[event(
        labels(outcome = Outcome::Error, operation = PowerOperation::ForceOff),
        log = error,
        message = "Failed to power off host during BFB power cycle; will retry"
    )]
    ForceOffFailed {
        #[context]
        dpu_bmc_ip_address: IpAddr,
        #[context]
        host_bmc_ip_address: IpAddr,
        #[context(value)]
        post_install: bool,
        #[context]
        error: String,
    },

    #[event(
        labels(outcome = Outcome::Error, operation = PowerOperation::On),
        log = error,
        message = "Failed to power on host during BFB power cycle; will retry"
    )]
    OnFailed {
        #[context]
        dpu_bmc_ip_address: IpAddr,
        #[context]
        host_bmc_ip_address: IpAddr,
        #[context(value)]
        post_install: bool,
        #[context]
        error: String,
    },
}

/// Wraps one preingestion Redfish power operation: the result is returned
/// untouched, while one Event updates the counter and owns any terminal log.
/// For operations that target a power state (`On`, `GracefulShutdown`,
/// `ForceOff`), `RedfishError::UnnecessaryOperation` counts as `ok` (the
/// requested state already held); for restarts, powercycles, and the BMC and
/// chassis resets it counts as `error` -- there is no state to already be in,
/// so a 409 is a refusal (see [`PowerOperation::treats_unnecessary_as_ok`]).
pub(crate) async fn instrument_power_op<T>(
    operation: PowerOperation,
    call: impl Future<Output = Result<T, RedfishError>>,
    log: PowerControlLog,
) -> Result<T, RedfishError> {
    let result = call.await;
    let outcome = match &result {
        Ok(_) => Outcome::Ok,
        Err(RedfishError::UnnecessaryOperation) if operation.treats_unnecessary_as_ok() => {
            Outcome::Ok
        }
        Err(_) => Outcome::Error,
    };
    let error = result
        .as_ref()
        .err()
        .map(ToString::to_string)
        .unwrap_or_default();

    match log {
        PowerControlLog::Step {
            bmc_ip_address,
            mut step,
        } => {
            if step == PowerControlStep::CecChassisReset && error.contains("is not supported") {
                step = PowerControlStep::CecChassisResetUnsupported;
            }
            emit(PowerControlFinished {
                operation,
                outcome,
                bmc_ip_address,
                power_step: step,
                error,
            });
        }
        PowerControlLog::InitialBmcReset {
            bmc_ip_address,
            attempt,
            max_attempts,
        } => emit(InitialBmcResetFinished {
            operation,
            outcome,
            bmc_ip_address,
            attempt: i64::from(attempt),
            max_attempts: i64::from(max_attempts),
            error,
        }),
        PowerControlLog::RecoverySequence { bmc_ip_address } => {
            let power_step = match (operation, &result) {
                (PowerOperation::ForceOff, Err(RedfishError::UnnecessaryOperation)) => {
                    PowerControlStep::RecoveryPowerOffNotNeeded
                }
                (PowerOperation::ForceOff, _) => PowerControlStep::RecoveryPowerOff,
                (PowerOperation::BmcReset, _) => PowerControlStep::RecoveryBmcReset,
                (PowerOperation::On, Err(RedfishError::UnnecessaryOperation)) => {
                    PowerControlStep::RecoveryPowerOnNotNeeded
                }
                (PowerOperation::On, _) => PowerControlStep::RecoveryPowerOn,
                _ => PowerControlStep::RecoveryPowerControl,
            };
            emit(PowerControlFinished {
                operation,
                outcome,
                bmc_ip_address,
                power_step,
                error,
            });
        }
        PowerControlLog::BfbPlatformPowercycle {
            dpu_bmc_ip_address,
            host_bmc_ip_address,
            post_install,
        } => emit(match (outcome, operation) {
            (Outcome::Ok, _) => BfbPlatformPowerControlFinished::Succeeded { operation },
            (Outcome::Error, PowerOperation::On) => BfbPlatformPowerControlFinished::OnFailed {
                dpu_bmc_ip_address,
                host_bmc_ip_address,
                post_install,
                error,
            },
            // Only `ForceOff` and `On` reach this path.
            (Outcome::Error, _) => BfbPlatformPowerControlFinished::ForceOffFailed {
                dpu_bmc_ip_address,
                host_bmc_ip_address,
                post_install,
                error,
            },
        }),
    }
    result
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use carbide_instrument::testing::{ApproxHistogramSum, MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values, value_scenarios};
    use carbide_utils::test_support::test_meter::TestMeter;
    use prometheus_text_parser::ParsedPrometheusMetrics;

    use super::*;

    #[test]
    fn test_metrics_collector() {
        let mut metrics = PreingestionMetrics::new();
        metrics.delayed_uploading = 10;
        metrics.waiting_for_installation = 15;
        metrics.machines_in_preingestion = 20;
        let test_meter = TestMeter::default();

        let metric_holder = Arc::new(MetricHolder::new(test_meter.meter(), Duration::MAX));
        metric_holder.update_metrics(metrics);

        assert_eq!(
            test_meter
                .export_metrics()
                .parse::<ParsedPrometheusMetrics>()
                .unwrap(),
            include_str!("fixtures/test_metrics_collector.txt")
                .parse::<ParsedPrometheusMetrics>()
                .unwrap()
        );
    }

    /// The label vocabularies are the dashboard contract: every power
    /// operation renders as its snake_case name, and the `From` mapping
    /// covers `SystemPowerControl` variant for variant.
    #[test]
    fn power_operation_label_covers_every_system_power_control() {
        value_scenarios!(
            run = |operation: PowerOperation| operation.label_value().to_string();
            "mapped from SystemPowerControl" {
                PowerOperation::from(SystemPowerControl::On) => "on".to_string(),
                PowerOperation::from(SystemPowerControl::GracefulShutdown) => "graceful_shutdown".to_string(),
                PowerOperation::from(SystemPowerControl::ForceOff) => "force_off".to_string(),
                PowerOperation::from(SystemPowerControl::GracefulRestart) => "graceful_restart".to_string(),
                PowerOperation::from(SystemPowerControl::ForceRestart) => "force_restart".to_string(),
                PowerOperation::from(SystemPowerControl::ACPowercycle) => "ac_powercycle".to_string(),
                PowerOperation::from(SystemPowerControl::PowerCycle) => "power_cycle".to_string(),
            }

            "not reachable from SystemPowerControl" {
                PowerOperation::BmcReset => "bmc_reset".to_string(),
                PowerOperation::ChassisReset => "chassis_reset".to_string(),
            }
        );
    }

    /// The manual `LabelValue` impl must render exactly what the derive
    /// would: the variant's snake_case name, for every component type.
    #[test]
    fn firmware_component_label_renders_snake_case() {
        value_scenarios!(
            run = |component: FirmwareComponentType| {
                FirmwareComponentLabel(component).label_value().to_string()
            };
            "every component type" {
                FirmwareComponentType::Bmc => "bmc".to_string(),
                FirmwareComponentType::Cec => "cec".to_string(),
                FirmwareComponentType::Uefi => "uefi".to_string(),
                FirmwareComponentType::Nic => "nic".to_string(),
                FirmwareComponentType::CpldMb => "cpld_mb".to_string(),
                FirmwareComponentType::CpldPdb => "cpld_pdb".to_string(),
                FirmwareComponentType::HGXBmc => "hgx_bmc".to_string(),
                FirmwareComponentType::CombinedBmcUefi => "combined_bmc_uefi".to_string(),
                FirmwareComponentType::Gpu => "gpu".to_string(),
                FirmwareComponentType::Cx7 => "cx7".to_string(),
                FirmwareComponentType::Unknown => "unknown".to_string(),
            }
        );
    }

    /// `from_failed_task_state` maps each terminal failure to its own label
    /// value, and anything outside the failure set falls back to `killed`.
    #[test]
    fn upgrade_task_final_state_maps_failure_states() {
        check_values(
            [
                Check {
                    scenario: "exception",
                    input: TaskState::Exception,
                    expect: "exception".to_string(),
                },
                Check {
                    scenario: "interrupted",
                    input: TaskState::Interrupted,
                    expect: "interrupted".to_string(),
                },
                Check {
                    scenario: "cancelled",
                    input: TaskState::Cancelled,
                    expect: "cancelled".to_string(),
                },
                Check {
                    scenario: "killed",
                    input: TaskState::Killed,
                    expect: "killed".to_string(),
                },
                Check {
                    scenario: "fallback outside the failure set",
                    input: TaskState::Running,
                    expect: "killed".to_string(),
                },
            ],
            |state| {
                UpgradeTaskFinalState::from_failed_task_state(state)
                    .label_value()
                    .to_string()
            },
        );
    }

    /// One emit per copy: the histogram records the duration under the copy's
    /// outcome, and the event owns the completion line -- INFO for a success,
    /// ERROR for a failure or timeout.
    #[test]
    fn bfb_copy_finished_records_duration_and_owns_the_completion_line() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(BfbCopyFinished::Ok {
                took: Duration::from_secs(90),
                address: IpAddr::from([10, 0, 0, 5]),
            });
            emit(BfbCopyFinished::Error {
                took: Duration::from_secs(30),
                address: IpAddr::from([10, 0, 0, 6]),
                error: "ssh connection reset".to_string(),
            });
            emit(BfbCopyFinished::Timeout {
                took: Duration::from_secs(2100),
                address: IpAddr::from([10, 0, 0, 7]),
                error: "BFB copy timed out after 35 minutes".to_string(),
            });
        });

        assert_eq!(logs.len(), 3, "every outcome writes its line: {logs:?}");
        assert_eq!(logs[0].level, tracing::Level::INFO);
        assert_eq!(logs[1].level, tracing::Level::ERROR);
        assert_eq!(logs[2].level, tracing::Level::ERROR);

        for (outcome, seconds) in [("ok", 90.0), ("error", 30.0), ("timeout", 2100.0)] {
            assert_eq!(
                metrics.histogram_count_delta(
                    "carbide_preingestion_bfb_copy_duration_seconds",
                    &[("outcome", outcome)],
                ),
                1,
                "one observation under outcome={outcome}",
            );
            let sum = metrics.histogram_sum_delta(
                "carbide_preingestion_bfb_copy_duration_seconds",
                &[("outcome", outcome)],
            );
            assert_eq!(
                sum,
                ApproxHistogramSum(seconds),
                "outcome={outcome} records {seconds}s"
            );
        }
    }

    #[derive(Clone, Copy)]
    struct FirmwareUploadInput {
        method: FirmwareUploadMethod,
        outcome: Outcome,
        error: &'static str,
    }

    #[derive(Debug, PartialEq)]
    struct FirmwareUploadObservation {
        counter_delta: f64,
        logs: Vec<FirmwareUploadLog>,
    }

    #[derive(Debug, PartialEq)]
    struct FirmwareUploadLog {
        level: tracing::Level,
        metadata_name: String,
        message: String,
        event_name: Option<String>,
        metric_name: Option<String>,
        method: Option<String>,
        outcome: Option<String>,
        bmc_ip_address: Option<String>,
        error: Option<String>,
    }

    fn observe_firmware_upload(input: FirmwareUploadInput) -> FirmwareUploadObservation {
        let metrics = MetricsCapture::start();
        let bmc_ip_address = IpAddr::from([10, 0, 0, 5]);
        let method_label = input.method.label_value();
        let outcome_label = input.outcome.label_value();
        let logs = capture_logs(|| {
            let error = input.error.to_string();
            emit(match (input.outcome, input.method) {
                (Outcome::Ok, method) => FirmwareUploadFinished::Succeeded { method },
                (Outcome::Error, FirmwareUploadMethod::SimpleUpdate) => {
                    FirmwareUploadFinished::SimpleUpdateFailed {
                        bmc_ip_address,
                        error,
                    }
                }
                (Outcome::Error, FirmwareUploadMethod::Multipart) => {
                    FirmwareUploadFinished::MultipartFailed {
                        bmc_ip_address,
                        error,
                    }
                }
                (Outcome::Error, FirmwareUploadMethod::HttpPush) => {
                    FirmwareUploadFinished::HttpPushFailed {
                        bmc_ip_address,
                        error,
                    }
                }
            });
        })
        .into_iter()
        .map(|log| {
            let event_name = log.field("event_name").map(str::to_owned);
            let metric_name = log.field("metric_name").map(str::to_owned);
            let method = log.field("method").map(str::to_owned);
            let outcome = log.field("outcome").map(str::to_owned);
            let bmc_ip_address = log.field("bmc_ip_address").map(str::to_owned);
            let error = log.field("error").map(str::to_owned);
            FirmwareUploadLog {
                level: log.level,
                metadata_name: log.metadata_name,
                message: log.message,
                event_name,
                metric_name,
                method,
                outcome,
                bmc_ip_address,
                error,
            }
        })
        .collect();

        FirmwareUploadObservation {
            counter_delta: metrics.counter_delta(
                "carbide_preingestion_firmware_upload_total",
                &[
                    ("method", method_label.as_str()),
                    ("outcome", outcome_label.as_str()),
                ],
            ),
            logs,
        }
    }

    fn expected_firmware_upload(
        method: FirmwareUploadMethod,
        outcome: Outcome,
        error: &str,
        log: Option<(tracing::Level, &str, &str)>,
    ) -> FirmwareUploadObservation {
        let method = method.label_value();
        let outcome = outcome.label_value();
        let logs = log
            .map(|(level, event_name, message)| FirmwareUploadLog {
                level,
                metadata_name: event_name.to_string(),
                message: message.to_string(),
                event_name: Some(event_name.to_string()),
                metric_name: Some("carbide_preingestion_firmware_upload_total".to_string()),
                method: Some(method.as_str().to_string()),
                outcome: Some(outcome.as_str().to_string()),
                bmc_ip_address: Some("10.0.0.5".to_string()),
                error: Some(error.to_string()),
            })
            .into_iter()
            .collect();

        FirmwareUploadObservation {
            counter_delta: 1.0,
            logs,
        }
    }

    /// Each upload attempt increments its existing `method`/`outcome` series.
    /// Successes stay silent; failures also write the route-specific record
    /// with `bmc_ip_address` and `error` as diagnostic context.
    #[test]
    fn firmware_upload_outcomes_emit_their_metric_and_historical_log() {
        check_values(
            [
                Check {
                    scenario: "SimpleUpdate success",
                    input: FirmwareUploadInput {
                        method: FirmwareUploadMethod::SimpleUpdate,
                        outcome: Outcome::Ok,
                        error: "",
                    },
                    expect: expected_firmware_upload(
                        FirmwareUploadMethod::SimpleUpdate,
                        Outcome::Ok,
                        "",
                        None,
                    ),
                },
                Check {
                    scenario: "SimpleUpdate failure",
                    input: FirmwareUploadInput {
                        method: FirmwareUploadMethod::SimpleUpdate,
                        outcome: Outcome::Error,
                        error: "simple update failed",
                    },
                    expect: expected_firmware_upload(
                        FirmwareUploadMethod::SimpleUpdate,
                        Outcome::Error,
                        "simple update failed",
                        Some((
                            tracing::Level::ERROR,
                            "preingestion_firmware_upload_finished",
                            "Simple firmware update failed",
                        )),
                    ),
                },
                Check {
                    scenario: "HttpPush success",
                    input: FirmwareUploadInput {
                        method: FirmwareUploadMethod::HttpPush,
                        outcome: Outcome::Ok,
                        error: "",
                    },
                    expect: expected_firmware_upload(
                        FirmwareUploadMethod::HttpPush,
                        Outcome::Ok,
                        "",
                        None,
                    ),
                },
                Check {
                    scenario: "HttpPush failure",
                    input: FirmwareUploadInput {
                        method: FirmwareUploadMethod::HttpPush,
                        outcome: Outcome::Error,
                        error: "HTTP push failed",
                    },
                    expect: expected_firmware_upload(
                        FirmwareUploadMethod::HttpPush,
                        Outcome::Error,
                        "HTTP push failed",
                        Some((
                            tracing::Level::ERROR,
                            "preingestion_firmware_upload_finished",
                            "Failed to upload firmware via HttpPushUri",
                        )),
                    ),
                },
            ],
            observe_firmware_upload,
        );
    }

    /// A declined multipart route writes its WARN and leaves the upload counter
    /// alone: the fallback that follows records the upload's only result, so the
    /// multipart failure series stays a count of real multipart failures.
    #[test]
    fn a_declined_multipart_route_does_not_count_as_an_upload_failure() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(MultipartFirmwareUploadUnsupported {
                bmc_ip_address: IpAddr::from([10, 0, 0, 6]),
                error: "multipart is unsupported".to_string(),
            });
            emit(FirmwareUploadFinished::Succeeded {
                method: FirmwareUploadMethod::HttpPush,
            });
        });

        assert_eq!(logs.len(), 1, "only the declined route logs: {logs:?}");
        assert_eq!(logs[0].level, tracing::Level::WARN);
        assert_eq!(
            logs[0].field("event_name"),
            Some("preingestion_firmware_upload_multipart_unsupported"),
        );
        assert_eq!(
            logs[0].field("metric_name"),
            None,
            "a declined route records no metric",
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_preingestion_firmware_upload_total",
                &[("method", "multipart"), ("outcome", "error")],
            ),
            0.0,
            "the fallback must not count against multipart failures",
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_preingestion_firmware_upload_total",
                &[("method", "http_push"), ("outcome", "ok")],
            ),
            1.0,
            "the upload's real result is the one the counter records",
        );
    }

    /// `FirmwareUpgradeTaskFinished` counts every terminal task state.
    /// Successes stay silent; failures also write the WARN record with
    /// `address` and `error` as diagnostic context.
    #[test]
    fn firmware_upgrade_task_failures_own_the_warn_line() {
        let metrics = MetricsCapture::start();
        let logs = capture_logs(|| {
            emit(FirmwareUpgradeTaskFinished::Ok {
                firmware: FirmwareComponentLabel(FirmwareComponentType::Bmc),
                final_state: UpgradeTaskFinalState::Completed,
            });
            emit(FirmwareUpgradeTaskFinished::Error {
                firmware: FirmwareComponentLabel(FirmwareComponentType::Uefi),
                final_state: UpgradeTaskFinalState::Exception,

                address: IpAddr::from([10, 0, 0, 6]),
                error: "flash verification failed".to_string(),
            });
        });

        assert_eq!(logs.len(), 1, "only the failure logs: {logs:?}");
        assert_eq!(logs[0].level, tracing::Level::WARN);

        assert_eq!(
            metrics.counter_delta(
                "carbide_preingestion_firmware_upgrade_tasks_total",
                &[
                    ("firmware", "bmc"),
                    ("final_state", "completed"),
                    ("outcome", "ok"),
                ],
            ),
            1.0,
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_preingestion_firmware_upgrade_tasks_total",
                &[
                    ("firmware", "uefi"),
                    ("final_state", "exception"),
                    ("outcome", "error"),
                ],
            ),
            1.0,
        );
    }

    /// `UnnecessaryOperation` is successful only for operations whose target
    /// is a durable power state. Restarts, powercycles, and reset calls ask the
    /// BMC to perform a transition, so the same HTTP 409 remains an error.
    #[test]
    fn power_operation_classifies_unnecessary_operation() {
        value_scenarios!(run = |operation| operation.treats_unnecessary_as_ok();
            "state targets" {
                PowerOperation::On => true,
                PowerOperation::GracefulShutdown => true,
                PowerOperation::ForceOff => true,
            }

            "transitions and reset calls" {
                PowerOperation::GracefulRestart => false,
                PowerOperation::ForceRestart => false,
                PowerOperation::AcPowercycle => false,
                PowerOperation::PowerCycle => false,
                PowerOperation::BmcReset => false,
                PowerOperation::ChassisReset => false,
            }
        );
    }

    #[derive(Clone, Copy)]
    enum PowerCall {
        Ok,
        Unnecessary,
        NotSupported(&'static str),
    }

    impl PowerCall {
        fn result(self) -> Result<(), RedfishError> {
            match self {
                Self::Ok => Ok(()),
                Self::Unnecessary => Err(RedfishError::UnnecessaryOperation),
                Self::NotSupported(error) => Err(RedfishError::NotSupported(error.to_string())),
            }
        }

        fn metric_outcome(self, operation: PowerOperation) -> Outcome {
            match self {
                Self::Ok => Outcome::Ok,
                Self::Unnecessary if operation.treats_unnecessary_as_ok() => Outcome::Ok,
                Self::Unnecessary | Self::NotSupported(_) => Outcome::Error,
            }
        }
    }

    struct PowerControlInput {
        operation: PowerOperation,
        call: PowerCall,
        log: PowerControlLog,
    }

    #[derive(Debug, PartialEq)]
    struct PowerControlObservation {
        returned_error: bool,
        counter_delta: f64,
        logs_are_correlated: bool,
        logs: Vec<String>,
    }

    fn observe_power_control(input: PowerControlInput) -> PowerControlObservation {
        use futures_util::FutureExt as _;

        let metrics = MetricsCapture::start();
        let metric_outcome = input.call.metric_outcome(input.operation);
        let operation_label = input.operation.label_value();
        let outcome_label = metric_outcome.label_value();
        let mut returned_error = false;
        let captured_logs = capture_logs(|| {
            returned_error = instrument_power_op(
                input.operation,
                std::future::ready(input.call.result()),
                input.log,
            )
            .now_or_never()
            .expect("ready future")
            .is_err();
        });
        let logs_are_correlated = captured_logs.iter().all(|log| {
            log.field("event_name") == Some(log.metadata_name.as_str())
                && log.field("metric_name") == Some("carbide_preingestion_power_control_total")
        });
        let logs = captured_logs
        .into_iter()
        .map(|log| {
            let field = |name| log.field(name).unwrap_or("-");
            format!(
                "{:?}|{}|{}|operation={}|outcome={}|bmc={}|dpu={}|host={}|step={}|attempt={}|max={}|post_install={}|error={}",
                log.level,
                log.metadata_name,
                log.message,
                field("operation"),
                field("outcome"),
                field("bmc_ip_address"),
                field("dpu_bmc_ip_address"),
                field("host_bmc_ip_address"),
                field("power_step"),
                field("attempt"),
                field("max_attempts"),
                field("post_install"),
                field("error"),
            )
        })
        .collect();

        PowerControlObservation {
            returned_error,
            counter_delta: metrics.counter_delta(
                "carbide_preingestion_power_control_total",
                &[
                    ("operation", operation_label.as_str()),
                    ("outcome", outcome_label.as_str()),
                ],
            ),
            logs_are_correlated,
            logs,
        }
    }

    fn expected_power_control(returned_error: bool, log: Option<&str>) -> PowerControlObservation {
        PowerControlObservation {
            returned_error,
            counter_delta: 1.0,
            logs_are_correlated: true,
            logs: log.map(str::to_string).into_iter().collect(),
        }
    }

    #[test]
    fn power_control_steps_retain_their_log_contract() {
        value_scenarios!(run = |power_step| {
            let event = PowerControlFinished {
                operation: PowerOperation::ForceOff,
                outcome: Outcome::Error,
                bmc_ip_address: IpAddr::from([10, 0, 0, 5]),
                power_step,
                error: "power control failed".to_string(),
            };
            let level = match DynamicLog::log_at(&event) {
                LogAt::Off => None,
                LogAt::Level(level) => Some(level),
            };
            (level, DynamicMessage::message(&event))
        };
            "workflow errors" {
                PowerControlStep::PowerOff => (Some(tracing::Level::ERROR), "Failed to power off"),
                PowerControlStep::AcPowercyclePrerequisite => (Some(tracing::Level::ERROR), "Failed to force off"),
                PowerControlStep::AcPowercycle => (Some(tracing::Level::ERROR), "Failed to power cycle"),
                PowerControlStep::PowerOn => (Some(tracing::Level::ERROR), "Failed to power on"),
                PowerControlStep::UefiReboot => (Some(tracing::Level::ERROR), "Failed to reboot"),
                PowerControlStep::BmcReboot => (Some(tracing::Level::ERROR), "Failed to reboot BMC"),
                PowerControlStep::CecChassisReset => (Some(tracing::Level::ERROR), "Failed to call chassis reset"),
                PowerControlStep::CecChassisResetUnsupported => (Some(tracing::Level::ERROR), "Chassis reset is not supported by current CEC firmware; host power cycle required"),
            }

            "recovery sequence" {
                PowerControlStep::RecoveryPowerOff => (Some(tracing::Level::WARN), "Could not turn off power"),
                PowerControlStep::RecoveryPowerOffNotNeeded => (Some(tracing::Level::DEBUG), "Power off not needed"),
                PowerControlStep::RecoveryBmcReset => (Some(tracing::Level::WARN), "Could not reset BMC"),
                PowerControlStep::RecoveryPowerOn => (Some(tracing::Level::WARN), "Could not turn on power"),
                PowerControlStep::RecoveryPowerOnNotNeeded => (Some(tracing::Level::DEBUG), "Power on not needed"),
                PowerControlStep::RecoveryPowerControl => (Some(tracing::Level::WARN), "Power control failed"),
            }
        );
    }

    #[test]
    fn bfb_platform_power_operations_retain_their_messages() {
        let message = |operation| {
            let event = match operation {
                PowerOperation::On => BfbPlatformPowerControlFinished::OnFailed {
                    dpu_bmc_ip_address: IpAddr::from([10, 0, 0, 6]),
                    host_bmc_ip_address: IpAddr::from([10, 0, 0, 5]),
                    post_install: false,
                    error: "power control failed".to_string(),
                },
                _ => BfbPlatformPowerControlFinished::ForceOffFailed {
                    dpu_bmc_ip_address: IpAddr::from([10, 0, 0, 6]),
                    host_bmc_ip_address: IpAddr::from([10, 0, 0, 5]),
                    post_install: false,
                    error: "power control failed".to_string(),
                },
            };
            carbide_instrument::Event::message(&event)
        };

        value_scenarios!(run = message;
            "operation" {
                PowerOperation::ForceOff => "Failed to power off host during BFB power cycle; will retry",
                PowerOperation::On => "Failed to power on host during BFB power cycle; will retry",
            }
        );
    }

    /// Every wrapped call increments its existing `operation`/`outcome`
    /// series. Successes stay silent; each failure retains the level, message,
    /// and caller-specific fields that previously lived beside the wrapper.
    #[test]
    fn power_control_results_emit_their_metric_and_historical_log() {
        let bmc_ip_address = IpAddr::from([10, 0, 0, 5]);
        check_values(
            [
                Check {
                    scenario: "successful workflow step",
                    input: PowerControlInput {
                        operation: PowerOperation::ForceOff,
                        call: PowerCall::Ok,
                        log: PowerControlLog::Step {
                            bmc_ip_address,
                            step: PowerControlStep::PowerOff,
                        },
                    },
                    expect: expected_power_control(false, None),
                },
                Check {
                    scenario: "workflow failure",
                    input: PowerControlInput {
                        operation: PowerOperation::ForceOff,
                        call: PowerCall::NotSupported("power control unavailable"),
                        log: PowerControlLog::Step {
                            bmc_ip_address,
                            step: PowerControlStep::PowerOff,
                        },
                    },
                    expect: expected_power_control(
                        true,
                        Some(
                            "Level(Error)|preingestion_power_control_finished|Failed to power off|operation=force_off|outcome=error|bmc=10.0.0.5|dpu=-|host=-|step=power_off|attempt=-|max=-|post_install=-|error=BMC vendor does not support this operation: power control unavailable",
                        ),
                    ),
                },
                Check {
                    scenario: "already-off workflow failure still logs but counts as ok",
                    input: PowerControlInput {
                        operation: PowerOperation::ForceOff,
                        call: PowerCall::Unnecessary,
                        log: PowerControlLog::Step {
                            bmc_ip_address,
                            step: PowerControlStep::PowerOff,
                        },
                    },
                    expect: expected_power_control(
                        true,
                        Some(
                            "Level(Error)|preingestion_power_control_finished|Failed to power off|operation=force_off|outcome=ok|bmc=10.0.0.5|dpu=-|host=-|step=power_off|attempt=-|max=-|post_install=-|error=UnnecessaryOperation such as trying to turn on a machine that is already on.",
                        ),
                    ),
                },
                Check {
                    scenario: "already-off recovery step",
                    input: PowerControlInput {
                        operation: PowerOperation::ForceOff,
                        call: PowerCall::Unnecessary,
                        log: PowerControlLog::RecoverySequence { bmc_ip_address },
                    },
                    expect: expected_power_control(
                        true,
                        Some(
                            "Level(Debug)|preingestion_power_control_finished|Power off not needed|operation=force_off|outcome=ok|bmc=10.0.0.5|dpu=-|host=-|step=recovery_power_off_not_needed|attempt=-|max=-|post_install=-|error=UnnecessaryOperation such as trying to turn on a machine that is already on.",
                        ),
                    ),
                },
                Check {
                    scenario: "retryable initial BMC reset",
                    input: PowerControlInput {
                        operation: PowerOperation::BmcReset,
                        call: PowerCall::NotSupported("reset unavailable"),
                        log: PowerControlLog::InitialBmcReset {
                            bmc_ip_address,
                            attempt: 1,
                            max_attempts: 3,
                        },
                    },
                    expect: expected_power_control(
                        true,
                        Some(
                            "Level(Warn)|preingestion_initial_bmc_reset_finished|Initial BMC reset failed; will retry|operation=bmc_reset|outcome=error|bmc=10.0.0.5|dpu=-|host=-|step=-|attempt=1|max=3|post_install=-|error=BMC vendor does not support this operation: reset unavailable",
                        ),
                    ),
                },
                Check {
                    scenario: "final initial BMC reset attempt",
                    input: PowerControlInput {
                        operation: PowerOperation::BmcReset,
                        call: PowerCall::NotSupported("reset unavailable"),
                        log: PowerControlLog::InitialBmcReset {
                            bmc_ip_address,
                            attempt: 3,
                            max_attempts: 3,
                        },
                    },
                    expect: expected_power_control(
                        true,
                        Some(
                            "Level(Warn)|preingestion_initial_bmc_reset_finished|Initial BMC reset failed; proceeding with preingestion without it|operation=bmc_reset|outcome=error|bmc=10.0.0.5|dpu=-|host=-|step=-|attempt=3|max=3|post_install=-|error=BMC vendor does not support this operation: reset unavailable",
                        ),
                    ),
                },
                Check {
                    scenario: "unsupported CEC reset",
                    input: PowerControlInput {
                        operation: PowerOperation::ChassisReset,
                        call: PowerCall::NotSupported("reset is not supported"),
                        log: PowerControlLog::Step {
                            bmc_ip_address,
                            step: PowerControlStep::CecChassisReset,
                        },
                    },
                    expect: expected_power_control(
                        true,
                        Some(
                            "Level(Error)|preingestion_power_control_finished|Chassis reset is not supported by current CEC firmware; host power cycle required|operation=chassis_reset|outcome=error|bmc=10.0.0.5|dpu=-|host=-|step=cec_chassis_reset_unsupported|attempt=-|max=-|post_install=-|error=BMC vendor does not support this operation: reset is not supported",
                        ),
                    ),
                },
                Check {
                    scenario: "BFB platform power failure",
                    input: PowerControlInput {
                        operation: PowerOperation::ForceOff,
                        call: PowerCall::NotSupported("host power unavailable"),
                        log: PowerControlLog::BfbPlatformPowercycle {
                            dpu_bmc_ip_address: IpAddr::from([10, 0, 0, 6]),
                            host_bmc_ip_address: bmc_ip_address,
                            post_install: true,
                        },
                    },
                    expect: expected_power_control(
                        true,
                        Some(
                            "Level(Error)|preingestion_bfb_platform_power_control_finished|Failed to power off host during BFB power cycle; will retry|operation=force_off|outcome=error|bmc=-|dpu=10.0.0.6|host=10.0.0.5|step=-|attempt=-|max=-|post_install=true|error=BMC vendor does not support this operation: host power unavailable",
                        ),
                    ),
                },
            ],
            observe_power_control,
        );
    }

    /// The Event replaces the old counter registration without changing its
    /// HELP text or adding log-only context to the Prometheus label set.
    #[test]
    fn power_control_counter_exposition_stays_stable() {
        let metrics = MetricsCapture::start();
        emit(PowerControlFinished {
            operation: PowerOperation::ForceOff,
            outcome: Outcome::Ok,
            bmc_ip_address: IpAddr::from([10, 0, 0, 5]),
            power_step: PowerControlStep::PowerOff,
            error: String::new(),
        });

        let encoded = metrics.render();
        assert!(encoded.contains(
            "# HELP carbide_preingestion_power_control_total Number of preingestion Redfish power operations (host power control, BMC and chassis resets), by operation and outcome.\n"
        ));
        assert!(encoded.contains("# TYPE carbide_preingestion_power_control_total counter\n"));
        let sample = encoded
            .lines()
            .find(|line| {
                line.starts_with("carbide_preingestion_power_control_total{")
                    && line.contains("operation=\"force_off\"")
                    && line.contains("outcome=\"ok\"")
            })
            .unwrap_or_else(|| panic!("missing force-off/ok power-control sample:\n{encoded}"));
        assert!(!sample.contains("bmc_ip_address"), "{sample}");
        assert!(!sample.contains("power_step"), "{sample}");
    }
}
