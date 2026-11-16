// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use bmc_backend::BackendBmcControl;
use carbide_redfish::state_handler::state_handler_platform_error as platform_error;
use chrono::Duration;
use model::machine::{DisableOnboardNicsState, ManagedHostStateSnapshot};
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use crate::context::MachineStateHandlerContextObjects;

pub(super) enum Progress {
    Wait,
    Next(DisableOnboardNicsState),
    Complete,
}

pub(super) const WAIT_REASON: &str = "Waiting for onboard NIC configuration to be applied";

/// Whether the host's onboard NICs must be cut off the management network:
/// its managed DPUs carry its traffic, and its BMC controls the NICs' access.
/// Hosts without managed DPUs keep their onboard NICs.
pub(super) async fn isolate(
    bmc_control: &BackendBmcControl<'_>,
    snapshot: &ManagedHostStateSnapshot,
) -> Result<bool, StateHandlerError> {
    if !snapshot.has_managed_dpus() {
        return Ok(false);
    }
    bmc_control
        .controls_host_management_network_access()
        .await
        .map_err(|error| platform_error("controls_host_management_network_access", error))
}

/// Shared ingestion and post-AC-cycle sequence: enable, wait, disable, wait.
/// The toggle applies the setting even when the BMC already stores it, which
/// it can after an AUX power cycle re-enables the NICs.
/// Each successful write advances persisted state; errors retry only that write.
pub(super) async fn advance(
    state: &DisableOnboardNicsState,
    snapshot: &ManagedHostStateSnapshot,
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
) -> Result<Progress, StateHandlerError> {
    match state {
        DisableOnboardNicsState::ConfigureOnboardNics { enable } => {
            let operation = if *enable {
                "enable_onboard_nics"
            } else {
                "disable_onboard_nics"
            };
            let bmc = ctx.services.connect_bmc(&snapshot.host_snapshot).await?;
            bmc.bmc_control()
                .map_err(|error| platform_error(operation, error.into()))?
                .set_host_management_network_access(*enable)
                .await
                .map_err(|error| platform_error(operation, error))?;
            Ok(Progress::Next(
                DisableOnboardNicsState::WaitForConfigToBeApplied {
                    onboard_nics_disabled: !enable,
                },
            ))
        }
        DisableOnboardNicsState::WaitForConfigToBeApplied {
            onboard_nics_disabled,
        } => {
            if snapshot.host_snapshot.state.version.since_state_change() < Duration::seconds(10) {
                Ok(Progress::Wait)
            } else if *onboard_nics_disabled {
                Ok(Progress::Complete)
            } else {
                Ok(Progress::Next(
                    DisableOnboardNicsState::ConfigureOnboardNics { enable: false },
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bmc_backend::BackendBmc;
    use carbide_redfish::libredfish::test_support::RedfishSim;
    use carbide_redfish::libredfish::{RedfishAuth, RedfishClientPool};
    use model::test_support::machine_snapshot::managed_host_state_snapshot;

    use super::*;

    /// libredfish controls the NICs of a Vera Rubin service root, whether or
    /// not the BMC reports their access.
    #[tokio::test]
    async fn onboard_nics_are_isolated_only_behind_managed_dpus_on_bmcs_that_control_them() {
        for (scenario, managed_dpus, product, expected) in [
            (
                "managed DPUs behind a Vera Rubin BMC",
                true,
                "VR NVL72",
                true,
            ),
            ("another NVIDIA BMC", true, "GB200 NVL", false),
            ("no managed DPUs", false, "VR NVL72", false),
        ] {
            let sim = Arc::new(RedfishSim::default());
            sim.set_service_root_vendor(Some("NVIDIA".into()));
            sim.set_service_root_product(Some(product.into()));
            let client = sim
                .create_client("host", None, RedfishAuth::Anonymous, None)
                .await
                .expect("the simulated BMC connects");
            let bmc = BackendBmc::from(client);
            let mut snapshot = managed_host_state_snapshot();
            if !managed_dpus {
                snapshot.dpu_snapshots.clear();
            }
            let bmc_control = bmc.bmc_control().expect("libredfish controls every BMC");

            assert_eq!(
                isolate(&bmc_control, &snapshot)
                    .await
                    .expect("the gate reads"),
                expected,
                "{scenario}"
            );
        }
    }
}
