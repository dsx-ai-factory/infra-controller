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

use carbide_uuid::machine::MachineId;
use clap::Parser;
use rpc::forge as forgerpc;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Check the lockdown status of a machine:
    $ nico-admin-cli bmc-machine lockdown-status \
    --machine 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(long, help = "ID of the machine to check lockdown status")]
    machine: MachineId,
}

impl From<Args> for forgerpc::LockdownStatusRequest {
    fn from(args: Args) -> Self {
        Self {
            bmc_endpoint_request: None,
            machine_id: Some(args.machine),
        }
    }
}
