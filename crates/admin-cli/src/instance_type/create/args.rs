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
use rpc::forge::{self as forgerpc, CreateInstanceTypeRequest, InstanceTypeAttributes};

use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Create an instance type with a name and description:
    $ nico-admin-cli instance-type create --name dgx-h100 \
    --description \"DGX H100 640GB\"

Create with labels and desired-capability filters:
    $ nico-admin-cli instance-type create --name dgx-h100 \
    --labels '{\"tier\":\"premium\"}' \
    --desired-capabilities '[{\"key\":\"gpu_count\",\"value\":\"8\"}]'

Create with an explicit id:
    $ nico-admin-cli instance-type create --id 12345678-1234-5678-90ab-cdef01234567 \
    --name dgx-h100

")]
pub(crate) struct Args {
    #[clap(
        short = 'i',
        long,
        help = "Optional, unique ID to use when creating the instance type"
    )]
    id: Option<String>,

    #[clap(short = 'n', long, help = "Name of the instance type")]
    name: Option<String>,

    #[clap(short = 'd', long, help = "Description of the instance type")]
    description: Option<String>,

    #[clap(
        short = 'l',
        long,
        help = "JSON map of simple key:value pairs to be applied as labels to the instance type"
    )]
    labels: Option<String>,

    #[clap(
        short = 'f',
        long,
        help = "Optional, JSON array containing a set of instance type capability filters"
    )]
    desired_capabilities: Option<String>,
}

impl TryFrom<Args> for CreateInstanceTypeRequest {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        let labels = if let Some(l) = args.labels {
            serde_json::from_str(&l)?
        } else {
            vec![]
        };

        let metadata = forgerpc::Metadata {
            name: args.name.unwrap_or_default(),
            description: args.description.unwrap_or_default(),
            labels,
        };

        let instance_type_attributes = args
            .desired_capabilities
            .map(|d| {
                serde_json::from_str(&d).map(|desired_capabilities| InstanceTypeAttributes {
                    desired_capabilities,
                })
            })
            .transpose()?;

        Ok(CreateInstanceTypeRequest {
            id: args.id,
            metadata: Some(metadata),
            instance_type_attributes,
        })
    }
}
