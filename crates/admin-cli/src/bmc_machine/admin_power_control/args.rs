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
use rpc::forge as forgerpc;

use crate::bmc_machine::common::AdminPowerControlAction;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Power a machine on:
    $ nico-admin-cli bmc-machine admin-power-control \
    --machine 12345678-1234-5678-90ab-cdef01234567 --action on

Gracefully shut a machine down:
    $ nico-admin-cli bmc-machine admin-power-control \
    --machine 12345678-1234-5678-90ab-cdef01234567 --action graceful-shutdown

Force a machine off (immediate, no OS shutdown):
    $ nico-admin-cli bmc-machine admin-power-control \
    --machine 12345678-1234-5678-90ab-cdef01234567 --action force-off

Gracefully restart a machine:
    $ nico-admin-cli bmc-machine admin-power-control \
    --machine 12345678-1234-5678-90ab-cdef01234567 --action graceful-restart

")]
pub(crate) struct Args {
    #[clap(long, help = "ID of the machine to reboot")]
    machine: String,
    #[clap(long, help = "Power control action")]
    action: AdminPowerControlAction,
}

impl From<Args> for forgerpc::AdminPowerControlRequest {
    fn from(args: Args) -> Self {
        Self {
            bmc_endpoint_request: None,
            machine_id: Some(args.machine),
            action: forgerpc::admin_power_control_request::SystemPowerControl::from(args.action)
                .into(),
        }
    }
}
