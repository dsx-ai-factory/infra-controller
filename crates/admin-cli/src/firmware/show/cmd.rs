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
use prettytable::{Table, row};

use super::args::Args;
use crate::async_write;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn show(
    _args: &Args,
    format: OutputFormat,
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let resp = api_client.0.list_host_firmware().await?;
    match format {
        OutputFormat::AsciiTable => {
            let mut table = Box::new(Table::new());
            table.set_titles(row![
                "Vendor",
                "Model",
                "Type",
                "Inventory Name",
                "Version",
                "Needs Explicit Start"
            ]);
            for row in resp.available {
                table.add_row(row![
                    row.vendor,
                    row.model,
                    row.r#type,
                    row.inventory_name_regex,
                    row.version,
                    row.needs_explicit_start,
                ]);
            }
            async_write!(output_file, "{}", table)?;
        }
        _ => {
            return Err(CarbideCliError::NotImplemented(
                "Format option not implemented".to_string(),
            ));
        }
    }
    Ok(())
}
