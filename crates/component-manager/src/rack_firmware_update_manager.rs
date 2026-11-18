// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Rack-level firmware-object updates and passive manifest comparisons.

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

/// Input for one passive comparison against an already selected manifest.
///
/// The caller owns manifest retrieval, profile selection, rack membership and
/// current endpoint/credential validation. One call uses one rack, manifest and
/// profile. Device credentials are included; artifact tokens are not.
pub struct RackFirmwareExpectationsRequest<'a> {
    /// Rack that owns every supplied device.
    pub rack_id: &'a RackId,

    /// Effective profile for the shared RMS descriptor and filter helpers.
    pub profile: &'a RackProfile,

    /// Already retrieved firmware-object JSON; this operation does not fetch it.
    pub config_json: &'a str,

    /// Component selectors with the same rules as firmware apply. Empty selects
    /// RMS mappings with `default_apply`.
    pub components: &'a [String],

    /// Compute targets, in caller order, including repeated slots.
    pub machines: &'a [FirmwareUpgradeDeviceInfo],

    /// Switch targets, in caller order, including repeated slots.
    pub switches: &'a [FirmwareUpgradeDeviceInfo],

    /// Power-shelf targets, in caller order, including repeated slots.
    pub power_shelves: &'a [FirmwareUpgradeDeviceInfo],
}

/// Logical comparison evidence for one requested device slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceFirmwareExpectations {
    /// Caller-supplied RMS node ID, scoped to the request's rack.
    pub node_id: String,
    /// Rows in backend order, preserving duplicates and grouped comparisons.
    /// Empty success means RMS returned no rows, not that policy is absent or
    /// complete. Attributable failures affect only that device. Transport
    /// failures affect the batch; a deadline also leaves unsent batches unavailable.
    pub expectations: Result<Vec<FirmwareExpectation>, FirmwareExpectationError>,
}

/// A logical manifest comparison row, without attribution to physical instances.
///
/// Component/SKU/target evidence is not a physical inventory identifier. No
/// join with installed inventory, completeness or compliance follows from this row.
/// Empty protobuf strings become `None`; protobuf does not distinguish an
/// omitted string from an explicitly empty string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareExpectation {
    /// Opaque component selector reported by RMS, without normalization.
    pub component: Option<String>,
    /// Proposed manifest version, or `None` when RMS supplies no proposal.
    pub proposed_version: Option<String>,
    /// Reported SKU selector; absence does not identify a wildcard SKU.
    pub sku_id: Option<String>,
    /// Opaque reported mapping target. It need not apply to each physical
    /// instance represented by a grouped comparison.
    pub reported_target: Option<String>,
    /// RMS's reported comparison classification, including unrecognized wire values.
    pub status: FirmwareExpectationStatus,
}

/// Reported RMS classification for one logical row; none verifies physical
/// instances or establishes complete manifest compliance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirmwareExpectationStatus {
    /// RMS supplied no comparison classification.
    Unspecified,
    /// RMS reported a matching logical comparison.
    Matched,
    /// RMS reported a differing logical comparison.
    Mismatched,
    /// RMS reported missing comparison inventory.
    Missing,
    /// RMS could not verify the comparison, even if a proposal is present.
    Unverified,
    /// A wire classification this revision does not recognize.
    Unknown(i32),
}

/// Unavailable comparison evidence without backend diagnostic text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FirmwareExpectationError {
    /// Descriptor preparation, transport or attributable node evidence failed.
    /// This includes RMS rejecting input or failing to read the device; safe
    /// diagnostic categories distinguish causes in logs.
    #[error("firmware comparison read failed")]
    ReadFailed,
    /// The shared call deadline expired before this device completed.
    #[error("firmware comparison read deadline exceeded")]
    DeadlineExceeded,
    /// Response collections or retained metadata exceeded application limits.
    #[error("firmware comparison read limit exceeded")]
    LimitExceeded,
    /// The remote service explicitly rejected an attempted RPC as unimplemented.
    #[error("firmware comparison is unsupported by the remote service")]
    Unsupported,
}

/// Backend contract for firmware-object updates and passive comparisons.
#[async_trait::async_trait]
pub trait RackFirmwareUpdateManager: sealed::Sealed + Send + Sync {
    /// Reads logical manifest comparison evidence without registration,
    /// persistence, artifact download, update submission or activation.
    ///
    /// RMS uses the supplied credentials to read live device inventory before
    /// comparing it with the manifest. An unreachable device can therefore have
    /// unavailable expectations even though NICo's installed reader is separate.
    ///
    /// Results preserve machines, then switches, then power-shelf slot order,
    /// including repeats. Identical descriptors share a read; any conflicting
    /// descriptor for the same node ID makes all its slots unavailable before
    /// dispatch, including conflicts across roles. An invalid role descriptor
    /// affects that role's devices without erasing valid roles.
    ///
    /// Work and retained evidence are bounded by the backend. The RMS limits
    /// are documented on [`crate::rms::rms_rack_firmware_update_manager`].
    /// Callers own concurrency across calls and any shorter enclosing deadline;
    /// dropping this future cancels its local work.
    ///
    /// # Errors
    ///
    /// Returns `InvalidArgument` before I/O for excessive request input or an
    /// empty node ID. Per-device failures, unsupported remote services and
    /// exhausted deadlines are returned in the corresponding result slots.
    async fn get_firmware_expectations(
        &self,
        request: RackFirmwareExpectationsRequest<'_>,
    ) -> Result<Vec<DeviceFirmwareExpectations>, ComponentManagerError>;

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
