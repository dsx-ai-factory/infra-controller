// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Passive observations of known firmware jobs and local staged work.
//! These reads do not submit updates, advance sequencing, or refresh tracking.

use mac_address::MacAddress;
use model::component_manager::FirmwareState;

/// `FirmwareJobSource` identifies a backend with passive job observations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareJobSource {
    /// Known jobs queried through Rack Management Service.
    Rms,
}

/// `FirmwareJobReadError` preserves missing or unavailable evidence without raw diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FirmwareJobReadError {
    /// Persisted tracking could not be read; this does not mean no jobs exist.
    #[error("firmware job tracking could not be read")]
    TrackingReadFailed,
    /// The backend reports that the known job no longer exists, not that it failed.
    #[error("firmware job was not found by the backend")]
    JobNotFound,
    /// An RPC or connection failed before a usable observation was obtained.
    #[error("firmware job poll failed")]
    PollFailed,
    /// The protocol is unimplemented or unconfigured, or its response has an
    /// unspecified status, missing/mismatched success ID, contradictory failure
    /// ID, or unrecognized state. None establishes a backend job's state.
    #[error("firmware job status is unavailable")]
    StatusUnavailable,
    /// The shared request deadline expired before this read completed.
    #[error("firmware job read deadline exceeded")]
    DeadlineExceeded,
}

/// `FirmwareJobKind` selects the protocol used to observe a tracked job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareJobKind {
    /// A firmware-object bundle update.
    FirmwareObject,
    /// An NVOS system-image update.
    SwitchSystemImage,
}

/// `FirmwareJobObservation` keeps one known job independent of its peers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareJobObservation {
    /// Protocol associated with the tracked ID.
    pub kind: FirmwareJobKind,
    /// Exact tracked backend ID; never replaced by a response's echoed ID.
    pub job_id: String,
    /// Recognized observed state (never `Unknown`) or unavailable status.
    /// Errors do not establish update failure.
    pub state: Result<FirmwareState, FirmwareJobReadError>,
}

/// `StagedFirmwareJob` describes a local firmware-object phase without its apply payload.
/// It exists only while the updater retains it in memory; absence after restart
/// does not establish that the phase completed or was never requested.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedFirmwareJob {
    /// The updater has not submitted this phase, even if its NVOS job has completed.
    Queued {
        /// NVOS job that must complete before the updater can submit this phase.
        system_image_job_id: String,
    },
    /// The updater began submission but has not finished recording its result.
    /// This marker does not prove whether RMS accepted the update and can
    /// remain if the submitting updater is cancelled.
    Submitting {
        /// NVOS job preceding the submission.
        system_image_job_id: String,
    },
    /// The updater abandoned this phase without recording a backend job.
    /// This does not prove RMS rejected the submission or performed no work.
    /// Its original diagnostic is excluded because it may contain apply payloads.
    Failed,
}

/// `ComponentFirmwareJobsResult` identifies requested management MAC and job evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentFirmwareJobsResult {
    /// Requested BMC MAC for compute/switches, or PMC MAC for a power shelf.
    pub mac_address: MacAddress,
    /// Recognized jobs in firmware-object then system-image order. Tracking has
    /// at most one ID per kind. An empty success means no supported tracked IDs
    /// were found, not that an update completed. Each job retains its own result.
    pub jobs: Result<Vec<FirmwareJobObservation>, FirmwareJobReadError>,
    /// Switch phase sampled before RPCs, independent of tracking/poll failures;
    /// the updater may change it before this response.
    /// Compute/shelf reads return `None`. Absence never proves update completion.
    pub staged: Option<StagedFirmwareJob>,
}
