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

use super::super::common::CreateSkuOptions;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::sku::show::cmd::show_skus_table;

pub(super) async fn replace(
    args: CreateSkuOptions,
    api_client: &ApiClient,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: &OutputFormat,
) -> CarbideCliResult<()> {
    let file_data = std::fs::read_to_string(args.filename)?;
    let mut sku: rpc::forge::Sku = serde_json::de::from_str(&file_data)?;
    sku.id = args.id.unwrap_or(sku.id);

    let updated_sku = api_client.0.replace_sku(sku).await?;
    show_skus_table(output, output_format, vec![updated_sku]).await?;
    Ok(())
}
