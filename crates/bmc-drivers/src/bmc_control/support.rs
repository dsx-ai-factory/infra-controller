/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Redfish manager reset and network-protocol mechanics.

use bmc_platform::{DriverOutcome, OpCx, PlatformError};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::manager::ManagerNetworkProtocolUpdate;
use nv_redfish::resource::ResetType;

/// Manager reset and network-protocol writes shared by BMC operations.
pub(super) trait RedfishBmcControlExt<B: Bmc> {
    /// Resets the manager over Redfish, falling back to an IPMI cold reset when
    /// the runtime attached IPMI and Redfish refused or could not be reached.
    async fn reset_manager(&self, reset_type: ResetType) -> Result<DriverOutcome, PlatformError>
    where
        B::Error: ActionError;

    /// Writes to the manager's advertised `NetworkProtocol` resource.
    async fn update_manager_network_protocol(
        &self,
        body: ManagerNetworkProtocolUpdate,
    ) -> Result<DriverOutcome, PlatformError>;
}

impl<B: Bmc> RedfishBmcControlExt<B> for OpCx<'_, B> {
    async fn reset_manager(&self, reset_type: ResetType) -> Result<DriverOutcome, PlatformError>
    where
        B::Error: ActionError,
    {
        let redfish = self
            .manager()
            .await?
            .reset(Some(reset_type))
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error));
        match (redfish, self.ipmi()) {
            (Err(_), Some(ipmi)) => ipmi
                .bmc_cold_reset()
                .await
                .map(|()| DriverOutcome::complete()),
            (result, _) => result,
        }
    }

    async fn update_manager_network_protocol(
        &self,
        body: ManagerNetworkProtocolUpdate,
    ) -> Result<DriverOutcome, PlatformError> {
        self.manager()
            .await?
            .network_protocol()
            .await
            .map_err(|error| self.map_redfish_error(error))?
            .ok_or(PlatformError::Unsupported)?
            .update(&body)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_redfish_error(error))
    }
}
