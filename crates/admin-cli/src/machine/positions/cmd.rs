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

use ::rpc::forge as forgerpc;
use prettytable::{Row, Table, row};

use super::args::Args;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

pub(super) async fn positions(args: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let machine_ids = if args.machine.is_empty() {
        // Query all machines if none specified
        api_client
            .0
            .find_machine_ids(forgerpc::MachineSearchConfig {
                include_dpus: true,
                include_predicted_host: true,
                ..Default::default()
            })
            .await?
            .machine_ids
    } else {
        args.machine
    };

    let req = forgerpc::MachinePositionQuery { machine_ids };
    let info = api_client.0.get_machine_position_info(req).await?;
    let mut table = Table::new();
    table.set_titles(Row::from(vec![
        "Machine ID",
        "Physical Slot",
        "Compute Tray",
        "Topology",
        "Revision",
        "Switch",
        "Power Shelf",
    ]));
    for x in info.machine_position_info {
        table.add_row(row![
            x.machine_id.unwrap_or_default(),
            x.physical_slot_number
                .map(|x| x.to_string())
                .unwrap_or("---".to_string()),
            x.compute_tray_index
                .map(|x| x.to_string())
                .unwrap_or("---".to_string()),
            x.topology_id
                .map(|x| x.to_string())
                .unwrap_or("---".to_string()),
            x.revision_id
                .map(|x| x.to_string())
                .unwrap_or("---".to_string()),
            x.switch_id
                .map(|id| id.to_string())
                .unwrap_or("---".to_string()),
            x.power_shelf_id
                .map(|id| id.to_string())
                .unwrap_or("---".to_string()),
        ]);
    }
    table.printstd();

    Ok(())
}
