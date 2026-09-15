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

use ::rpc::admin_cli::OutputFormat;
use carbide_uuid::machine::HostMachineId;
use prettytable::row;

use crate::dpf::common::DpfQuery;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn show(
    query: &DpfQuery,
    _format: OutputFormat,
    page_size: usize,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let machine_ids = if let Some(host) = query.host {
        vec![host]
    } else {
        api_client
            .0
            .find_machine_ids(::rpc::forge::MachineSearchConfig {
                include_dpus: false,
                include_predicted_host: true,
                ..Default::default()
            })
            .await?
            .machine_ids
            .into_iter()
            .filter_map(|id| HostMachineId::try_from(id).ok())
            .collect()
    };

    let response = api_client.get_dpf_state(machine_ids, page_size).await?;
    if response.is_empty() {
        println!("No DPF state found for machines");
        return Ok(());
    }

    if response.len() == 1 {
        println!(
            "DPF status for machine {}:",
            response[0].machine_id.unwrap_or_default(),
        );
        println!("\tEnabled            : {}", response[0].enabled);
        println!("\tUsed For Ingestion : {}", response[0].used_for_ingestion);
    } else {
        let mut table = prettytable::Table::new();
        table.set_titles(row!["Id", "Enabled", "Used For Ingestion"]);

        for dpf_state in response {
            table.add_row(row![
                dpf_state.machine_id.unwrap_or_default().to_string(),
                dpf_state.enabled.to_string(),
                dpf_state.used_for_ingestion.to_string(),
            ]);
        }
        table.printstd();
    }

    Ok(())
}
