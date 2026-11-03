/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Standard Redfish power control.

use async_trait::async_trait;
use bmc_platform::{DriverOutcome, OpCx, PlatformError, Power, Quirk};
use nv_redfish::core::{ActionError, Bmc};
use nv_redfish::resource::{PowerState, ResetType};

use crate::power::support::RedfishPowerExt as _;
use crate::resources::RedfishResourcesExt as _;

/// Spec-compliant Redfish power control.
///
/// With [`Quirk::RedfishRestartCutsDpuPower`], restarts go over IPMI.
pub(crate) struct StandardPower;

#[async_trait]
impl<B: Bmc> Power<B> for StandardPower
where
    B::Error: ActionError,
{
    fn standard(&self) -> &dyn Power<B> {
        self
    }

    async fn state(&self, cx: &OpCx<'_, B>) -> Result<Option<PowerState>, PlatformError> {
        cx.system()
            .await?
            .power_state()
            .map(Some)
            .ok_or(PlatformError::NoContent)
    }

    /// Standard Redfish offers no AC power cycle.
    async fn ac_power_cycle_supported(&self, _cx: &OpCx<'_, B>) -> Result<bool, PlatformError> {
        Ok(false)
    }

    async fn set(
        &self,
        cx: &OpCx<'_, B>,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        match reset_type {
            // Platforms with an AC power cycle implement it in their own driver.
            ResetType::FullPowerCycle => Err(PlatformError::Unsupported),
            ResetType::ForceRestart | ResetType::GracefulRestart
                if cx.has_quirk(Quirk::RedfishRestartCutsDpuPower) =>
            {
                cx.ipmi_restart().await
            }
            other => already_satisfied_is_complete(
                cx.system()
                    .await?
                    .reset(Some(other))
                    .await
                    .map(DriverOutcome::from)
                    .map_err(|error| cx.map_redfish_error(error)),
            ),
        }
    }

    async fn chassis_reset(
        &self,
        cx: &OpCx<'_, B>,
        chassis_id: &str,
        reset_type: ResetType,
    ) -> Result<DriverOutcome, PlatformError> {
        already_satisfied_is_complete(
            cx.chassis_by_id(chassis_id)
                .await?
                .ok_or(PlatformError::Unsupported)?
                .reset(Some(reset_type))
                .await
                .map(DriverOutcome::from)
                .map_err(|error| cx.map_redfish_error(error)),
        )
    }
}

/// BMCs answer a reset that would not change the power state with 409, which
/// for power control means the requested state already holds.
fn already_satisfied_is_complete(
    result: Result<DriverOutcome, PlatformError>,
) -> Result<DriverOutcome, PlatformError> {
    match result {
        Err(PlatformError::Bmc { status: 409, .. }) => Ok(DriverOutcome::complete()),
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use bmc_platform::{IpmiOps, PlatformIdentity, SystemIdentity};
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn standard_reset_posts_to_the_selected_systems_advertised_action() {
        let bmc = bmc_mock::test_support::dell_poweredge_r750_bmc().await;
        let identity = PlatformIdentity {
            system: Some(SystemIdentity {
                id: "System.Embedded.1".to_string(),
                ..SystemIdentity::default()
            }),
            ..PlatformIdentity::default()
        };
        let cx = OpCx::new(bmc.bmc.as_ref(), bmc.service_root.as_ref(), &identity);
        cx.system().await.expect("selected system resolves");
        bmc.http_client.take_requests();

        let outcome = StandardPower
            .set(&cx, ResetType::ForceOff)
            .await
            .expect("reset succeeds");
        assert_eq!(outcome, DriverOutcome::complete());

        let requests = bmc.http_client.take_requests();
        let request = requests
            .iter()
            .find(|request| request.method == "POST")
            .expect("a POST was issued");
        assert!(
            request
                .uri
                .ends_with("/redfish/v1/Systems/System.Embedded.1/Actions/ComputerSystem.Reset"),
            "{}",
            request.uri
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&request.body).expect("JSON body"),
            json!({"ResetType": "ForceOff"})
        );
    }

    #[derive(Default)]
    struct RecordingIpmi {
        power_resets: AtomicUsize,
    }

    #[async_trait]
    impl IpmiOps for RecordingIpmi {
        async fn bmc_cold_reset(&self) -> Result<(), PlatformError> {
            Ok(())
        }

        async fn chassis_power_reset(&self) -> Result<(), PlatformError> {
            self.power_resets.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn restarts_go_over_ipmi_when_a_redfish_restart_cuts_dpu_power() {
        let bmc = bmc_mock::test_support::dell_poweredge_r750_bmc().await;
        let identity = PlatformIdentity::default();
        let ipmi = RecordingIpmi::default();
        let quirks = BTreeSet::from([Quirk::RedfishRestartCutsDpuPower]);
        let cx = OpCx::new(bmc.bmc.as_ref(), bmc.service_root.as_ref(), &identity)
            .with_ipmi(&ipmi)
            .with_quirks(&quirks);
        bmc.http_client.take_requests();

        for reset_type in [ResetType::ForceRestart, ResetType::GracefulRestart] {
            assert_eq!(
                StandardPower.set(&cx, reset_type).await,
                Ok(DriverOutcome::complete())
            );
        }
        assert_eq!(ipmi.power_resets.load(Ordering::SeqCst), 2);
        assert!(bmc.http_client.take_requests().is_empty());
    }

    #[tokio::test]
    async fn standard_ac_power_cycle_is_unsupported_without_a_request() {
        let bmc = bmc_mock::test_support::dell_poweredge_r750_bmc().await;
        let identity = PlatformIdentity::default();
        let cx = OpCx::new(bmc.bmc.as_ref(), bmc.service_root.as_ref(), &identity);
        bmc.http_client.take_requests();

        assert_eq!(
            StandardPower.set(&cx, ResetType::FullPowerCycle).await,
            Err(PlatformError::Unsupported)
        );
        assert!(bmc.http_client.take_requests().is_empty());
    }
}
