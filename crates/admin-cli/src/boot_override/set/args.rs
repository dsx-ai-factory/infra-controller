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

use ::rpc::forge::MachineBootOverride;
use carbide_uuid::machine::MachineInterfaceId;
use clap::Parser;

use crate::errors::{CarbideCliError, CarbideCliResult};

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set a custom iPXE script for a machine interface:
    $ nico-admin-cli boot-override set 12345678-1234-5678-90ab-cdef01234567 \
    --custom-pxe ./boot.ipxe

Set custom user-data for a machine interface:
    $ nico-admin-cli boot-override set 12345678-1234-5678-90ab-cdef01234567 \
    --custom-user-data ./user-data.yaml

Set both a custom iPXE script and custom user-data:
    $ nico-admin-cli boot-override set 12345678-1234-5678-90ab-cdef01234567 \
    --custom-pxe ./boot.ipxe --custom-user-data ./user-data.yaml

")]
pub(crate) struct Args {
    interface_id: MachineInterfaceId,
    #[clap(short = 'p', long)]
    custom_pxe: Option<String>,
    #[clap(short = 'u', long)]
    custom_user_data: Option<String>,
}

impl TryFrom<Args> for MachineBootOverride {
    type Error = CarbideCliError;

    fn try_from(args: Args) -> CarbideCliResult<Self> {
        if args.custom_pxe.is_none() && args.custom_user_data.is_none() {
            return Err(CarbideCliError::GenericError(
                "Either custom pxe or custom user data is required".to_owned(),
            ));
        }

        let custom_pxe = args.custom_pxe.map(std::fs::read_to_string).transpose()?;
        let custom_user_data = args
            .custom_user_data
            .map(std::fs::read_to_string)
            .transpose()?;

        Ok(MachineBootOverride {
            machine_interface_id: Some(args.interface_id),
            custom_pxe,
            custom_user_data,
        })
    }
}
