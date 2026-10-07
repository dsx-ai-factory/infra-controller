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
use ::rpc::forge as rpc;
use prettytable::{Cell, Row, Table};

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn get(format: OutputFormat, api_client: &ApiClient) -> CarbideCliResult<()> {
    let route_servers = api_client.0.get_route_servers().await?;

    match format {
        OutputFormat::AsciiTable => {
            let table = route_servers_to_table(&route_servers)?;
            table.printstd();
        }
        OutputFormat::Csv => {
            println!("address,source_type");
            for route_server in &route_servers.route_servers {
                println!("{},{:?}", route_server.address, route_server.source_type)
            }
        }
        OutputFormat::Json => {
            println!("{}", serde_json::to_string(&route_servers)?)
        }
        OutputFormat::Yaml => {
            println!("{}", serde_yaml::to_string(&route_servers)?)
        }
    }

    Ok(())
}

// route_servers_to_table converts the RouteServerEntries
// response into a pretty ASCII table.
fn route_servers_to_table(
    route_server_entries: &rpc::RouteServerEntries,
) -> CarbideCliResult<Table> {
    let mut table = Table::new();

    table.add_row(Row::new(vec![
        Cell::new("Address"),
        Cell::new("Source Type"),
    ]));

    for route_server in &route_server_entries.route_servers {
        let source_type = rpc::RouteServerSourceType::try_from(route_server.source_type)
            .map_err(|e| e.to_string())
            .map_err(CarbideCliError::GenericError)?;

        table.add_row(Row::new(vec![
            Cell::new(&route_server.address),
            Cell::new(format!("{source_type:?}").as_str()),
        ]));
    }

    Ok(table)
}
