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

mod create;
mod delete;
mod show;

use clap::Parser;
use prettytable::{Table, row};
use rpc::forge::VpcPeering;

use crate::cfg::dispatch::Dispatch;
use crate::errors::CarbideCliResult;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Create VPC peering.")]
    Create(create::Args),
    #[clap(about = "Show list of VPC peerings.")]
    Show(show::Args),
    #[clap(about = "Delete VPC peering.")]
    Delete(delete::Args),
}

fn convert_vpc_peerings_to_table(vpc_peerings: &[VpcPeering]) -> CarbideCliResult<Box<Table>> {
    let mut table = Box::new(Table::new());

    table.set_titles(row!["Id", "VPC1 ID", "VPC2 ID"]);

    for vpc_peering in vpc_peerings {
        let id = vpc_peering.id.map(|id| id.to_string()).unwrap_or_default();
        let vpc_id = vpc_peering
            .vpc_id
            .as_ref()
            .map(|uuid| uuid.to_string())
            .unwrap_or("None".to_string());
        let peer_vpc_id = vpc_peering
            .peer_vpc_id
            .as_ref()
            .map(|uuid| uuid.to_string())
            .unwrap_or("None".to_string());

        table.add_row(row![id, vpc_id, peer_vpc_id]);
    }

    Ok(table)
}
