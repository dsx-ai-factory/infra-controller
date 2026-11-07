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

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::instance_type::common::convert_itypes_to_table;
use crate::rpc::ApiClient;

/// Create an instance type.
/// On successful creation, the details of the
/// new type will be displayed.
pub(super) async fn create(
    args: Args,
    output_format: OutputFormat,
    api_client: &ApiClient,
) -> CarbideCliResult<()> {
    let is_json = output_format == OutputFormat::Json;

    let req: ::rpc::forge::CreateInstanceTypeRequest = args.try_into()?;
    let itype = api_client
        .0
        .create_instance_type(req)
        .await?
        .instance_type
        .ok_or(CarbideCliError::Empty)?;

    if is_json {
        println!(
            "{}",
            serde_json::to_string_pretty(&itype).map_err(CarbideCliError::JsonError)?
        );
    } else {
        convert_itypes_to_table(&[itype], true)?.printstd();
    }

    Ok(())
}
