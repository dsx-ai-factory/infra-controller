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

use carbide_uuid::device::DeviceId;
use carbide_uuid::machine::MachineId;
use carbide_uuid::power_shelf::PowerShelfId;
use carbide_uuid::switch::SwitchId;
use clap::{ArgGroup, Parser};
use rpc::forge as forgerpc;

use crate::bmc_machine::common::ResetTypeArg;

#[derive(Parser, Debug, Clone)]
#[command(
    long_about = "Reset a BMC.\n\nExactly one target must be specified: --machine, --switch, or --power-shelf. Providing more than one target is rejected.",
    after_long_help = "\
EXAMPLES:

Reset the BMC of a machine via Redfish:
    $ nico-admin-cli bmc-machine bmc-reset --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Reset a switch BMC, forcing a ForceRestart:
    $ nico-admin-cli bmc-machine bmc-reset --switch sw100nt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --reset-type force

Reset a power shelf PMC:
    $ nico-admin-cli bmc-machine bmc-reset --power-shelf ps100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Reset a machine BMC using ipmitool instead of Redfish:
    $ nico-admin-cli bmc-machine bmc-reset --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --use-ipmitool

"
)]
#[clap(group(
    ArgGroup::new("target")
        .required(true)
        .multiple(false)
        .args(["machine", "switch", "power_shelf"])
))]
pub(crate) struct Args {
    #[clap(long, group = "target", help = "ID of the machine whose BMC to reset")]
    machine: Option<MachineId>,

    #[clap(long, group = "target", help = "ID of the switch whose BMC to reset")]
    switch: Option<SwitchId>,

    #[clap(
        long = "power-shelf",
        group = "target",
        help = "ID of the power shelf whose PMC to reset"
    )]
    power_shelf: Option<PowerShelfId>,

    #[clap(
        long = "reset-type",
        value_enum,
        help = "Redfish Manager.Reset type. Omit for the vendor default. Ignored with --use-ipmitool."
    )]
    reset_type: Option<ResetTypeArg>,

    #[clap(
        short,
        long,
        help = "Use ipmitool instead of Redfish to reset the BMC. ipmitool bmc reset requests may be silently ignored if the BMC is in lockdown mode."
    )]
    pub(super) use_ipmitool: bool,
}

impl From<Args> for forgerpc::AdminBmcResetRequest {
    fn from(args: Args) -> Self {
        let device_id = if let Some(machine) = args.machine {
            DeviceId::Machine(machine)
        } else if let Some(switch) = args.switch {
            DeviceId::Switch(switch)
        } else if let Some(power_shelf) = args.power_shelf {
            DeviceId::PowerShelf(power_shelf)
        } else {
            unreachable!("clap ArgGroup requires exactly one target");
        };

        Self {
            bmc_endpoint_request: None,
            use_ipmitool: args.use_ipmitool,
            device_id: Some(device_id),
            reset_type: args
                .reset_type
                .map(forgerpc::admin_bmc_reset_request::ResetType::from)
                .unwrap_or(forgerpc::admin_bmc_reset_request::ResetType::Unspecified)
                as i32,
            ..Default::default()
        }
    }
}
