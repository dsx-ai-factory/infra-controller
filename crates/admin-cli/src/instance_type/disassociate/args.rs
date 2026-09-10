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
use rpc::forge::RemoveMachineInstanceTypeAssociationRequest;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Remove a machine's instance-type association:
    $ nico-admin-cli instance-type disassociate fm100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0

")]
pub(crate) struct Args {
    #[clap(help = "Machine Id")]
    pub(super) machine_id: MachineId,
}

impl From<&Args> for RemoveMachineInstanceTypeAssociationRequest {
    fn from(args: &Args) -> Self {
        Self {
            machine_id: args.machine_id.to_string(),
        }
    }
}
