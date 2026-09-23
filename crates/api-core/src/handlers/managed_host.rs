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

use ::rpc::forge as rpc;
use carbide_uuid::machine::{HostMachineId, StableHostMachineId};
use model::machine::ManagedHostState;
use model::machine::machine_search_config::MachineSearchConfig;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_machine_id, log_request_data};
use crate::auth::AuthContext;
use crate::handlers::primary_interface::{PrimaryInterfaceSelector, update_primary_interface};
use crate::handlers::utils::{convert_and_log_machine_id, enqueue_boot_interface_reconciliation};

pub(crate) async fn decommission_managed_host(
    api: &Api,
    request: Request<rpc::DecommissionManagedHostRequest>,
) -> Result<Response<rpc::DecommissionManagedHostResponse>, Status> {
    log_request_data(&request);
    let machine_id: StableHostMachineId =
        convert_and_log_machine_id(request.into_inner().machine_id.as_ref())?;

    let mut txn = api.txn_begin().await?;
    let machine = db::machine::find_one(
        &mut txn,
        &machine_id,
        MachineSearchConfig {
            for_update: true,
            ..Default::default()
        },
    )
    .await?
    .ok_or_else(|| CarbideError::NotFoundError {
        kind: "managed host",
        id: machine_id.to_string(),
    })?;

    if !matches!(machine.current_state(), ManagedHostState::Ready) {
        return Err(CarbideError::FailedPrecondition(format!(
            "managed host {machine_id} must be in the ready state to be decommissioned (current state: {})",
            machine.current_state()
        ))
        .into());
    }

    let dpus = db::machine::find_dpus_by_host_machine_id(&mut txn, &machine_id).await?;
    let unsupported_dpus = dpus
        .iter()
        .filter(|dpu| !dpu.status.bmc_info.supports_bfb_install())
        .map(|dpu| {
            format!(
                "{} (BMC firmware {})",
                dpu.id,
                dpu.status
                    .bmc_info
                    .firmware_version
                    .as_deref()
                    .unwrap_or("unknown")
            )
        })
        .collect::<Vec<_>>();
    if !unsupported_dpus.is_empty() {
        return Err(CarbideError::FailedPrecondition(format!(
            "managed host {machine_id} cannot be decommissioned because its dpus do not support bfb installation through redfish: {}",
            unsupported_dpus.join(", ")
        ))
        .into());
    }

    db::machine::set_decommission_requested(&mut txn, machine_id).await?;
    txn.commit().await?;

    if let Err(error) = api
        .machine_state_handler_enqueuer
        .enqueue_object(&machine_id)
        .await
    {
        tracing::warn!(
            %machine_id,
            %error,
            "Failed to enqueue managed host after recording decommission request",
        );
    }

    Ok(Response::new(rpc::DecommissionManagedHostResponse {}))
}

pub(crate) async fn set_primary_dpu(
    api: &Api,
    request: Request<rpc::SetPrimaryDpuRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let host_machine_id = request
        .host_machine_id
        .ok_or_else(|| CarbideError::InvalidArgument("host machine ID is required".to_string()))?;
    let dpu_machine_id = request
        .dpu_machine_id
        .ok_or_else(|| CarbideError::InvalidArgument("DPU machine ID is required".to_string()))?;
    // `reboot` is only a compatibility alias for `force_reconcile`.
    #[allow(deprecated)]
    let force_reconcile = request.force_reconcile || request.reboot;

    log_machine_id(&host_machine_id);

    set_primary_interface_and_enqueue_reconciliation(
        api,
        host_machine_id,
        PrimaryInterfaceSelector::Dpu(dpu_machine_id),
        force_reconcile,
    )
    .await
}

/// Make any host interface -- DPU or not -- the primary (boot) interface,
/// identified directly by its machine-interface id. This is the generic form of
/// [`set_primary_dpu`]; unlike that alias it also works on zero-DPU hosts.
pub(crate) async fn set_primary_interface(
    api: &Api,
    request: Request<rpc::SetPrimaryInterfaceRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let request = request.into_inner();
    let host_machine_id = request
        .host_machine_id
        .ok_or_else(|| CarbideError::InvalidArgument("host machine ID is required".to_string()))?;
    let interface_id = request
        .interface_id
        .ok_or_else(|| CarbideError::InvalidArgument("interface ID is required".to_string()))?;
    // `reboot` is only a compatibility alias for `force_reconcile`.
    #[allow(deprecated)]
    let force_reconcile = request.force_reconcile || request.reboot;

    log_machine_id(&host_machine_id);

    set_primary_interface_and_enqueue_reconciliation(
        api,
        host_machine_id,
        PrimaryInterfaceSelector::Interface(interface_id),
        force_reconcile,
    )
    .await
}

/// Updates the primary interface, then wakes the controller after the transaction commits when
/// boot reconciliation is needed.
async fn set_primary_interface_and_enqueue_reconciliation(
    api: &Api,
    host_machine_id: StableHostMachineId,
    selector: PrimaryInterfaceSelector,
    force_reconcile: bool,
) -> Result<Response<()>, Status> {
    let update = update_primary_interface(api, host_machine_id, selector, force_reconcile).await?;
    enqueue_boot_interface_reconciliation(
        api,
        host_machine_id.into(),
        update.reconciliation_needed,
    )
    .await;

    Ok(Response::new(()))
}

/// Maintenance mode: Put a machine into maintenance mode or take it out.
///
/// Switching a host into maintenance mode prevents an instance being assigned
/// to it and suppresses external alerting on the host. It also excludes the
/// host from state-machine SLA tracking so that machines being worked on by an
/// operator do not page on-call for time-in-state breaches (e.g. stuck-instance
/// alerts) regardless of which state or substate they happen to be in.
pub(crate) async fn set_maintenance(
    api: &Api,
    request: Request<rpc::MaintenanceRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);
    let triggered_by = request
        .extensions()
        .get::<AuthContext>()
        .and_then(|ctx| ctx.get_external_user_name())
        .map(String::from);
    let req = request.into_inner();
    let machine_id: HostMachineId = convert_and_log_machine_id(req.host_id.as_ref())?;

    let (_, mut txn) = api
        .load_machine(&machine_id, MachineSearchConfig::default())
        .await?;
    let dpu_machines = db::machine::find_dpus_by_host_machine_id(&mut txn, &machine_id).await?;
    txn.commit().await?;

    // We set status on both host and dpu machine to make them easier to query from DB
    match req.operation() {
        rpc::MaintenanceOperation::Enable => {
            let Some(reference) = req.reference else {
                return Err(
                    CarbideError::InvalidArgument("missing reference url".to_string()).into(),
                );
            };

            let reference = reference.trim().to_string();
            if reference.len() < 5 {
                return Err(CarbideError::InvalidArgument(
                    "provide some valid reference. minimum expected length is 5".into(),
                )
                .into());
            }

            // Maintenance mode is implemented as a host health override
            crate::handlers::health::insert_machine_health_report(
                api,
                Request::new(rpc::InsertMachineHealthReportRequest {
                    machine_id: req.host_id,
                    health_report_entry: Some(::rpc::forge::HealthReportEntry {
                        report: Some(health_report::HealthReport {
                            source: "maintenance".to_string(),
                            triggered_by,
                            observed_at: Some(chrono::Utc::now()),
                            successes: Vec::new(),
                            alerts: vec![health_report::HealthProbeAlert {
                                id: "Maintenance".parse().unwrap(),
                                target: None,
                                in_alert_since: Some(chrono::Utc::now()),
                                message: reference.clone(),
                                tenant_message: None,
                                classifications: vec![
                                    health_report::HealthAlertClassification::prevent_allocations(),
                                    health_report::HealthAlertClassification::suppress_external_alerting(),
                                    health_report::HealthAlertClassification::exclude_from_state_machine_sla(),
                                ],
                            }],
                        }
                                     .into()),
                        mode: ::rpc::forge::HealthReportApplyMode::Merge.into(),
                    }),
                }),
            )
                .await?;
        }
        rpc::MaintenanceOperation::Disable => {
            for dpu_machine in dpu_machines.iter() {
                if dpu_machine.reprovision_requested.is_some() {
                    return Err(CarbideError::InvalidArgument(format!(
                        "reprovisioning request is set on DPU: {}. clear it first",
                        dpu_machine.id
                    ))
                    .into());
                }
            }

            match crate::handlers::health::remove_machine_health_report(
                api,
                Request::new(rpc::RemoveMachineHealthReportRequest {
                    machine_id: req.host_id,
                    source: "maintenance".to_string(),
                }),
            )
            .await
            {
                Ok(_) => (),
                Err(status) if status.code() == tonic::Code::NotFound => (),
                Err(status) => return Err(status),
            };
        }
    };

    Ok(Response::new(()))
}
