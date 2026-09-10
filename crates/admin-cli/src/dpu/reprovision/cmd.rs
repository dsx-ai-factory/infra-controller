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

use ::rpc::forge::DpuReprovisioningRequest;
use carbide_uuid::machine::MachineType;
use prettytable::{Table, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::machine::{HealthReportTemplates, get_health_report};
use crate::rpc::ApiClient;

pub(super) async fn reprovision(api_client: &ApiClient, reprov: Args) -> CarbideCliResult<()> {
    match reprov {
        Args::Set(data) => {
            if let Some(update_message) = &data.update_message {
                apply_health_report(api_client, data.id, update_message.clone()).await?;
            }
            let req: DpuReprovisioningRequest = (&data).into();
            api_client.0.trigger_dpu_reprovisioning(req).await?;
            Ok(())
        }
        Args::Clear(data) => {
            let req: DpuReprovisioningRequest = (&data).into();
            api_client.0.trigger_dpu_reprovisioning(req).await?;
            Ok(())
        }
        Args::List => list_dpus_pending(api_client).await,
        Args::Restart(data) => {
            let req: DpuReprovisioningRequest = (&data).into();
            api_client.0.trigger_dpu_reprovisioning(req).await?;
            Ok(())
        }
    }
}

#[allow(deprecated)]
async fn apply_health_report(
    api_client: &ApiClient,
    id: carbide_uuid::machine::MachineId,
    update_message: String,
) -> CarbideCliResult<()> {
    // Set a HostUpdateInProgress health report entry on the Host
    let host_id: Option<carbide_uuid::machine::MachineId> = match id.machine_type() {
        MachineType::Host => Some(id),
        MachineType::Dpu => {
            let machine = api_client
                .get_machines_by_ids(&[id])
                .await?
                .machines
                .into_iter()
                .next();

            if let Some(host_id) = machine.map(|x| x.associated_host_machine_id) {
                host_id.map(Into::into)
            } else {
                return Err(CarbideCliError::GenericError(format!(
                    "Could not find host attached with dpu {id}",
                )));
            }
        }
        _ => {
            return Err(CarbideCliError::GenericError(format!(
                "Invalid machine ID for reprovisioning, only Hosts and DPUs are supported: {id}"
            )));
        }
    };

    // Check host must not already have a host-update health report entry
    if let Some(host_machine_id) = &host_id {
        let host_machine = api_client
            .get_machines_by_ids(&[*host_machine_id])
            .await?
            .machines
            .into_iter()
            .next();

        if let Some(host_machine) = host_machine
            && host_machine
                .health_sources
                .iter()
                .any(|or| or.source == "host-update")
        {
            return Err(CarbideCliError::GenericError(format!(
                "Host machine: {:?} already has a \"host-update\" health report entry.",
                host_machine.id,
            )));
        }

        let report = get_health_report(HealthReportTemplates::HostUpdate, Some(update_message));

        api_client
            .machine_insert_health_report_override(host_machine_id, report.into(), false)
            .await?;
    }

    Ok(())
}

async fn list_dpus_pending(api_client: &ApiClient) -> CarbideCliResult<()> {
    let response = api_client.0.list_dpu_waiting_for_reprovisioning().await?;
    print_pending_dpus(response);
    Ok(())
}

fn print_pending_dpus(dpus: ::rpc::forge::DpuReprovisioningListResponse) {
    let mut table = Table::new();

    table.set_titles(row![
        "Id",
        "State",
        "Initiator",
        "Requested At",
        "Initiated At",
        "Update Firmware",
        "User Approved"
    ]);

    for dpu in dpus.dpus {
        let user_approval = if dpu.user_approval_received {
            "Yes"
        } else if dpu.state.contains("Assigned") {
            "No"
        } else {
            "NA"
        };
        table.add_row(row![
            dpu.id.unwrap_or_default().to_string(),
            dpu.state,
            dpu.initiator,
            dpu.requested_at.unwrap_or_default(),
            dpu.initiated_at
                .map(|x| x.to_string())
                .unwrap_or_else(|| "Not Started".to_string()),
            dpu.update_firmware,
            user_approval
        ]);
    }

    table.printstd();
}
