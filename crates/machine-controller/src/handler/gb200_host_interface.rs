/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 *
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 *
 * http://www.apache.org/licenses/LICENSE-2.0
 *
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

//! Checks and restores the BMC host interface that GB200 SBIOS uses for
//! Redfish access.

use bmc_platform::{ManagerSettings, ManagerSettingsStatus, PlatformError};
use carbide_redfish::state_handler::state_handler_platform_error as platform_error;
use model::machine::ManagedHostStateSnapshot;
use model::rack_type::RackProductFamily;
use serde_json::Value;
use state_controller::state_handler::{StateHandlerContext, StateHandlerError};

use crate::context::MachineStateHandlerContextObjects;

/// Reports whether a GB200 waiting for `scout` needs its BMC host interface
/// restored before another boot can refresh the UEFI HTTP option.
pub(super) async fn gb200_host_interface_address_is_missing(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<bool, StateHandlerError> {
    if !is_reported_gb200(ctx, mh_snapshot).await? {
        return Ok(false);
    }

    let bmc = ctx.services.connect_bmc(&mh_snapshot.host_snapshot).await?;
    let status = async { bmc.bmc_control()?.settings_status().await };
    Ok(address_is_missing(&status.await.map_err(
        |error: PlatformError| platform_error("bmc_control.settings_status", error),
    )?))
}

/// Restores the expected address only for hosts identified as GB200 by their
/// persisted Site Explorer report.
pub(super) async fn repair_gb200_host_interface_if_needed(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<(), StateHandlerError> {
    if !is_reported_gb200(ctx, mh_snapshot).await? {
        return Ok(());
    }

    let bmc = ctx.services.connect_bmc(&mh_snapshot.host_snapshot).await?;
    let bmc_control = bmc
        .bmc_control()
        .map_err(|error| platform_error("bmc_control", error.into()))?;
    let status = bmc_control
        .settings_status()
        .await
        .map_err(|error| platform_error("bmc_control.settings_status", error))?;
    if status.is_applied {
        return Ok(());
    }

    // Applying is a no-op for the expected address, which a status that
    // cannot read address values does not report as applied.
    match bmc_control
        .apply_settings(&ManagerSettings::default(), &ctx.services.instrumentation)
        .await
    {
        Ok(_) => {
            if address_is_missing(&status) {
                tracing::warn!(
                    machine_id = %mh_snapshot.host_snapshot.id,
                    "Restored the GB200 BMC hostusb0 static IPv4 address before host platform configuration"
                );
            }
            Ok(())
        }
        Err(PlatformError::ManualInterventionRequired { .. }) => {
            Err(StateHandlerError::ManualInterventionRequired(format!(
                "GB200 host {} has unexpected static IPv4 configuration on hostusb0; refusing to overwrite it",
                mh_snapshot.host_snapshot.id,
            )))
        }
        Err(error) => Err(platform_error("bmc_control.apply_settings", error)),
    }
}

/// `hostusb0` reports no static IPv4 address at all, as opposed to a
/// different one: every difference's reported value is an empty list.
fn address_is_missing(status: &ManagerSettingsStatus) -> bool {
    !status.is_applied
        && !status.differences.is_empty()
        && status.differences.iter().all(|diff| {
            diff.actual
                .as_ref()
                .and_then(Value::as_array)
                .is_some_and(Vec::is_empty)
        })
}

/// Uses only the persisted Redfish report so stale or missing rack association
/// data cannot enable this BMC write on other hardware.
async fn is_reported_gb200(
    ctx: &mut StateHandlerContext<'_, MachineStateHandlerContextObjects>,
    mh_snapshot: &ManagedHostStateSnapshot,
) -> Result<bool, StateHandlerError> {
    let Some(host_bmc_ip) = mh_snapshot.host_snapshot.status.bmc_info.ip else {
        return Ok(false);
    };

    let endpoints =
        db::explored_endpoints::find_by_ips(&ctx.services.db_pool, vec![host_bmc_ip]).await?;
    let product_family = endpoints
        .first()
        .and_then(|endpoint| endpoint.report.model())
        .as_deref()
        .and_then(RackProductFamily::from_hardware_model);

    Ok(product_family == Some(RackProductFamily::Gb200))
}

#[cfg(test)]
mod tests {
    use bmc_platform::ManagerSettingsDiff;
    use carbide_test_support::value_scenarios;
    use serde_json::json;

    use super::*;

    fn differing(actual: Option<Value>) -> ManagerSettingsStatus {
        ManagerSettingsStatus {
            is_applied: false,
            differences: vec![ManagerSettingsDiff {
                key: "hostusb0.IPv4StaticAddresses".to_string(),
                expected: json!([{"Address": "10.0.1.1"}]),
                actual,
            }],
        }
    }

    /// Only an empty address list rewinds discovery; a different or unread
    /// address is left for the repair, which refuses to overwrite a
    /// different one.
    #[test]
    fn only_an_empty_address_list_is_missing() {
        value_scenarios!(run = |status: ManagerSettingsStatus| address_is_missing(&status);
            "missing" {
                differing(Some(json!([]))) => true,
            }
            "configured, different, or unread" {
                ManagerSettingsStatus { is_applied: true, differences: vec![] } => false,
                ManagerSettingsStatus { is_applied: false, differences: vec![] } => false,
                differing(Some(json!([{"Address": "192.0.2.1"}]))) => false,
                differing(None) => false,
            }
        );
    }
}
