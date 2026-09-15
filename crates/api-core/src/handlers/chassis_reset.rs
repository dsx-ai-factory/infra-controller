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

//! Handler for the administrative out-of-band Redfish chassis reset.

use ::rpc::forge as rpc;
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::machine::HostMachineId;
use model::machine::machine_search_config::MachineSearchConfig;
use model::machine::{MachineMaintenanceOperation, ManagedHostState};
use tonic::{Request, Response, Status};

use crate::api::{Api, log_request_data};
use crate::handlers::utils::convert_and_log_machine_id;

/// Maps the requested reset action to a Redfish chassis power control.
///
/// v1 only supports `ForceRestart`; all other actions are rejected because
/// Redfish `Chassis.Reset` allowable `ResetType` values are vendor-specific.
fn map_chassis_reset_action(action: i32) -> Result<libredfish::SystemPowerControl, Status> {
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

/// Handle an administrative out-of-band Redfish chassis reset and return the response.
pub(crate) async fn admin_chassis_reset(
    api: &Api,
    request: Request<rpc::AdminChassisResetRequest>,
) -> Result<Response<rpc::AdminChassisResetResponse>, Status> {
    log_request_data(&request);
    let req = request.into_inner();
    let machine_id: HostMachineId = convert_and_log_machine_id(req.machine_id.as_ref())?;
    let chassis_id = req
        .chassis_id
        .none_if_empty()
        // xtask:allow-error-case: HGX_Chassis_0 is a case-sensitive Redfish chassis id
        .ok_or_else(|| Status::invalid_argument("chassis_id is required (e.g. HGX_Chassis_0)"))?;

    map_chassis_reset_action(req.action)?;

    let (host_machine, mut txn) = api
        .load_machine(
            &machine_id,
            MachineSearchConfig {
                for_update: true,
                ..Default::default()
            },
        )
        .await?;
    if !matches!(host_machine.current_state(), ManagedHostState::Ready) {
        return Err(Status::failed_precondition(
            "host state does not allow a chassis reset",
        ));
    }
    if db::instance::find_id_by_machine_id(&mut txn, &host_machine.id)
        .await?
        .is_some()
    {
        return Err(Status::failed_precondition(
            "host is assigned to a tenant; a chassis reset is not allowed",
        ));
    }

    if host_machine.health_reports.maintenance_override().is_none() {
        return Err(Status::failed_precondition(
            "host must be in maintenance mode before a chassis reset (enable it with SetMaintenance)",
        ));
    }

    db::machine::set_machine_maintenance_requested(
        &mut txn,
        machine_id,
        "admin-chassis-reset",
        MachineMaintenanceOperation::ChassisReset { chassis_id },
    )
    .await?;
    txn.commit().await?;

    if let Err(error) = api
        .machine_state_handler_enqueuer
        .enqueue_object(&machine_id)
        .await
    {
        tracing::warn!(
            %machine_id,
            %error,
            "Failed to enqueue managed host after recording chassis reset request",
        );
    }

    Ok(Response::new(rpc::AdminChassisResetResponse {}))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::map_chassis_reset_action;

    #[test]
    fn chassis_reset_action_maps_force_restart_and_rejects_others() {
        use libredfish::SystemPowerControl as L;

        use super::rpc::admin_power_control_request::SystemPowerControl as Spc;
        value_scenarios!(run = |a: i32| { map_chassis_reset_action(a).map_err(|_| ()) };
            "chassis reset action mapping" {
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
