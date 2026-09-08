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

use crate::component_manager::common::{
    ComputeTraySelection, PowerActionArg, PowerControlTargetArgs,
};

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Power on a switch:
    $ nico-admin-cli component-manager component-power-control switch \
    --switch-id 12345678-1234-5678-90ab-cdef01234567 --action on

Force off a compute tray:
    $ nico-admin-cli component-manager component-power-control compute-tray \
    --machine-id 12345678-1234-5678-90ab-cdef01234567 --action force-off

Force off a compute tray by BMC MAC (targets the tray before ingestion):
    $ nico-admin-cli component-manager component-power-control compute-tray \
    --mac-address 00:11:22:33:44:55 --action force-off

AC power-cycle a power shelf:
    $ nico-admin-cli component-manager component-power-control power-shelf \
    --power-shelf-id 12345678-1234-5678-90ab-cdef01234567 --action ac-powercycle

")]
pub(crate) struct Args {
    #[clap(subcommand)]
    target: PowerControlTargetArgs,

    #[clap(
        long = "action",
        value_enum,
        help = "Power control action to apply to the targeted components"
    )]
    action: PowerActionArg,

    #[clap(
        long = "bypass-state-controller",
        help = "Bypass the state controller and dispatch directly to the component backend"
    )]
    bypass_state_controller: bool,
}

impl From<Args> for rpc::forge::ComponentPowerControlRequest {
    fn from(args: Args) -> Self {
        let action = ::rpc::common::SystemPowerControl::from(args.action) as i32;
        let bypass_state_controller = args.bypass_state_controller;
        match args.target {
            PowerControlTargetArgs::Switch(target) => Self {
                target: Some(
                    rpc::forge::component_power_control_request::Target::SwitchIds(target.into()),
                ),
                action,
                bypass_state_controller,
            },
            PowerControlTargetArgs::PowerShelf(target) => Self {
                target: Some(
                    rpc::forge::component_power_control_request::Target::PowerShelfIds(
                        target.into(),
                    ),
                ),
                action,
                bypass_state_controller,
            },
            PowerControlTargetArgs::ComputeTray(target) => {
                let target = match target.into_selection() {
                    ComputeTraySelection::MachineIds(list) => {
                        rpc::forge::component_power_control_request::Target::MachineIds(list)
                    }
                    ComputeTraySelection::Macs(macs) => {
                        rpc::forge::component_power_control_request::Target::ComputeBmcMacs(macs)
                    }
                };
                Self {
                    target: Some(target),
                    action,
                    bypass_state_controller,
                }
            }
        }
    }
}
