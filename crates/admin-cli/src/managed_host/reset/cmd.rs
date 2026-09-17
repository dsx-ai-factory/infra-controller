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

use ::rpc::forge::ManagedHostResetRequest;
use model::machine_update_module::HOST_UPDATE_HEALTH_REPORT_SOURCE;
use prettytable::{Table, row};

use super::args::{ResetClear, ResetSet};
use crate::errors::CarbideCliResult;
use crate::machine::{HealthReportTemplates, get_health_report};
use crate::rpc::ApiClient;

pub(super) async fn reset_set(data: ResetSet, api_client: &ApiClient) -> CarbideCliResult<()> {
    if let Some(update_message) = data.update_message.clone() {
        // A reset may be re-requested on a host that is already resetting, and
        // that host still carries the alert from the earlier attempt. Only add
        // the alert when it is absent so re-requesting stays possible.
        let already_alerted = api_client
            .get_machines_by_ids(&[data.machine])
            .await?
            .machines
            .into_iter()
            .next()
            .and_then(|machine| machine.status)
            .is_some_and(|status| {
                status
                    .health_sources
                    .iter()
                    .any(|source| source.source == HOST_UPDATE_HEALTH_REPORT_SOURCE)
            });

        if !already_alerted {
            let report = get_health_report(HealthReportTemplates::HostUpdate, Some(update_message));

            api_client
                .machine_insert_health_report_override(&data.machine, report.into(), false)
                .await?;
        }
    }

    let req: ManagedHostResetRequest = (&data).into();
    api_client.0.trigger_managed_host_reset(req).await?;

    Ok(())
}

pub(super) async fn reset_clear(data: ResetClear, api_client: &ApiClient) -> CarbideCliResult<()> {
    api_client.0.trigger_managed_host_reset(data).await?;
    Ok(())
}

pub(super) async fn list_pending_resets(api_client: &ApiClient) -> CarbideCliResult<()> {
    let response = api_client.0.list_managed_hosts_waiting_for_reset().await?;
    pending_resets_table(response).printstd();
    Ok(())
}

fn pending_resets_table(hosts: ::rpc::forge::ManagedHostResetListResponse) -> Table {
    let mut table = Table::new();

    table.set_titles(row![
        "Id",
        "State",
        "Initiator",
        "Requested At",
        "Started At"
    ]);

    for host in hosts.hosts {
        table.add_row(row![
            host.id.unwrap_or_default().to_string(),
            host.state,
            host.initiator,
            host.requested_at.unwrap_or_default(),
            host.started_at
                .map(|x| x.to_string())
                .unwrap_or_else(|| "Not Started".to_string())
        ]);
    }

    table
}

#[cfg(test)]
mod tests {
    use ::rpc::forge::ManagedHostResetListResponse;
    use ::rpc::forge::managed_host_reset_list_response::ManagedHostResetListItem;
    use carbide_uuid::machine::MachineId;

    use super::*;

    const TEST_MACHINE_ID: &str = "fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";

    /// `Started At` is how an operator tells an unpicked-up request from one already running,
    /// so an absent value has to read as "Not Started", not as a blank or an epoch timestamp.
    #[test]
    fn pending_resets_table_names_its_columns_and_labels_an_unstarted_reset() {
        let machine_id: MachineId = TEST_MACHINE_ID.parse().unwrap();
        let response = ManagedHostResetListResponse {
            hosts: vec![
                ManagedHostResetListItem {
                    id: Some(machine_id),
                    state: "Reset/DeletingCrs".to_string(),
                    initiator: "UPDATE_INITIATOR_ADMIN_CLI".to_string(),
                    // The default Timestamp is the unix epoch; Display renders RFC 3339.
                    requested_at: Some(Default::default()),
                    started_at: Some(Default::default()),
                },
                ManagedHostResetListItem {
                    id: Some(machine_id),
                    state: "Assigned/Ready".to_string(),
                    initiator: "UPDATE_INITIATOR_ADMIN_CLI".to_string(),
                    requested_at: Some(Default::default()),
                    started_at: None,
                },
            ],
        };

        let table = pending_resets_table(response);

        let rendered = table.to_string();
        for header in ["Id", "State", "Initiator", "Requested At", "Started At"] {
            assert!(rendered.contains(header), "missing {header} column");
        }

        let started = table.get_row(0).expect("the started host should render");
        assert_eq!(started.get_cell(0).unwrap().get_content(), TEST_MACHINE_ID);
        assert_eq!(
            started.get_cell(1).unwrap().get_content(),
            "Reset/DeletingCrs"
        );
        assert_eq!(
            started.get_cell(4).unwrap().get_content(),
            "1970-01-01T00:00:00Z"
        );

        let unstarted = table.get_row(1).expect("the unstarted host should render");
        assert_eq!(
            unstarted.get_cell(4).unwrap().get_content(),
            "Not Started",
            "an absent start time has to name the condition, not render empty"
        );
    }
}
