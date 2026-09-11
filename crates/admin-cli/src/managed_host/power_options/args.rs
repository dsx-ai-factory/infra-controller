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

use carbide_uuid::machine::HostMachineId;
use clap::{Parser, ValueEnum};
use rpc::forge::{self as forgerpc, PowerOptionUpdateRequest};

#[derive(Parser, Debug)]
pub(crate) enum Args {
    #[command(after_long_help = "\
EXAMPLES:

Show power options for all hosts:
    $ nico-admin-cli managed-host power-options show

Show power options for one host:
    $ nico-admin-cli managed-host power-options show 12345678-1234-5678-90ab-cdef01234567

")]
    Show(ShowPowerOptions),
    #[command(after_long_help = "\
EXAMPLES:

Set a host's desired power state (on / off / power-manager-disabled):
    $ nico-admin-cli managed-host power-options update 12345678-1234-5678-90ab-cdef01234567 \
    --desired-power-state off

")]
    Update(UpdatePowerOptions),
    #[clap(about = "Get machine ingestion state")]
    #[command(after_long_help = "\
EXAMPLES:

Get the ingestion state for a machine by its BMC MAC:
    $ nico-admin-cli managed-host power-options get-machine-ingestion-state \
    --mac-address 00:11:22:33:44:55

")]
    GetMachineIngestionState(BmcMacAddress),
    #[clap(about = "Allow a machine to power on")]
    #[command(after_long_help = "\
EXAMPLES:

Allow a machine to be ingested and powered on:
    $ nico-admin-cli managed-host power-options allow-ingestion-and-power-on \
    --mac-address 00:11:22:33:44:55

")]
    AllowIngestionAndPowerOn(BmcMacAddress),
}

#[derive(Parser, Debug)]
pub(crate) struct ShowPowerOptions {
    #[clap(help = "ID of the host or nothing for all")]
    pub(super) machine: Option<HostMachineId>,
}

#[derive(Parser, Debug)]
pub(crate) struct UpdatePowerOptions {
    #[clap(help = "ID of the host")]
    machine: HostMachineId,
    #[clap(long, short, help = "Desired Power State")]
    desired_power_state: DesiredPowerState,
}

impl From<UpdatePowerOptions> for PowerOptionUpdateRequest {
    fn from(args: UpdatePowerOptions) -> Self {
        let power_state = match args.desired_power_state {
            DesiredPowerState::On => forgerpc::PowerState::On,
            DesiredPowerState::Off => forgerpc::PowerState::Off,
            DesiredPowerState::PowerManagerDisabled => forgerpc::PowerState::PowerManagerDisabled,
        };
        Self {
            machine_id: Some(args.machine),
            power_state: power_state as i32,
        }
    }
}

#[derive(ValueEnum, Parser, Debug, Clone, PartialEq)]
enum DesiredPowerState {
    On,
    Off,
    PowerManagerDisabled,
}

#[derive(Parser, Debug)]
pub(crate) struct BmcMacAddress {
    #[clap(short, long, help = "MAC Address of host BMC endpoint")]
    pub(super) mac_address: mac_address::MacAddress,
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use clap::ValueEnum;

    use super::DesiredPowerState;

    #[test]
    fn desired_power_state_value_enum() {
        scenarios!(
            run = |s| DesiredPowerState::from_str(s, false).map(|v| format!("{v:?}"));
            "on" {
                "on" => Yields(format!("{:?}", DesiredPowerState::On)),
            }

            "off" {
                "off" => Yields(format!("{:?}", DesiredPowerState::Off)),
            }

            "power-manager-disabled" {
                "power-manager-disabled" => Yields(format!("{:?}", DesiredPowerState::PowerManagerDisabled)),
            }

            "invalid value" {
                "invalid" => Fails,
            }
        );
    }
}
