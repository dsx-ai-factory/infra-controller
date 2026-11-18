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
        let redfish = async {
            self.manager()
                .await?
                .reset(Some(reset_type))
                .await
                .map(DriverOutcome::from)
                .map_err(|error| self.map_redfish_error(error))
        }
        .await;
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use bmc_platform::{IpmiOps, ManagerIdentity, PlatformIdentity};

    use super::*;

    #[derive(Default)]
    struct RecordingIpmi {
        cold_resets: AtomicUsize,
    }

    #[async_trait]
    impl IpmiOps for RecordingIpmi {
        async fn bmc_cold_reset(&self) -> Result<(), PlatformError> {
            self.cold_resets.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn chassis_power_reset(&self) -> Result<(), PlatformError> {
            Ok(())
        }
    }

    /// A manager Redfish cannot return, as from a hung or refusing Redfish
    /// service, still falls back to the IPMI cold reset.
    #[tokio::test]
    async fn a_manager_redfish_cannot_return_falls_back_to_ipmi() {
        let bmc = bmc_mock::test_support::dell_poweredge_r750_bmc().await;
        let identity = PlatformIdentity {
            manager: Some(ManagerIdentity {
                id: "Unlisted".to_string(),
                ..ManagerIdentity::default()
            }),
            ..PlatformIdentity::default()
        };
        let ipmi = RecordingIpmi::default();
        let cx = OpCx::new(bmc.bmc.as_ref(), bmc.service_root.as_ref(), &identity).with_ipmi(&ipmi);

        assert_eq!(
            cx.reset_manager(ResetType::GracefulRestart).await,
            Ok(DriverOutcome::complete())
        );
        assert_eq!(ipmi.cold_resets.load(Ordering::SeqCst), 1);
    }
}
