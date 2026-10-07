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

use prettytable::{Table, row};

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(crate) async fn list(data: Args, api_client: &ApiClient) -> CarbideCliResult<()> {
    let response = api_client.0.admin_list_resource_pools(data).await?;
    if response.pools.is_empty() {
        println!("No resource pools defined");
        return Err(CarbideCliError::Empty);
    }

    let mut table = Table::new();
    table.set_titles(row!["Name", "Min", "Max", "Size", "Used"]);
    for pool in response.pools {
        table.add_row(row![
            pool.name,
            pool.min,
            pool.max,
            pool.total,
            format!(
                "{} ({:.0}%)",
                pool.allocated,
                pool.allocated as f64 / pool.total as f64 * 100.0
            ),
        ]);
    }
    table.printstd();
    Ok(())
}
