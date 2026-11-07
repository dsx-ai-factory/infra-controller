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

use carbide_uuid::machine::MachineInterfaceId;
use clap::Parser;

use crate::boot_override::common::BootOverride;

// Args wraps the shared BootOverride as a subcommand
// specific newtype to allow sharing of BootOverride, and still
// providing a subcommand-specific Run trait implementation.
#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Show the boot override for a machine interface:
    $ nico-admin-cli boot-override get 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct Args {
    #[clap(flatten)]
    inner: BootOverride,
}

impl From<Args> for MachineInterfaceId {
    fn from(args: Args) -> Self {
        args.inner.interface_id
    }
}
