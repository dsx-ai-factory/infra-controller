// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::sync::Arc;

use carbide_redfish::libredfish::RedfishClientPool;
use carbide_secrets::credentials::Credentials;
use model::component_manager::{ComputeTrayComponent, PowerAction};

use crate::compute_tray_manager::{
    Backend, ComputeTrayEndpoint, ComputeTrayFirmwareUpdateStatus, ComputeTrayResult,
    ComputeTrayVendor,
};
use crate::error::ComponentManagerError;

/// Compute tray manager backend that uses NICo-core's Redfish stack for
/// power control. Firmware operations are not yet supported.
pub struct CoreComputeTrayManager {
    redfish_pool: Arc<dyn RedfishClientPool>,
}

impl std::fmt::Debug for CoreComputeTrayManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreComputeTrayManager").finish()
    }
}

impl CoreComputeTrayManager {
    pub fn new(redfish_pool: Arc<dyn RedfishClientPool>) -> Self {
        Self { redfish_pool }
    }
}

fn map_vendor(vendor: ComputeTrayVendor) -> Option<libredfish::model::service_root::RedfishVendor> {
    use libredfish::model::service_root::RedfishVendor;
    match vendor {
        ComputeTrayVendor::Dell => Some(RedfishVendor::Dell),
        ComputeTrayVendor::Hpe => Some(RedfishVendor::Hpe),
        ComputeTrayVendor::Lenovo => Some(RedfishVendor::Lenovo),
        ComputeTrayVendor::Supermicro => Some(RedfishVendor::Supermicro),
        ComputeTrayVendor::Nvidia | ComputeTrayVendor::Unknown => None,
    }
}

fn map_power_action(action: PowerAction) -> libredfish::SystemPowerControl {
    match action {
        PowerAction::On => libredfish::SystemPowerControl::On,
        PowerAction::GracefulShutdown => libredfish::SystemPowerControl::GracefulShutdown,
        PowerAction::ForceOff => libredfish::SystemPowerControl::ForceOff,
        PowerAction::GracefulRestart => libredfish::SystemPowerControl::GracefulRestart,
        PowerAction::ForceRestart => libredfish::SystemPowerControl::ForceRestart,
        PowerAction::AcPowercycle => libredfish::SystemPowerControl::ACPowercycle,
    }
}

#[async_trait::async_trait]
impl crate::compute_tray_manager::ComputeTrayManager for CoreComputeTrayManager {
    fn name(&self) -> &str {
        "core"
    }

    fn backend(&self) -> Backend {
        Backend::Core
    }

    async fn power_control(
        &self,
        endpoints: &[ComputeTrayEndpoint],
        action: PowerAction,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        let redfish_action = map_power_action(action);
        let mut results = Vec::with_capacity(endpoints.len());

        for ep in endpoints {
            let Credentials::UsernamePassword {
                ref username,
                ref password,
            } = ep.bmc_credentials;

            let auth = carbide_redfish::libredfish::RedfishAuth::Direct(
                username.clone(),
                password.clone(),
            );
            let vendor = map_vendor(ep.vendor);

            let outcome = async {
                let client = self
                    .redfish_pool
                    .create_client(&ep.bmc_ip.to_string(), Some(443), auth, vendor)
                    .await
                    .map_err(|e| format!("failed to create Redfish client: {e}"))?;

                client
                    .power(redfish_action)
                    .await
                    .map_err(|e| format!("Redfish power control failed: {e}"))
            }
            .await;

            results.push(ComputeTrayResult {
                bmc_ip: ep.bmc_ip,
                bmc_mac: ep.bmc_mac,
                success: outcome.is_ok(),
                error: outcome.err(),
            });
        }

        Ok(results)
    }

    async fn update_firmware(
        &self,
        _endpoints: &[ComputeTrayEndpoint],
        _target_version: &str,
        _components: &[ComputeTrayComponent],
        _options: &crate::types::FirmwareUpdateOptions,
    ) -> Result<Vec<ComputeTrayResult>, ComponentManagerError> {
        Err(ComponentManagerError::Internal(
            "firmware update is not supported by the core compute tray backend".into(),
        ))
    }

    async fn get_firmware_status(
        &self,
        _endpoints: &[ComputeTrayEndpoint],
    ) -> Result<Vec<ComputeTrayFirmwareUpdateStatus>, ComponentManagerError> {
        Err(ComponentManagerError::Internal(
            "firmware status is not supported by the core compute tray backend".into(),
        ))
    }

    async fn list_firmware_bundles(&self) -> Result<Vec<String>, ComponentManagerError> {
        Err(ComponentManagerError::Internal(
            "firmware bundles listing is not supported by the core compute tray backend".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use libredfish::model::service_root::RedfishVendor;

    use super::*;

    #[test]
    fn compute_tray_vendor_maps_to_redfish_vendor() {
        value_scenarios!(map_vendor:
            "supported Redfish vendors" {
                ComputeTrayVendor::Dell => Some(RedfishVendor::Dell),
                ComputeTrayVendor::Hpe => Some(RedfishVendor::Hpe),
                ComputeTrayVendor::Lenovo => Some(RedfishVendor::Lenovo),
                ComputeTrayVendor::Supermicro => Some(RedfishVendor::Supermicro),
            }

            "vendors without a Redfish adapter" {
                ComputeTrayVendor::Nvidia => None,
                ComputeTrayVendor::Unknown => None,
            }
        );
    }
}
