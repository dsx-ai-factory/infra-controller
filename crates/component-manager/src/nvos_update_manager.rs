// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Backend-neutral contracts for durable rack-level NVOS updates.

use std::net::IpAddr;

use carbide_secrets::credentials::Credentials;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use model::rack::{FirmwareUpgradeDeviceInfo, NvosUpdateJob};
use model::rack_type::RackProfile;

use crate::error::ComponentManagerError;
use crate::nv_switch_manager::SwitchPasswordRotationState;

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Input for one rack-level NVOS system-image update.
pub struct NvosUpdateRequest<'a> {
    /// Rack that owns every target switch.
    pub rack_id: &'a RackId,

    /// Rack profile used by the backend to identify target hardware.
    pub profile: &'a RackProfile,

    /// Source-of-truth NVOS firmware-object JSON.
    pub config_json: &'a str,

    /// Artifact authorization token or the RMS `NOAUTH` sentinel.
    pub access_token: &'a str,

    /// NV-Switch targets and their credentials.
    pub switches: Vec<FirmwareUpgradeDeviceInfo>,
}

/// Backend contract for submitting and polling rack-level NVOS updates.
///
/// Implementations return every parent or per-switch job handle needed to
/// resume polling after a process restart.
#[async_trait::async_trait]
pub trait NvosUpdateManager: sealed::Sealed + Send + Sync {
    /// Submits an NVOS system-image update and returns the initial status and
    /// durable job handles for every requested switch.
    ///
    /// # Errors
    ///
    /// Returns [`ComponentManagerError::InvalidArgument`] when the request cannot
    /// be translated for the backend or the backend rejects it as invalid before
    /// accepting work. An explicit backend rejection without a durable job handle
    /// returns [`ComponentManagerError::RejectedBeforeDispatch`]. Other submission
    /// failures return [`ComponentManagerError::Internal`].
    async fn start_nvos_update(
        &self,
        request: NvosUpdateRequest<'_>,
    ) -> Result<NvosUpdateJob, ComponentManagerError>;

    /// Polls a submitted NVOS update and returns its current per-switch and
    /// aggregate status while preserving the durable job handles. Retryable
    /// per-switch lookup failures remain in the returned job so a later poll
    /// can retry them without losing progress from other switches.
    ///
    /// # Errors
    ///
    /// Returns a backend error when the rack-level status request cannot be
    /// completed.
    async fn get_nvos_update_status(
        &self,
        job: &NvosUpdateJob,
    ) -> Result<NvosUpdateJob, ComponentManagerError>;

    /// Starts recovery of a switch's desired NVOS password and returns its
    /// backend job ID.
    ///
    /// Retrying is safe only with unchanged `credentials`. RMS verifies the
    /// desired password before trying recovery credentials.
    ///
    /// # Errors
    ///
    /// Returns [`ComponentManagerError::InvalidArgument`] when RMS cannot map
    /// the profile to a switch identity,
    /// [`ComponentManagerError::RejectedBeforeDispatch`] for empty credentials
    /// or rejection before dispatch, [`ComponentManagerError::Unsupported`] for
    /// an unsupported operation, and
    /// [`ComponentManagerError::OperationOutcomeUnknown`] when submission does
    /// not return a durable job ID.
    async fn start_nvos_password_update(
        &self,
        rack_id: &RackId,
        profile: &RackProfile,
        switch_id: &SwitchId,
        nvos_ip: IpAddr,
        credentials: &Credentials,
    ) -> Result<String, ComponentManagerError>;

    /// Returns the latest observation of an NVOS password-recovery job.
    ///
    /// [`SwitchPasswordRotationState::NotFound`] means the backend cannot
    /// resolve the job ID; it does not establish whether the password changed.
    ///
    /// # Errors
    ///
    /// Returns [`ComponentManagerError::InvalidArgument`] for an empty or
    /// rejected job ID, [`ComponentManagerError::Unsupported`] when RMS lacks
    /// status polling, [`ComponentManagerError::Unavailable`] for retryable
    /// polling failures, and [`ComponentManagerError::Internal`] for other
    /// lookup failures.
    async fn get_nvos_password_update_status(
        &self,
        job_id: &str,
    ) -> Result<SwitchPasswordRotationState, ComponentManagerError>;
}
