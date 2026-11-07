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

Delete a rack by ID:
    $ nico-admin-cli rack delete 12345678-1234-5678-90ab-cdef01234567

Delete a rack by name:
    $ nico-admin-cli rack delete rack-01

")]
pub(crate) struct Args {
    #[clap(
        help = "Rack ID or name to delete (should not have any associated compute trays, nvlink switches or power shelves)"
    )]
    identifier: String,
}

impl From<Args> for ::rpc::forge::DeleteRackRequest {
    fn from(args: Args) -> Self {
        Self {
            id: args.identifier,
        }
    }
}
