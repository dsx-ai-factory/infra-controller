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

use super::args::{Args, ShowQuery};
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

pub(super) async fn show(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
    _page_size: usize,
) -> CarbideCliResult<()> {
    let query: ShowQuery = args.try_into()?;
    let is_json = output_format == OutputFormat::Json;
    let images = match query {
        ShowQuery::Single(id) => vec![api_client.0.get_os_image(id).await?],
        ShowQuery::List(tenant_org_id) => api_client.list_os_image(tenant_org_id).await?,
    };
    if is_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&images).map_err(CarbideCliError::JsonError)?
        );
    } else {
        // todo: pretty print in table form
        println!("{images:?}");
    }
    Ok(())
}
