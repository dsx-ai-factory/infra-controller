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

use ::rpc::forge::ClearHostUefiPasswordRequest;
use clap::Parser;

use crate::machine::MachineQuery;

// Args wraps the shared MachineQuery as a subcommand
// specific newtype to allow sharing of MachineQuery, and still
// providing a subcommand-specific Run trait implementation.
#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Clear the UEFI password for a host by machine ID:
    $ nico-admin-cli host clear-uefi-password --query 12345678-1234-5678-90ab-cdef01234567

Clear the UEFI password for a host selected by MAC address:
    $ nico-admin-cli host clear-uefi-password --query 00:11:22:33:44:55

")]
pub(crate) struct Args {
    #[clap(flatten)]
    inner: MachineQuery,
}

impl From<Args> for ClearHostUefiPasswordRequest {
    fn from(args: Args) -> Self {
        Self {
            host_id: None,
            machine_query: Some(args.inner.query),
        }
    }
}
