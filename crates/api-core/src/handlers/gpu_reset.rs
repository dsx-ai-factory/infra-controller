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

//! Handler for the admin out-of-band GPU baseboard reset (Redfish Chassis.Reset).

use ::rpc::forge as rpc;
use carbide_utils::none_if_empty::NoneIfEmpty;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{MachineInterfaceSnapshot, ManagedHostState};
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};
use crate::handlers::bmc_endpoint_explorer::{
    resolve_bmc_interface, validate_and_complete_bmc_endpoint_request,
};
use crate::handlers::utils::convert_and_log_machine_id;

/// Maps the requested reset action to a Redfish chassis power control.
///
/// v1 only supports `ForceRestart`; all other actions are rejected because
/// Redfish `Chassis.Reset` allowable `ResetType` values are vendor-specific.
fn map_gpu_reset_action(action: i32) -> Result<libredfish::SystemPowerControl, Status> {
    use rpc::admin_power_control_request::SystemPowerControl as Spc;
    let action = Spc::try_from(action).map_err(|_| Status::invalid_argument("unknown action"))?;
    match action {
        Spc::ForceRestart => Ok(libredfish::SystemPowerControl::ForceRestart),
        Spc::On
        | Spc::GracefulRestart
        | Spc::AcPowercycle
        | Spc::GracefulShutdown
        | Spc::ForceOff => Err(Status::invalid_argument(
            "action must be ForceRestart (the only reset type supported today)",
        )),
    }
}

/// Handle an administrative out-of-band GPU baseboard reset and return the reset response.
pub(crate) async fn admin_gpu_reset(
    api: &Api,
    request: Request<rpc::AdminGpuResetRequest>,
) -> Result<Response<rpc::AdminGpuResetResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let machine_id = convert_and_log_machine_id(req.machine_id.as_ref())?;
    let chassis_id = req
        .chassis_id
        .none_if_empty()
        // xtask:allow-error-case: HGX_Chassis_0 is a case-sensitive Redfish chassis id
        .ok_or_else(|| Status::invalid_argument("chassis_id is required (e.g. HGX_Chassis_0)"))?;

    // Reject tenant-assigned hosts and require maintenance mode before a reset.
    let (host_machine, txn) = api
        .load_machine(&machine_id, MachineSearchConfig::default())
        .await?;
    if matches!(
        host_machine.current_state(),
        ManagedHostState::Assigned { .. }
    ) {
        return Err(Status::failed_precondition(
            "host is assigned to a tenant; a GPU reset is not allowed",
        ));
    }
    if host_machine.health_reports.maintenance_override().is_none() {
        return Err(Status::failed_precondition(
            "host must be in maintenance mode before a GPU reset \
             (nico-admin-cli managed-host maintenance on <machine-id>)",
        ));
    }
    drop(txn);

    let action = map_gpu_reset_action(req.action)?;

    let mut txn = api.txn_begin().await?;
    let (bmc_endpoint_request, _) =
        validate_and_complete_bmc_endpoint_request(&mut txn, None, Some(machine_id)).await?;
    txn.commit().await?;

    let (bmc_addr, bmc_mac_address) = resolve_bmc_interface(api, &bmc_endpoint_request).await?;
    let machine_interface = MachineInterfaceSnapshot::mock_with_mac(bmc_mac_address);

    api.bmc_client
        .redfish_chassis_reset(bmc_addr, &machine_interface, &chassis_id, action)
        .await
        .map_err(|e| CarbideError::internal(e.to_string()))?;

    Ok(Response::new(rpc::AdminGpuResetResponse {}))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::map_gpu_reset_action;

    #[test]
    fn gpu_reset_action_maps_force_restart_and_rejects_others() {
        use libredfish::SystemPowerControl as L;

        use super::rpc::admin_power_control_request::SystemPowerControl as Spc;
        value_scenarios!(run = |a: i32| { map_gpu_reset_action(a).map_err(|_| ()) };
            "gpu reset action mapping" {
                Spc::ForceRestart as i32 => Ok(L::ForceRestart),
                Spc::On as i32 => Err(()),
                0 => Err(()), // omitted action (proto3 default)
                Spc::GracefulRestart as i32 => Err(()),
                Spc::AcPowercycle as i32 => Err(()),
                Spc::GracefulShutdown as i32 => Err(()),
                Spc::ForceOff as i32 => Err(()),
                9999 => Err(()),
            }
        );
    }
}
