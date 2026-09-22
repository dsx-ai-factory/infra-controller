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

use ::rpc::forge::HostReprovisioningRequest;
use carbide_uuid::machine::StableHostMachineId;
use prettytable::{Table, row};

use super::args::{ReprovisionClear, ReprovisionSet};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::machine::{HealthReportTemplates, get_health_report};
use crate::rpc::ApiClient;

#[allow(deprecated)]
pub(super) async fn trigger_reprovisioning_set(
    data: ReprovisionSet,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    if let Some(update_message) = data.update_message.clone() {
        // Set a HostUpdateInProgress health report entry on the Host

        let host_machine = api_client
            .get_machines_by_ids(&[data.id])
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
            .machine_insert_health_report_override(data.id.as_machine_id(), report.into(), false)
            .await?;
    }

    let req: HostReprovisioningRequest = (&data).into();
    api_client.0.trigger_host_reprovisioning(req).await?;

    Ok(())
}

pub(super) async fn trigger_reprovisioning_clear(
    data: ReprovisionClear,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    api_client.0.trigger_host_reprovisioning(data).await?;
    Ok(())
}

pub(super) async fn list_hosts_pending(api_client: &ApiClient) -> CarbideCliResult<()> {
    let response = api_client.0.list_hosts_waiting_for_reprovisioning().await?;
    print_pending_hosts(response);
    Ok(())
}

pub(super) async fn mark_manual_firmware_upgrade_complete(
    machine_id: StableHostMachineId,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    api_client
        .0
        .mark_manual_firmware_upgrade_complete(machine_id)
        .await?;

    println!("Marked manual firmware upgrade as complete for machine {machine_id}",);

    Ok(())
}

fn print_pending_hosts(hosts: ::rpc::forge::HostReprovisioningListResponse) {
    let mut table = Table::new();

    table.set_titles(row![
        "Id",
        "State",
        "Initiator",
        "Requested At",
        "Initiated At",
        "User Approved"
    ]);

    for host in hosts.hosts {
        let user_approval = if host.user_approval_received {
            "Yes"
        } else if host.state.contains("Assigned") {
            "No"
        } else {
            "NA"
        };
        table.add_row(row![
            host.id.unwrap_or_default().to_string(),
            host.state,
            host.initiator,
            host.requested_at.unwrap_or_default(),
            host.initiated_at
                .map(|x| x.to_string())
                .unwrap_or_else(|| "Not Started".to_string()),
            user_approval
        ]);
    }

    table.printstd();
}
