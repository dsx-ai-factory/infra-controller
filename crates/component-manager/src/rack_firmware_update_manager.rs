// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Backend-neutral contracts for durable rack-level firmware-object updates.

use carbide_uuid::rack::RackId;
use model::rack::{FirmwareUpgradeDeviceInfo, FirmwareUpgradeJob};
use model::rack_type::RackProfile;

use crate::error::ComponentManagerError;

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Input for one rack-level firmware-object update.
pub struct RackFirmwareUpdateRequest<'a> {
    /// Rack that owns every target device.
    pub rack_id: &'a RackId,

    /// Rack profile used by the backend to identify target hardware.
    pub profile: &'a RackProfile,

    /// Source-of-truth firmware-object JSON.
    pub config_json: &'a str,

    /// Optional authorization token supplied with the maintenance request.
    pub access_token: Option<&'a str>,

    /// Whether the backend should apply matching firmware again.
    pub force_update: bool,

    /// Firmware-object component filters.
    pub components: &'a [String],

    /// Compute targets and their credentials.
    pub machines: Vec<FirmwareUpgradeDeviceInfo>,

    /// NV-Switch targets and their credentials.
    pub switches: Vec<FirmwareUpgradeDeviceInfo>,
}

/// Backend contract for durable rack-level firmware-object updates.
#[async_trait::async_trait]
pub trait RackFirmwareUpdateManager: sealed::Sealed + Send + Sync {
    /// Submits a firmware-object update and returns the durable job handles for
    /// every requested device.
    ///
    /// # Errors
    ///
    /// Returns an error when the request cannot be translated or submitted to
    /// the configured backend.
    async fn start_firmware_update(
        &self,
        request: RackFirmwareUpdateRequest<'_>,
    ) -> Result<FirmwareUpgradeJob, ComponentManagerError>;

    /// Polls a submitted firmware-object update using its durable job handles.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured backend cannot process the status
    /// request.
    async fn get_firmware_update_status(
        &self,
        job: &FirmwareUpgradeJob,
    ) -> Result<FirmwareUpgradeJob, ComponentManagerError>;
}
