// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::fmt::Debug;
use std::net::IpAddr;

use carbide_secrets::credentials::Credentials;
use mac_address::MacAddress;
use model::component_manager::{ComputeTrayComponent, FirmwareState, PowerAction};

use crate::error::ComponentManagerError;
use crate::types::FirmwareUpdateOptions;

/// Physical network identifiers for a compute tray, used to register with and
/// operate against the backend service (CTM).
#[derive(Debug, Clone)]
pub struct ComputeTrayEndpoint {
    pub vendor: ComputeTrayVendor,
    pub bmc_ip: IpAddr,
    pub bmc_mac: MacAddress,
    pub bmc_credentials: Credentials,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeTrayVendor {
    Unknown,
    Dell,
    Hpe,
    Lenovo,
    Supermicro,
    Nvidia,
}

impl From<bmc_vendor::BMCVendor> for ComputeTrayVendor {
    fn from(vendor: bmc_vendor::BMCVendor) -> Self {
        match vendor {
            bmc_vendor::BMCVendor::Dell => Self::Dell,
            bmc_vendor::BMCVendor::Hpe => Self::Hpe,
            bmc_vendor::BMCVendor::Lenovo => Self::Lenovo,
            bmc_vendor::BMCVendor::Supermicro => Self::Supermicro,
            bmc_vendor::BMCVendor::Nvidia => Self::Nvidia,
            bmc_vendor::BMCVendor::LenovoAMI
            | bmc_vendor::BMCVendor::Liteon
            | bmc_vendor::BMCVendor::Delta
            | bmc_vendor::BMCVendor::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComputeTrayModel {
    Unknown,
    Viking,
    GB200,
    GB300,
}

#[derive(Debug, Clone)]
pub struct ComputeTrayResult {
    pub bmc_ip: IpAddr,
    pub bmc_mac: MacAddress,
    pub success: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ComputeTrayFirmwareUpdateStatus {
    pub bmc_ip: IpAddr,
    pub bmc_mac: MacAddress,
    pub state: FirmwareState,
    pub target_version: String,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Core,
    #[default]
    Rms,
    Mock,
}

impl std::fmt::Display for Backend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rms => f.write_str("rms"),
            Self::Core => f.write_str("core"),
            Self::Mock => f.write_str("mock"),
        }
    }
}

/// Backend trait for compute tray management operations.
///
/// Implementations receive physical endpoint information (BMC IP/MAC + vendor)
/// and handle registration with the backend service internally. Each result
/// echoes the endpoint's `bmc_ip` and `bmc_mac` so callers can correlate an
/// outcome back to the target they supplied without a side lookup (the BMC IP
/// is not a stable correlation key before ingestion, where leases churn).
#[async_trait::async_trait]
pub trait ComputeTrayManager: Send + Sync + Debug + 'static {
    fn name(&self) -> &str;

    fn backend(&self) -> Backend;

    async fn power_control(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        action: PowerAction,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError>;

    /// Update firmware on compute trays.
    ///
    /// Endpoints that resolve to an ingested machine row are handled through
    /// that row; rack-scale trays with no row yet (pre-ingestion) are resolved
    /// from the expected inventory by BMC MAC where the backend supports it.
    async fn update_firmware(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        target_version: &str,
        components: &[ComputeTrayComponent],
        options: &FirmwareUpdateOptions,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError>;

    async fn get_firmware_status(
        &self,
        endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComputeTrayFirmwareUpdateStatus>, ComponentManagerError>;

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError>;
}

#[cfg(test)]
mod tests {
    use bmc_vendor::BMCVendor;
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn bmc_vendor_maps_to_compute_tray_vendor() {
        value_scenarios!(run = ComputeTrayVendor::from;
            "supported compute tray vendors" {
                BMCVendor::Dell => ComputeTrayVendor::Dell,
                BMCVendor::Hpe => ComputeTrayVendor::Hpe,
                BMCVendor::Lenovo => ComputeTrayVendor::Lenovo,
                BMCVendor::Supermicro => ComputeTrayVendor::Supermicro,
                BMCVendor::Nvidia => ComputeTrayVendor::Nvidia,
            }

            "unsupported compute tray vendors" {
                BMCVendor::LenovoAMI => ComputeTrayVendor::Unknown,
                BMCVendor::Liteon => ComputeTrayVendor::Unknown,
                BMCVendor::Delta => ComputeTrayVendor::Unknown,
                BMCVendor::Unknown => ComputeTrayVendor::Unknown,
            }
        );
    }
}
