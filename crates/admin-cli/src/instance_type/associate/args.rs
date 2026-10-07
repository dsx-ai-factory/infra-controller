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
use rpc::forge::AssociateMachinesWithInstanceTypeRequest;

use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Associate one machine with an instance type:
    $ nico-admin-cli instance-type associate 12345678-1234-5678-90ab-cdef01234567 \
    abcdef01-2345-6789-abcd-ef0123456789

Associate several machines (comma-separated, no spaces):
    $ nico-admin-cli instance-type associate 12345678-1234-5678-90ab-cdef01234567 \
    abcdef01-2345-6789-abcd-ef0123456789,11111111-2222-3333-4444-555555555555

")]
pub(crate) struct Args {
    #[clap(help = "InstanceTypeId")]
    instance_type_id: String,
    #[clap(help = "Machine Ids, separated by comma", value_delimiter = ',')]
    machine_ids: Vec<String>,
}

impl TryFrom<Args> for AssociateMachinesWithInstanceTypeRequest {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        if args.machine_ids.is_empty() {
            return Err(CarbideCliError::GenericError(
                "Machine ids can not be empty.".to_string(),
            ));
        }

        Ok(AssociateMachinesWithInstanceTypeRequest {
            instance_type_id: args.instance_type_id,
            machine_ids: args.machine_ids,
        })
    }
}
