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

use clap::{ArgGroup, Parser};

#[derive(Parser, Debug)]
#[clap(group(ArgGroup::new("group").required(true).multiple(true).args(&["description", "device_type"])))]
#[command(after_long_help = "\
EXAMPLES:

Update a SKU's description:
    $ nico-admin-cli sku update-metadata DGX-H100-640GB --description \"DGX H100 640GB\"

Update a SKU's device type:
    $ nico-admin-cli sku update-metadata DGX-H100-640GB --device-type gpu-server

Update both at once:
    $ nico-admin-cli sku update-metadata DGX-H100-640GB \
    --description \"DGX H100 640GB\" --device-type gpu-server

")]
pub(crate) struct Args {
    #[clap(help = "SKU ID of the SKU to update")]
    pub(in crate::sku) sku_id: String,
    #[clap(help = "Update the SKU's description", long, group("group"))]
    pub(in crate::sku) description: Option<String>,
    #[clap(help = "Update the SKU's device type", long, group("group"))]
    pub(in crate::sku) device_type: Option<String>,
}

impl From<Args> for ::rpc::forge::SkuUpdateMetadataRequest {
    fn from(value: Args) -> Self {
        ::rpc::forge::SkuUpdateMetadataRequest {
            sku_id: value.sku_id,
            description: value.description,
            device_type: value.device_type,
        }
    }
}
