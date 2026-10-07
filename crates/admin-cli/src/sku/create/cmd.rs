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
use ::rpc::forge::SkuList;

use super::args::Args;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;
use crate::sku::show::cmd::show_skus_table;

pub(super) async fn create(
    args: Args,
    api_client: &ApiClient,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: &OutputFormat,
) -> CarbideCliResult<()> {
    let file_data = std::fs::read_to_string(args.filename)?;
    // attempt to deserialize a single sku.  if it fails try to deserialize as a SkuList
    let mut sku_list = match serde_json::de::from_str(&file_data) {
        Ok(sku) => SkuList { skus: vec![sku] },
        Err(e) => serde_json::de::from_str(&file_data).map_err(|_| e)?,
    };
    if let Some(id) = args.id {
        if sku_list.skus.len() != 1 {
            return Err(CarbideCliError::GenericError(
                "ID cannot be specified when creating multiple SKUs".to_string(),
            ));
        }
        sku_list.skus[0].id = id;
    }
    let sku_ids = api_client.0.create_sku(sku_list).await?;
    let sku_list = api_client.0.find_skus_by_ids(sku_ids.ids).await?;
    show_skus_table(output, output_format, sku_list.skus).await?;
    Ok(())
}
