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

//! Scout control-loop and stream events.
//!
//! `ScoutActionHandled` and `ScoutStreamReconnect` pair their counters with
//! the records operators use to diagnose those outcomes.
//! `ScoutStreamConnection` remains metric-only because the surrounding stream
//! lifecycle logs already carry the endpoint and machine context.
//! The storage cleanup Event keeps each device's terminal record together with
//! the duration histogram operators use to compare NVMe and HDD/SAS cleanup.

use std::time::Duration;

use carbide_instrument::{Event, LabelValue, MetricFamily, Outcome};
use carbide_uuid::machine::MachineId;
use rpc::forge_agent_control_response as fac;

/// Which control-loop action scout handled, as a bounded metric label: one
/// variant per [`fac::Action`] arm the service loop can dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum ScoutAction {
    Noop,
    Reset,
    Discovery,
    Rebuild,
    Retry,
    Measure,
    LogError,
    MachineValidation,
    MlxAction,
    FirmwareUpgrade,
}

impl From<&fac::Action> for ScoutAction {
    fn from(action: &fac::Action) -> Self {
        match action {
            fac::Action::Noop(_) => Self::Noop,
            fac::Action::Reset(_) => Self::Reset,
            fac::Action::Discovery(_) => Self::Discovery,
            fac::Action::Rebuild(_) => Self::Rebuild,
            fac::Action::Retry(_) => Self::Retry,
            fac::Action::Measure(_) => Self::Measure,
            fac::Action::LogError(_) => Self::LogError,
            fac::Action::MachineValidation(_) => Self::MachineValidation,
            fac::Action::MlxAction(_) => Self::MlxAction,
            fac::Action::FirmwareUpgrade(_) => Self::FirmwareUpgrade,
        }
    }
}

/// One scout action served. Each variant is the result.
#[derive(Event)]
#[event(
    event_name = "scout_action_handled",
    metric_name = "carbide_scout_actions_total",
    component = "nico-scout",
    metric = counter,
    describe = "Number of scout control-loop actions handled, by action and outcome.",
    labels(outcome: Outcome, action: ScoutAction),
)]
pub(super) enum ScoutActionHandled {
    #[event(
        labels(outcome = Outcome::Ok),
        log = info,
        message = "Successfully served action"
    )]
    Ok {
        #[label]
        action: ScoutAction,
        #[context]
        action_name: &'static str,
    },

    #[event(
        labels(outcome = Outcome::Error),
        log = info,
        message = "Failed to serve action"
    )]
    Error {
        #[label]
        action: ScoutAction,
        #[context]
        action_name: &'static str,
        #[context]
        error: String,
    },
}
/// `ScoutStreamConnection` records whether scout established its bidirectional
/// stream. `error` covers both client construction and the opening stream RPC.
#[derive(Event)]
#[event(
    event_name = "scout_stream_connection",
    metric_name = "carbide_scout_stream_connections_total",
    component = "nico-scout",
    log = off,
    metric = counter,
    describe = "Number of scout stream connection attempts, by outcome."
)]
pub(super) struct ScoutStreamConnection {
    #[label]
    pub(super) outcome: Outcome,
}

/// `ScoutStreamReconnect` records the retry boundary after a stream closes or
/// errors. Endpoint and machine context stay on the warning, while the counter
/// tracks how often scout reaches the fixed reconnect delay.
#[derive(Event)]
#[event(
    event_name = "scout_stream_reconnect",
    metric_name = "carbide_scout_stream_reconnects_total",
    component = "nico-scout",
    log = warn,
    metric = counter,
    message = "scout stream reconnecting after 10s delay",
    describe = "Number of scout stream reconnect cycles after a stream closed or errored."
)]
pub(super) struct ScoutStreamReconnect {
    #[context]
    pub(super) api_endpoint: String,
    #[context]
    pub(super) machine_id: MachineId,
}

/// `ScoutStreamResponseDropped` records a response scout could not return
/// after the outbound request stream closed. The stream loop still breaks and
/// reconnects, while this Event identifies the response that was lost.
#[derive(Event)]
#[event(
    event_name = "scout_stream_response_dropped",
    metric_name = "carbide_scout_stream_responses_dropped_total",
    component = "nico-scout",
    log = error,
    metric = counter,
    message = "scout stream failed to send response",
    describe = "Number of scout stream responses dropped after the outbound request stream closed."
)]
pub(super) struct ScoutStreamResponseDropped {
    #[context]
    pub(super) api_endpoint: String,
    #[context]
    pub(super) machine_id: MachineId,
    #[context]
    pub(super) error: String,
}

/// Which firmware-upgrade input is being downloaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum FirmwareDownloadKind {
    Script,
    Artifact,
}

/// What Scout will do after a failed firmware download attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum FirmwareDownloadNextAction {
    Retry,
    GiveUp,
}

/// A firmware download attempt failed.
#[derive(Event)]
#[event(
    event_name = "scout_firmware_download_attempt_failed",
    metric_name = "carbide_scout_firmware_download_attempt_failures_total",
    component = "nico-scout",
    log = warn,
    metric = counter,
    message = "[firmware_upgrade] download attempt failed; retrying",
    describe = "Number of failed Scout firmware download attempts, by download kind and next action."
)]
pub(super) struct ScoutFirmwareDownloadAttemptFailed {
    #[label]
    pub(super) kind: FirmwareDownloadKind,
    #[label]
    pub(super) next_action: FirmwareDownloadNextAction,
    #[context(value)]
    pub(super) attempt: i64,
    #[context]
    pub(super) url: String,
    #[context]
    pub(super) error: String,
    #[context(value)]
    pub(super) retry_delay_seconds: f64,
}

/// The bounded operation vocabulary for Scout's MLX failure counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum ScoutMlxOperation {
    DeviceReportPublish,
    InfoReport,
    ObservationReport,
    Reconciliation,
    ReconciliationLock,
    ReconciliationUnlock,
    ProfileSync,
    ProfileCompare,
    ProfileApply,
    ProfileReset,
    LockdownLock,
    LockdownUnlock,
    LockdownStatus,
    DeviceInfo,
    RegistryShow,
    ConfigQuery,
    ConfigCompare,
    ConfigSet,
    ConfigSync,
    FirmwareFlash,
}

/// Where an MLX operation failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum ScoutMlxFailureStage {
    Create,
    Publish,
    Decode,
    Validate,
    Discover,
    Initialize,
    Lookup,
    Execute,
    Serialize,
}

/// `ScoutMlxFailures` is the one metric behind every Scout MLX failure below:
/// which operation failed, where it failed, and what class of failure it was.
/// The Events that record it keep their own severity, wording, and diagnostic
/// context, and the derive checks each one against the labels they supply.
#[derive(MetricFamily)]
#[metric(
    name = "carbide_scout_mlx_failures_total",
    kind = counter,
    component = "nico-scout",
    describe = "Number of Scout MLX observation, read, mutation, and recovery failures, by operation and failure stage."
)]
pub(super) struct ScoutMlxFailures {
    operation: ScoutMlxOperation,
    failure_stage: ScoutMlxFailureStage,
}

// The operation, request, device, profile, registry, config, firmware, and
// reconciliation groups all record `ScoutMlxFailures`, but their existing
// diagnostics need different context. Keep separate `Event` structs so each log
// retains its fields without filling unrelated fields with empty values.
//
// Named constructors still own each valid (`operation`, `failure_stage`) pair,
// so a call site names the situation rather than assembling one. The dynamic
// log and message matches keep generic fallbacks: `emit()` is diagnostic
// plumbing and must not panic if a future constructor reaches a pair its
// matching table does not know yet.

/// An MLX failure whose existing diagnostic only holds an error. Each variant
/// is one place a request breaks, fixing the `(operation, failure_stage)` pair
/// that names it and the diagnostic that pair already logged.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_operation_failed",
    metric_family = ScoutMlxFailures
)]
pub(super) enum ScoutMlxOperationFailed {
    #[event(
        labels(
            operation = ScoutMlxOperation::DeviceReportPublish,
            failure_stage = ScoutMlxFailureStage::Create
        ),
        log = warn,
        message = "failed to create PublishMlxDeviceReportRequest"
    )]
    DeviceReportCreate {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::DeviceReportPublish,
            failure_stage = ScoutMlxFailureStage::Publish
        ),
        log = warn,
        message = "failed to publish PublishMlxDeviceReportRequest"
    )]
    DeviceReportPublish {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ObservationReport,
            failure_stage = ScoutMlxFailureStage::Publish
        ),
        log = error,
        message = "Error from publish_mlx_observation_report"
    )]
    ObservationReportPublish {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ProfileCompare,
            failure_stage = ScoutMlxFailureStage::Decode
        ),
        log = error,
        message = "[scout_stream::mlx_device] failed to parse profile"
    )]
    ProfileCompareDecode {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ProfileSync,
            failure_stage = ScoutMlxFailureStage::Decode
        ),
        log = error,
        message = "[scout_stream::mlx_device] failed to parse profile"
    )]
    ProfileSyncDecode {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ProfileCompare,
            failure_stage = ScoutMlxFailureStage::Serialize
        ),
        log = error,
        message = "[scout_stream::mlx_device] profile compare result failed to serialize"
    )]
    ProfileCompareSerialize {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ProfileSync,
            failure_stage = ScoutMlxFailureStage::Serialize
        ),
        log = error,
        message = "[scout_stream::mlx_device] profile sync result failed to serialize"
    )]
    ProfileSyncSerialize {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::LockdownLock,
            failure_stage = ScoutMlxFailureStage::Initialize
        ),
        log = error,
        message = "[scout_stream::mlx_device] lockdown manager initialization failed"
    )]
    LockdownLockInitialize {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::LockdownUnlock,
            failure_stage = ScoutMlxFailureStage::Initialize
        ),
        log = error,
        message = "[scout_stream::mlx_device] lockdown manager initialization failed"
    )]
    LockdownUnlockInitialize {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::LockdownStatus,
            failure_stage = ScoutMlxFailureStage::Initialize
        ),
        log = error,
        message = "[scout_stream::mlx_device] lockdown manager initialization failed"
    )]
    LockdownStatusInitialize {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::InfoReport,
            failure_stage = ScoutMlxFailureStage::Decode
        ),
        log = error,
        message = "[scout_stream::mlx_device] device report request failed to parse filters"
    )]
    InfoReportDecode {
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::InfoReport,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] device report generation failed"
    )]
    InfoReportExecute {
        #[context]
        error: String,
    },
}

/// An MLX request failed validation before it had additional context.
/// Each variant is the operation that was rejected.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_request_rejected",
    metric_family = ScoutMlxFailures
)]
pub(super) enum ScoutMlxRequestRejected {
    #[event(
        labels(
            operation = ScoutMlxOperation::ProfileCompare,
            failure_stage = ScoutMlxFailureStage::Validate
        ),
        log = off,
        message = "no serializable profile data in message"
    )]
    ProfileCompare {},

    #[event(
        labels(
            operation = ScoutMlxOperation::ProfileSync,
            failure_stage = ScoutMlxFailureStage::Validate
        ),
        log = off,
        message = "no serializable profile data in message"
    )]
    ProfileSync {},

    #[event(
        labels(
            operation = ScoutMlxOperation::Reconciliation,
            failure_stage = ScoutMlxFailureStage::Validate
        ),
        log = error,
        message = "handle_mlxreport_action dev_pci_name empty"
    )]
    Reconciliation {},
}

/// An MLX device read failed. Each variant names the operation and the stage
/// it broke at, and holds the device it was reading.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_device_operation_failed",
    metric_family = ScoutMlxFailures
)]
pub(super) enum ScoutMlxDeviceOperationFailed {
    #[event(
        labels(
            operation = ScoutMlxOperation::LockdownStatus,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] lockdown status check failed"
    )]
    LockdownStatusExecute {
        #[context]
        device_id: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::LockdownLock,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] lockdown lock failed"
    )]
    LockdownLockExecute {
        #[context]
        device_id: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::LockdownUnlock,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] lockdown unlock failed"
    )]
    LockdownUnlockExecute {
        #[context]
        device_id: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::DeviceInfo,
            failure_stage = ScoutMlxFailureStage::Discover
        ),
        log = error,
        message = "[scout_stream::mlx_device] device info request failed"
    )]
    DeviceInfoDiscover {
        #[context]
        device_id: String,
        #[context]
        error: String,
    },
}

/// A profile operation failed against one device. Each variant names the
/// operation and holds the device and profile it was working on.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_profile_operation_failed",
    metric_family = ScoutMlxFailures
)]
pub(super) enum ScoutMlxProfileOperationFailed {
    #[event(
        labels(
            operation = ScoutMlxOperation::ProfileCompare,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] profile compare against device failed"
    )]
    Compare {
        #[context]
        device_id: String,
        #[context]
        profile_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ProfileSync,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] profile sync to device failed"
    )]
    Sync {
        #[context]
        device_id: String,
        #[context]
        profile_name: String,
        #[context]
        error: String,
    },
}

/// A registry-show request named a registry Scout does not know.
///
/// Registry show preserves its existing error severity. Config query and
/// comparison use the warning-level `Event` below for the same lookup failure.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_registry_lookup_failed",
    metric_family = ScoutMlxFailures,
    log = error,
    message = "[scout_stream::mlx_device] variable registry not found"
)]
pub(super) struct ScoutMlxRegistryLookupFailed {
    #[label]
    operation: ScoutMlxOperation,
    #[label]
    failure_stage: ScoutMlxFailureStage,
    #[context]
    registry_name: String,
}

impl ScoutMlxRegistryLookupFailed {
    pub(super) fn registry_show_lookup(registry_name: String) -> Self {
        let failure_stage = ScoutMlxFailureStage::Lookup;
        Self {
            operation: ScoutMlxOperation::RegistryShow,
            failure_stage,
            registry_name,
        }
    }
}

/// A config read named a registry Scout does not know. Each variant is the
/// config operation that could not resolve it.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_config_registry_lookup_failed",
    metric_family = ScoutMlxFailures
)]
pub(super) enum ScoutMlxConfigRegistryLookupFailed {
    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigQuery,
            failure_stage = ScoutMlxFailureStage::Lookup
        ),
        log = warn,
        message = "[scout_stream::mlx_device] config registry not found"
    )]
    Query {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigCompare,
            failure_stage = ScoutMlxFailureStage::Lookup
        ),
        log = warn,
        message = "[scout_stream::mlx_device] config registry not found"
    )]
    Compare {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigSet,
            failure_stage = ScoutMlxFailureStage::Lookup
        ),
        log = warn,
        message = "[scout_stream::mlx_device] config registry not found"
    )]
    Set {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigSync,
            failure_stage = ScoutMlxFailureStage::Lookup
        ),
        log = warn,
        message = "[scout_stream::mlx_device] config registry not found"
    )]
    Sync {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
    },
}

/// An MLX configuration operation failed after registry lookup. Each variant
/// names the operation and the stage it broke at.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_config_operation_failed",
    metric_family = ScoutMlxFailures
)]
pub(super) enum ScoutMlxConfigOperationFailed {
    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigQuery,
            failure_stage = ScoutMlxFailureStage::Serialize
        ),
        log = error,
        message = "[scout_stream::mlx_device] config query result failed to serialize"
    )]
    QuerySerialize {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigQuery,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] config query against device failed"
    )]
    QueryExecute {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigCompare,
            failure_stage = ScoutMlxFailureStage::Serialize
        ),
        log = error,
        message = "[scout_stream::mlx_device] config compare result failed to serialize"
    )]
    CompareSerialize {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigCompare,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] config compare against device failed"
    )]
    CompareExecute {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigSet,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] config set to device failed"
    )]
    SetExecute {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigSync,
            failure_stage = ScoutMlxFailureStage::Serialize
        ),
        log = error,
        message = "[scout_stream::mlx_device] config sync result failed to serialize"
    )]
    SyncSerialize {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ConfigSync,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = error,
        message = "[scout_stream::mlx_device] config sync to device failed"
    )]
    SyncExecute {
        #[context]
        device_id: String,
        #[context]
        registry_name: String,
        #[context]
        error: String,
    },
}

/// Resetting one device's MLX configuration to its factory defaults failed.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_profile_reset_failed",
    metric_family = ScoutMlxFailures,
    log = error,
    message = "mlxconfig reset failed"
)]
pub(super) struct ScoutMlxProfileResetFailed {
    #[label]
    operation: ScoutMlxOperation,
    #[label]
    failure_stage: ScoutMlxFailureStage,
    #[context]
    device: String,
    #[context]
    error: String,
}

impl ScoutMlxProfileResetFailed {
    pub(super) fn execute(device: String, error: String) -> Self {
        let failure_stage = ScoutMlxFailureStage::Execute;
        Self {
            operation: ScoutMlxOperation::ProfileReset,
            failure_stage,
            device,
            error,
        }
    }
}

/// Applying one tenant profile after its device reset failed.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_profile_apply_failed",
    metric_family = ScoutMlxFailures,
    log = error,
    message = "mlxconfig profile sync failed"
)]
pub(super) struct ScoutMlxProfileApplyFailed {
    #[label]
    operation: ScoutMlxOperation,
    #[label]
    failure_stage: ScoutMlxFailureStage,
    #[context]
    device: String,
    #[context]
    profile: String,
    #[context]
    error: String,
}

impl ScoutMlxProfileApplyFailed {
    pub(super) fn execute(device: String, profile: String, error: String) -> Self {
        let failure_stage = ScoutMlxFailureStage::Execute;
        Self {
            operation: ScoutMlxOperation::ProfileApply,
            failure_stage,
            device,
            profile,
            error,
        }
    }
}

/// Scout could not initialize a firmware flasher for one MLX device.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_firmware_flasher_initialization_failed",
    metric_family = ScoutMlxFailures,
    log = error,
    message = "failed to create FirmwareFlasher"
)]
pub(super) struct ScoutMlxFirmwareFlasherInitializationFailed {
    #[label]
    operation: ScoutMlxOperation,
    #[label]
    failure_stage: ScoutMlxFailureStage,
    #[context]
    device: String,
    #[context]
    part_number: String,
    #[context]
    psid: String,
    #[context]
    error: String,
}

impl ScoutMlxFirmwareFlasherInitializationFailed {
    pub(super) fn new(device: String, part_number: String, psid: String, error: String) -> Self {
        let failure_stage = ScoutMlxFailureStage::Initialize;
        Self {
            operation: ScoutMlxOperation::FirmwareFlash,
            failure_stage,
            device,
            part_number,
            psid,
            error,
        }
    }
}

/// Scout initialized a firmware flasher, but applying its profile failed.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_firmware_flash_failed",
    metric_family = ScoutMlxFailures,
    log = error,
    message = "firmware flash failed"
)]
pub(super) struct ScoutMlxFirmwareFlashFailed {
    #[label]
    operation: ScoutMlxOperation,
    #[label]
    failure_stage: ScoutMlxFailureStage,
    #[context]
    device: String,
    #[context]
    part_number: String,
    #[context]
    psid: String,
    #[context]
    firmware_url: String,
    #[context]
    target_version: String,
    #[context]
    error: String,
}

impl ScoutMlxFirmwareFlashFailed {
    pub(super) fn execute(
        device: String,
        part_number: String,
        psid: String,
        firmware_url: String,
        target_version: String,
        error: String,
    ) -> Self {
        let failure_stage = ScoutMlxFailureStage::Execute;
        Self {
            operation: ScoutMlxOperation::FirmwareFlash,
            failure_stage,
            device,
            part_number,
            psid,
            firmware_url,
            target_version,
            error,
        }
    }
}

/// One MLX reconciliation step failed. Each variant is the step, and holds
/// the device it was reconciling.
#[derive(Event)]
#[event(
    event_name = "scout_mlx_reconciliation_failed",
    metric_family = ScoutMlxFailures
)]
pub(super) enum ScoutMlxReconciliationFailed {
    #[event(
        labels(
            operation = ScoutMlxOperation::Reconciliation,
            failure_stage = ScoutMlxFailureStage::Decode
        ),
        log = error,
        message = "handle_mlxreport_action error decoding command"
    )]
    Decode {
        #[context]
        pci_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::Reconciliation,
            failure_stage = ScoutMlxFailureStage::Discover
        ),
        log = error,
        message = "handle_mlxreport_action error from discover_device::from_str"
    )]
    Discover {
        #[context]
        pci_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ReconciliationLock,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = info,
        message = "handle_mlxreport_action error from lock_device"
    )]
    Lock {
        #[context]
        pci_name: String,
        #[context]
        error: String,
    },

    #[event(
        labels(
            operation = ScoutMlxOperation::ReconciliationUnlock,
            failure_stage = ScoutMlxFailureStage::Execute
        ),
        log = info,
        message = "handle_mlxreport_action error from unlock_device"
    )]
    Unlock {
        #[context]
        pci_name: String,
        #[context]
        error: String,
    },
}

/// Which storage cleanup path scout ran for a device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, LabelValue)]
pub(super) enum StorageDeviceType {
    Nvme,
    HddSas,
}

/// One storage-device cleanup pass. Every attempt records its duration.
#[derive(Event)]
#[event(
    event_name = "scout_storage_device_cleanup",
    metric_name = "carbide_scout_storage_device_cleanup_duration_seconds",
    component = "nico-scout",
    metric = histogram,
    describe = "Duration of per-device scout storage cleanup operations, by device type and outcome.",
    labels(outcome: Outcome, device_type: StorageDeviceType),
)]
pub(super) enum ScoutStorageDeviceCleanup {
    #[event(
        labels(outcome = Outcome::Ok),
        log = info,
        message = "Cleanup completed successfully"
    )]
    Ok {
        #[observation]
        took: Duration,
        #[label]
        device_type: StorageDeviceType,
        #[context]
        duration: String,
    },

    #[event(
        labels(outcome = Outcome::Error),
        log = error,
        message = "Cleanup failed"
    )]
    Error {
        #[observation]
        took: Duration,
        #[label]
        device_type: StorageDeviceType,
        #[context]
        duration: String,
        #[context]
        error: String,
    },
}

impl ScoutStorageDeviceCleanup {
    /// Which case a cleanup landed in. A success has no error to report, so the
    /// failure text exists only on `Error`.
    pub(super) fn from_result<E>(
        device_type: StorageDeviceType,
        duration: Duration,
        result: &Result<(), E>,
    ) -> Self
    where
        E: std::fmt::Display,
    {
        let took = duration;
        let duration = format!("{duration:?}");
        match result {
            Result::Ok(()) => Self::Ok {
                device_type,
                took,
                duration,
            },
            Result::Err(error) => Self::Error {
                device_type,
                took,
                duration,
                error: error.to_string(),
            },
        }
    }
}
#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use carbide_instrument::emit;
    use carbide_instrument::testing::{
        ApproxHistogramSum, CapturedFieldKind, MetricsCapture, capture_logs,
    };
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn firmware_download_attempt_failures_share_one_bounded_metric() {
        struct AttemptCase {
            kind: FirmwareDownloadKind,
            kind_label: &'static str,
            next_action: FirmwareDownloadNextAction,
            next_action_label: &'static str,
            retry_delay_seconds: f64,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            counter_delta: f64,
            level: tracing::Level,
            message: String,
            kind: Option<String>,
            next_action: Option<String>,
            attempt: Option<String>,
            url: Option<String>,
            error: Option<String>,
            retry_delay_seconds: Option<String>,
        }

        check_values(
            [
                Check {
                    scenario: "script attempt will retry",
                    input: AttemptCase {
                        kind: FirmwareDownloadKind::Script,
                        kind_label: "script",
                        next_action: FirmwareDownloadNextAction::Retry,
                        next_action_label: "retry",
                        retry_delay_seconds: 4.0,
                    },
                    expect: Observation {
                        counter_delta: 1.0,
                        level: tracing::Level::WARN,
                        message: "[firmware_upgrade] download attempt failed; retrying".to_string(),
                        kind: Some("script".to_string()),
                        next_action: Some("retry".to_string()),
                        attempt: Some("2".to_string()),
                        url: Some("https://firmware.example/script.sh".to_string()),
                        error: Some("HTTP 503".to_string()),
                        retry_delay_seconds: Some("4".to_string()),
                    },
                },
                Check {
                    scenario: "artifact attempt will give up",
                    input: AttemptCase {
                        kind: FirmwareDownloadKind::Artifact,
                        kind_label: "artifact",
                        next_action: FirmwareDownloadNextAction::GiveUp,
                        next_action_label: "give_up",
                        retry_delay_seconds: 0.0,
                    },
                    expect: Observation {
                        counter_delta: 1.0,
                        level: tracing::Level::WARN,
                        message: "[firmware_upgrade] download attempt failed; retrying".to_string(),
                        kind: Some("artifact".to_string()),
                        next_action: Some("give_up".to_string()),
                        attempt: Some("2".to_string()),
                        url: Some("https://firmware.example/image.bin".to_string()),
                        error: Some("HTTP 503".to_string()),
                        retry_delay_seconds: Some("0".to_string()),
                    },
                },
            ],
            |case| {
                let metrics = MetricsCapture::start();
                let url = match case.kind {
                    FirmwareDownloadKind::Script => "https://firmware.example/script.sh",
                    FirmwareDownloadKind::Artifact => "https://firmware.example/image.bin",
                };
                let logs = capture_logs(|| {
                    emit(ScoutFirmwareDownloadAttemptFailed {
                        kind: case.kind,
                        next_action: case.next_action,
                        attempt: 2,
                        url: url.to_string(),
                        error: "HTTP 503".to_string(),
                        retry_delay_seconds: case.retry_delay_seconds,
                    });
                });
                let log = &logs[0];

                Observation {
                    counter_delta: metrics.counter_delta(
                        "carbide_scout_firmware_download_attempt_failures_total",
                        &[
                            ("kind", case.kind_label),
                            ("next_action", case.next_action_label),
                        ],
                    ),
                    level: log.level,
                    message: log.message.clone(),
                    kind: log.field("kind").map(str::to_string),
                    next_action: log.field("next_action").map(str::to_string),
                    attempt: log.field("attempt").map(str::to_string),
                    url: log.field("url").map(str::to_string),
                    error: log.field("error").map(str::to_string),
                    retry_delay_seconds: log.field("retry_delay_seconds").map(str::to_string),
                }
            },
        );
    }

    #[test]
    fn mlx_request_rejections_count_with_existing_diagnostics() {
        #[derive(Clone, Copy)]
        enum RequestCase {
            ProfileCompare,
            ProfileSync,
            Reconciliation,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            counter_delta: f64,
            logs: Vec<(tracing::Level, String)>,
        }

        check_values(
            [
                Check {
                    scenario: "missing profile data remains metric-only",
                    input: RequestCase::ProfileCompare,
                    expect: Observation {
                        counter_delta: 1.0,
                        logs: Vec::new(),
                    },
                },
                Check {
                    scenario: "missing profile sync data remains metric-only",
                    input: RequestCase::ProfileSync,
                    expect: Observation {
                        counter_delta: 1.0,
                        logs: Vec::new(),
                    },
                },
                Check {
                    scenario: "empty reconciliation PCI retains its error",
                    input: RequestCase::Reconciliation,
                    expect: Observation {
                        counter_delta: 1.0,
                        logs: vec![(
                            tracing::Level::ERROR,
                            "handle_mlxreport_action dev_pci_name empty".to_string(),
                        )],
                    },
                },
            ],
            |case| {
                let (event, operation) = match case {
                    RequestCase::ProfileCompare => (
                        ScoutMlxRequestRejected::ProfileCompare {},
                        "profile_compare",
                    ),
                    RequestCase::ProfileSync => {
                        (ScoutMlxRequestRejected::ProfileSync {}, "profile_sync")
                    }
                    RequestCase::Reconciliation => {
                        (ScoutMlxRequestRejected::Reconciliation {}, "reconciliation")
                    }
                };
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| emit(event));

                Observation {
                    counter_delta: metrics.counter_delta(
                        "carbide_scout_mlx_failures_total",
                        &[("operation", operation), ("failure_stage", "validate")],
                    ),
                    logs: logs
                        .into_iter()
                        .map(|log| (log.level, log.message))
                        .collect(),
                }
            },
        );
    }

    #[test]
    fn mlx_mutation_events_keep_context_out_of_metric_labels() {
        #[derive(Clone, Copy)]
        enum MutationCase {
            ProfileReset,
            ProfileApply,
            FirmwareInitialize,
            FirmwareExecute,
        }

        struct Expected {
            operation: &'static str,
            failure_stage: &'static str,
            event_name: &'static str,
            message: &'static str,
            context: Vec<(&'static str, &'static str)>,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            counter_delta: f64,
            level: tracing::Level,
            event_name: Option<String>,
            message: String,
            context: Vec<(String, String, Option<CapturedFieldKind>)>,
        }

        let expected = |case| match case {
            MutationCase::ProfileReset => Expected {
                operation: "profile_reset",
                failure_stage: "execute",
                event_name: "scout_mlx_profile_reset_failed",
                message: "mlxconfig reset failed",
                context: vec![("device", "device"), ("error", "failure")],
            },
            MutationCase::ProfileApply => Expected {
                operation: "profile_apply",
                failure_stage: "execute",
                event_name: "scout_mlx_profile_apply_failed",
                message: "mlxconfig profile sync failed",
                context: vec![
                    ("device", "device"),
                    ("profile", "profile"),
                    ("error", "failure"),
                ],
            },
            MutationCase::FirmwareInitialize => Expected {
                operation: "firmware_flash",
                failure_stage: "initialize",
                event_name: "scout_mlx_firmware_flasher_initialization_failed",
                message: "failed to create FirmwareFlasher",
                context: vec![
                    ("device", "device"),
                    ("part_number", "part-number"),
                    ("psid", "psid"),
                    ("error", "failure"),
                ],
            },
            MutationCase::FirmwareExecute => Expected {
                operation: "firmware_flash",
                failure_stage: "execute",
                event_name: "scout_mlx_firmware_flash_failed",
                message: "firmware flash failed",
                context: vec![
                    ("device", "device"),
                    ("part_number", "part-number"),
                    ("psid", "psid"),
                    ("firmware_url", "https://firmware.example/fw.bin"),
                    ("target_version", "1.2.3"),
                    ("error", "failure"),
                ],
            },
        };
        let expected_observation = |case| {
            let expected = expected(case);
            Observation {
                counter_delta: 1.0,
                level: tracing::Level::ERROR,
                event_name: Some(expected.event_name.to_string()),
                message: expected.message.to_string(),
                context: expected
                    .context
                    .into_iter()
                    .map(|(name, value)| {
                        (
                            name.to_string(),
                            value.to_string(),
                            Some(CapturedFieldKind::Debug),
                        )
                    })
                    .collect(),
            }
        };

        check_values(
            [
                Check {
                    scenario: "profile reset failure",
                    input: MutationCase::ProfileReset,
                    expect: expected_observation(MutationCase::ProfileReset),
                },
                Check {
                    scenario: "profile apply failure",
                    input: MutationCase::ProfileApply,
                    expect: expected_observation(MutationCase::ProfileApply),
                },
                Check {
                    scenario: "firmware flasher initialization failure",
                    input: MutationCase::FirmwareInitialize,
                    expect: expected_observation(MutationCase::FirmwareInitialize),
                },
                Check {
                    scenario: "firmware flash execution failure",
                    input: MutationCase::FirmwareExecute,
                    expect: expected_observation(MutationCase::FirmwareExecute),
                },
            ],
            |case| {
                let expected = expected(case);
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| match case {
                    MutationCase::ProfileReset => emit(ScoutMlxProfileResetFailed::execute(
                        "device".to_string(),
                        "failure".to_string(),
                    )),
                    MutationCase::ProfileApply => emit(ScoutMlxProfileApplyFailed::execute(
                        "device".to_string(),
                        "profile".to_string(),
                        "failure".to_string(),
                    )),
                    MutationCase::FirmwareInitialize => {
                        emit(ScoutMlxFirmwareFlasherInitializationFailed::new(
                            "device".to_string(),
                            "part-number".to_string(),
                            "psid".to_string(),
                            "failure".to_string(),
                        ))
                    }
                    MutationCase::FirmwareExecute => emit(ScoutMlxFirmwareFlashFailed::execute(
                        "device".to_string(),
                        "part-number".to_string(),
                        "psid".to_string(),
                        "https://firmware.example/fw.bin".to_string(),
                        "1.2.3".to_string(),
                        "failure".to_string(),
                    )),
                });
                let log = &logs[0];

                Observation {
                    counter_delta: metrics.counter_delta(
                        "carbide_scout_mlx_failures_total",
                        &[
                            ("operation", expected.operation),
                            ("failure_stage", expected.failure_stage),
                        ],
                    ),
                    level: log.level,
                    event_name: log.field("event_name").map(str::to_string),
                    message: log.message.clone(),
                    context: expected
                        .context
                        .iter()
                        .map(|(name, _)| {
                            (
                                (*name).to_string(),
                                log.field(name).unwrap().to_string(),
                                log.field_kind(name),
                            )
                        })
                        .collect(),
                }
            },
        );
    }

    #[test]
    fn scout_action_maps_every_dispatchable_action() {
        check_values(
            [
                Check {
                    scenario: "noop",
                    input: fac::Action::Noop(fac::Noop {}),
                    expect: ScoutAction::Noop,
                },
                Check {
                    scenario: "reset",
                    input: fac::Action::Reset(fac::Reset {}),
                    expect: ScoutAction::Reset,
                },
                Check {
                    scenario: "discovery",
                    input: fac::Action::Discovery(fac::Discovery {}),
                    expect: ScoutAction::Discovery,
                },
                Check {
                    scenario: "rebuild",
                    input: fac::Action::Rebuild(fac::Rebuild {}),
                    expect: ScoutAction::Rebuild,
                },
                Check {
                    scenario: "retry",
                    input: fac::Action::Retry(fac::Retry {}),
                    expect: ScoutAction::Retry,
                },
                Check {
                    scenario: "measure",
                    input: fac::Action::Measure(fac::Measure {}),
                    expect: ScoutAction::Measure,
                },
                Check {
                    scenario: "log error",
                    input: fac::Action::LogError(fac::LogError {}),
                    expect: ScoutAction::LogError,
                },
                Check {
                    scenario: "machine validation",
                    input: fac::Action::MachineValidation(fac::MachineValidation::default()),
                    expect: ScoutAction::MachineValidation,
                },
                Check {
                    scenario: "mlx action",
                    input: fac::Action::MlxAction(fac::MlxAction::default()),
                    expect: ScoutAction::MlxAction,
                },
                Check {
                    scenario: "firmware upgrade",
                    input: fac::Action::FirmwareUpgrade(fac::FirmwareUpgrade::default()),
                    expect: ScoutAction::FirmwareUpgrade,
                },
            ],
            |action| ScoutAction::from(&action),
        );
    }

    #[test]
    fn scout_stream_connection_counter_moves_per_outcome() {
        struct ConnectionCase {
            outcome: Outcome,
            outcome_label: &'static str,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            log_count: usize,
            counter_delta: f64,
        }

        check_values(
            [
                Check {
                    scenario: "stream connected",
                    input: ConnectionCase {
                        outcome: Outcome::Ok,
                        outcome_label: "ok",
                    },
                    expect: Observation {
                        log_count: 0,
                        counter_delta: 1.0,
                    },
                },
                Check {
                    scenario: "stream connection failed",
                    input: ConnectionCase {
                        outcome: Outcome::Error,
                        outcome_label: "error",
                    },
                    expect: Observation {
                        log_count: 0,
                        counter_delta: 1.0,
                    },
                },
            ],
            |ConnectionCase {
                 outcome,
                 outcome_label,
             }| {
                let metrics = MetricsCapture::start();
                let logs = capture_logs(|| emit(ScoutStreamConnection { outcome }));
                Observation {
                    log_count: logs.len(),
                    counter_delta: metrics.counter_delta(
                        "carbide_scout_stream_connections_total",
                        &[("outcome", outcome_label)],
                    ),
                }
            },
        );
    }

    #[test]
    fn scout_stream_reconnect_logs_and_counts() {
        let metrics = MetricsCapture::start();
        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")
                .expect("valid machine id");
        let logs = capture_logs(|| {
            emit(ScoutStreamReconnect {
                api_endpoint: "https://[::1]:1079".to_string(),
                machine_id,
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].metadata_name, "scout_stream_reconnect");
        assert_eq!(logs[0].level, tracing::Level::WARN);
        assert_eq!(logs[0].message, "scout stream reconnecting after 10s delay");
        assert_eq!(logs[0].field("event_name"), Some("scout_stream_reconnect"));
        assert_eq!(
            logs[0].field("metric_name"),
            Some("carbide_scout_stream_reconnects_total")
        );
        assert_eq!(logs[0].field("api_endpoint"), Some("https://[::1]:1079"));
        let machine_id = machine_id.to_string();
        assert_eq!(logs[0].field("machine_id"), Some(machine_id.as_str()));

        assert_eq!(
            metrics.counter_delta("carbide_scout_stream_reconnects_total", &[]),
            1.0
        );
    }

    #[test]
    fn scout_stream_response_drop_logs_and_counts() {
        let metrics = MetricsCapture::start();
        let machine_id =
            MachineId::from_str("fm100htes3rn1npvbtm5qd57dkilaag7ljugl1llmm7rfuq1ov50i0rpl30")
                .expect("valid machine id");
        let logs = capture_logs(|| {
            emit(ScoutStreamResponseDropped {
                api_endpoint: "https://[::1]:1079".to_string(),
                machine_id,
                error: "request stream closed".to_string(),
            });
        });

        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].metadata_name, "scout_stream_response_dropped");
        assert_eq!(logs[0].level, tracing::Level::ERROR);
        assert_eq!(logs[0].message, "scout stream failed to send response");
        assert_eq!(
            logs[0].field("event_name"),
            Some("scout_stream_response_dropped")
        );
        assert_eq!(
            logs[0].field("metric_name"),
            Some("carbide_scout_stream_responses_dropped_total")
        );
        assert_eq!(logs[0].field("api_endpoint"), Some("https://[::1]:1079"));
        let machine_id = machine_id.to_string();
        assert_eq!(logs[0].field("machine_id"), Some(machine_id.as_str()));
        assert_eq!(logs[0].field("error"), Some("request stream closed"));

        assert_eq!(
            metrics.counter_delta("carbide_scout_stream_responses_dropped_total", &[]),
            1.0
        );
    }

    #[test]
    fn storage_device_cleanup_logs_and_records_duration() {
        const METRIC_NAME: &str = "carbide_scout_storage_device_cleanup_duration_seconds";

        enum CleanupCase {
            Succeeded {
                device_type: StorageDeviceType,
                duration: Duration,
            },
            Failed {
                device_type: StorageDeviceType,
                duration: Duration,
                error: &'static str,
            },
        }

        #[derive(Debug, PartialEq)]
        struct LogObservation {
            metadata_name: String,
            level: tracing::Level,
            message: String,
            event_name: Option<String>,
            metric_name: Option<String>,
            device_type: Option<String>,
            outcome: Option<String>,
            duration: Option<String>,
            duration_kind: Option<CapturedFieldKind>,
            error: Option<String>,
            error_kind: Option<CapturedFieldKind>,
        }

        #[derive(Debug, PartialEq)]
        struct Observation {
            log_count: usize,
            log: Option<LogObservation>,
            histogram_count_delta: u64,
            histogram_sum_delta: ApproxHistogramSum,
        }

        fn observe(case: CleanupCase) -> Observation {
            let metrics = MetricsCapture::start();
            let (device_type, duration, result) = match case {
                CleanupCase::Succeeded {
                    device_type,
                    duration,
                } => (device_type, duration, Ok(())),
                CleanupCase::Failed {
                    device_type,
                    duration,
                    error,
                } => (device_type, duration, Err(error)),
            };
            let device_type_label = device_type.label_value().to_string();
            let outcome_label = match result {
                Ok(()) => "ok",
                Err(_) => "error",
            };
            let labels = [
                ("device_type", device_type_label.as_str()),
                ("outcome", outcome_label),
            ];
            let logs = capture_logs(|| {
                emit(ScoutStorageDeviceCleanup::from_result(
                    device_type,
                    duration,
                    &result,
                ));
            });
            let log = logs.first().map(|log| LogObservation {
                metadata_name: log.metadata_name.clone(),
                level: log.level,
                message: log.message.clone(),
                event_name: log.field("event_name").map(str::to_string),
                metric_name: log.field("metric_name").map(str::to_string),
                device_type: log.field("device_type").map(str::to_string),
                outcome: log.field("outcome").map(str::to_string),
                duration: log.field("duration").map(str::to_string),
                duration_kind: log.field_kind("duration"),
                error: log.field("error").map(str::to_string),
                error_kind: log.field_kind("error"),
            });

            Observation {
                log_count: logs.len(),
                log,
                histogram_count_delta: metrics.histogram_count_delta(METRIC_NAME, &labels),
                histogram_sum_delta: metrics.histogram_sum_delta(METRIC_NAME, &labels),
            }
        }

        check_values(
            [
                Check {
                    scenario: "NVMe cleanup succeeded",
                    input: CleanupCase::Succeeded {
                        device_type: StorageDeviceType::Nvme,
                        duration: Duration::from_millis(250),
                    },
                    expect: Observation {
                        log_count: 1,
                        log: Some(LogObservation {
                            metadata_name: "scout_storage_device_cleanup".to_string(),
                            level: tracing::Level::INFO,
                            message: "Cleanup completed successfully".to_string(),
                            event_name: Some("scout_storage_device_cleanup".to_string()),
                            metric_name: Some(METRIC_NAME.to_string()),
                            device_type: Some("nvme".to_string()),
                            outcome: Some("ok".to_string()),
                            duration: Some("250ms".to_string()),
                            duration_kind: Some(CapturedFieldKind::Debug),
                            error: None,
                            error_kind: None,
                        }),
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(0.25),
                    },
                },
                Check {
                    scenario: "NVMe cleanup failed",
                    input: CleanupCase::Failed {
                        device_type: StorageDeviceType::Nvme,
                        duration: Duration::from_millis(500),
                        error: "NVMe sanitize command failed",
                    },
                    expect: Observation {
                        log_count: 1,
                        log: Some(LogObservation {
                            metadata_name: "scout_storage_device_cleanup".to_string(),
                            level: tracing::Level::ERROR,
                            message: "Cleanup failed".to_string(),
                            event_name: Some("scout_storage_device_cleanup".to_string()),
                            metric_name: Some(METRIC_NAME.to_string()),
                            device_type: Some("nvme".to_string()),
                            outcome: Some("error".to_string()),
                            duration: Some("500ms".to_string()),
                            duration_kind: Some(CapturedFieldKind::Debug),
                            error: Some("NVMe sanitize command failed".to_string()),
                            error_kind: Some(CapturedFieldKind::Debug),
                        }),
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(0.5),
                    },
                },
                Check {
                    scenario: "HDD/SAS cleanup succeeded",
                    input: CleanupCase::Succeeded {
                        device_type: StorageDeviceType::HddSas,
                        duration: Duration::from_millis(750),
                    },
                    expect: Observation {
                        log_count: 1,
                        log: Some(LogObservation {
                            metadata_name: "scout_storage_device_cleanup".to_string(),
                            level: tracing::Level::INFO,
                            message: "Cleanup completed successfully".to_string(),
                            event_name: Some("scout_storage_device_cleanup".to_string()),
                            metric_name: Some(METRIC_NAME.to_string()),
                            device_type: Some("hdd_sas".to_string()),
                            outcome: Some("ok".to_string()),
                            duration: Some("750ms".to_string()),
                            duration_kind: Some(CapturedFieldKind::Debug),
                            error: None,
                            error_kind: None,
                        }),
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(0.75),
                    },
                },
                Check {
                    scenario: "HDD/SAS cleanup failed",
                    input: CleanupCase::Failed {
                        device_type: StorageDeviceType::HddSas,
                        duration: Duration::from_secs(1),
                        error: "HDD security erase failed",
                    },
                    expect: Observation {
                        log_count: 1,
                        log: Some(LogObservation {
                            metadata_name: "scout_storage_device_cleanup".to_string(),
                            level: tracing::Level::ERROR,
                            message: "Cleanup failed".to_string(),
                            event_name: Some("scout_storage_device_cleanup".to_string()),
                            metric_name: Some(METRIC_NAME.to_string()),
                            device_type: Some("hdd_sas".to_string()),
                            outcome: Some("error".to_string()),
                            duration: Some("1s".to_string()),
                            duration_kind: Some(CapturedFieldKind::Debug),
                            error: Some("HDD security erase failed".to_string()),
                            error_kind: Some(CapturedFieldKind::Debug),
                        }),
                        histogram_count_delta: 1,
                        histogram_sum_delta: ApproxHistogramSum(1.0),
                    },
                },
            ],
            observe,
        );
    }
}
