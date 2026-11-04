// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::Debug;
use std::net::IpAddr;

use carbide_secrets::credentials::Credentials;
use mac_address::MacAddress;
use model::component_manager::{FirmwareState, PowerAction, PowerShelfComponent};

use crate::component_common::{ComponentPowerStateResult, PowerStateSource};
use crate::error::ComponentManagerError;
use crate::firmware_inventory::{ComponentFirmwareInventoryResult, FirmwareInventorySource};
use crate::types::FirmwareUpdateOptions;

/// Selects which `PowerShelfManager` backend is used
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Psm,
    #[default]
    Rms,
    Mock,
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Psm => f.write_str("psm"),
            Self::Rms => f.write_str("rms"),
            Self::Mock => f.write_str("mock"),
        }
    }
}

/// Physical network identifiers for a power shelf, used to register with and
/// operate against the backend service (PSM).
#[derive(Debug, Clone)]
pub struct PowerShelfEndpoint {
    pub pmc_ip: IpAddr,
    pub pmc_mac: MacAddress,
    pub pmc_vendor: PowerShelfVendor,
    pub pmc_credentials: Credentials,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerShelfVendor {
    Unknown,
    Liteon,
}

impl PowerShelfVendor {
    pub const DEFAULT: Self = Self::Liteon;
}

#[derive(Debug, Clone)]
pub struct PowerShelfComponentResult {
    pub pmc_mac: MacAddress,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PowerShelfFirmwareUpdateStatus {
    pub pmc_mac: MacAddress,
    pub state: FirmwareState,
    pub target_version: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PowerShelfFirmwareVersions {
    pub pmc_mac: MacAddress,
    pub versions: Vec<String>,
    pub error: Option<String>,
}

/// Backend trait for power shelf management operations.
///
/// Implementations receive physical endpoint information (PMC IP/MAC + vendor)
/// and handle registration with the backend service internally. Results are
/// keyed by the PMC MAC (`pmc_mac`, or `mac_address` for power and firmware
/// observations).
#[async_trait::async_trait]
pub trait PowerShelfManager: Send + Sync + Debug + 'static {
    fn name(&self) -> &str;

    /// Identifies the source when `get_power_state` supports fresh, passive reads.
    /// `None` means callers must reject passive reads before fetching credentials
    /// or invoking the getter; PSM registers devices and reads cached inventory.
    fn power_state_source(&self) -> Option<PowerStateSource> {
        None
    }

    /// Identifies a backend that can read firmware inventory without registering
    /// devices or advancing updates. `None` means callers must skip credential
    /// lookup and report [`ComponentManagerError::Unsupported`]; existing update/status
    /// methods are not substitutes.
    fn firmware_inventory_source(&self) -> Option<FirmwareInventorySource> {
        None
    }

    /// Reads each endpoint's firmware inventory in caller order, preserving repeated
    /// endpoints and inventory members. Member errors must not discard healthy peers.
    /// Versions describe backend observations, not proof of an active image.
    /// This read must not register devices, unlike PSM's `list_firmware`, and
    /// preserves member identity, optional metadata, and individual read failures
    /// rather than flattening the inventory into version strings.
    /// Whole-call failures use [`ComponentManagerError`], including `Unsupported`
    /// without I/O for backends lacking this capability and `InvalidArgument` for
    /// oversized input. Collection/member failures stay nested in their endpoint result.
    async fn get_firmware_inventory(
        &self,
        _endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<ComponentFirmwareInventoryResult>, ComponentManagerError> {
        Err(ComponentManagerError::Unsupported(format!(
            "firmware inventory is not supported by the {} backend",
            self.name()
        )))
    }

    fn supports_firmware_object_json(&self) -> bool {
        false
    }

    async fn power_control(
        &self,
        endpoints: &[PowerShelfEndpoint],
        action: PowerAction,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError>;

    async fn update_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
        target_version: &str,
        components: &[PowerShelfComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<PowerShelfComponentResult>, ComponentManagerError>;

    async fn get_firmware_status(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareUpdateStatus>, ComponentManagerError>;

    async fn list_firmware(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<PowerShelfFirmwareVersions>, ComponentManagerError>;

    /// Reads power observations in endpoint order, identified by each shelf's
    /// PMC MAC. Per-shelf absence and errors are represented in each result;
    /// an error preparing or fetching the batch can fail the whole call.
    /// The PSM backend registers endpoints before reading its cached inventory.
    async fn get_power_state(
        &self,
        endpoints: &[PowerShelfEndpoint],
    ) -> Result<Vec<ComponentPowerStateResult>, ComponentManagerError>;
}
