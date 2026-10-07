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

use clap::Parser;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List all SKUs:
    $ nico-admin-cli sku show

Show details for one SKU:
    $ nico-admin-cli sku show DGX-H100-640GB

")]
pub(crate) struct ShowSkuOptions {
    #[clap(help = "Show SKU details")]
    pub(super) sku_id: Option<String>,
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Create SKUs from a file:
    $ nico-admin-cli sku create ./skus.json

Create from a file but override the SKU ID:
    $ nico-admin-cli sku create ./skus.json --id DGX-H100-640GB

")]
pub(crate) struct CreateSkuOptions {
    #[clap(help = "The filename of the SKU data")]
    pub(super) filename: String,
    #[clap(help = "override the ID of the SKU in the file data", long)]
    pub(super) id: Option<String>,
}
