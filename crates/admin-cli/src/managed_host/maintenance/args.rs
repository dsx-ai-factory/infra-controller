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

/// Enable or disable maintenance mode on a managed host.
/// To list machines in maintenance mode use `nico-admin-cli mh show --all --fix`
#[derive(Parser, Debug)]
pub(crate) enum Args {
    /// Put this machine into maintenance mode. Prevents an instance being assigned to it.
    On(MaintenanceOn),
    /// Return this machine to normal operation.
    Off(MaintenanceOff),
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Put a host into maintenance mode (prevents instance assignment):
    $ nico-admin-cli managed-host maintenance on --host 12345678-1234-5678-90ab-cdef01234567 \
    --reference https://tickets.example.com/MH-42

")]
pub(crate) struct MaintenanceOn {
    #[clap(long, required(true), help = "Managed Host ID")]
    host: MachineId,

    #[clap(
        long,
        visible_alias = "ref",
        required(true),
        help = "URL of reference (ticket, issue, etc) for this machine's maintenance"
    )]
    reference: String,
}

impl From<MaintenanceOn> for forgerpc::MaintenanceRequest {
    fn from(args: MaintenanceOn) -> Self {
        Self {
            operation: forgerpc::MaintenanceOperation::Enable.into(),
            host_id: Some(args.host),
            reference: Some(args.reference),
        }
    }
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Return a host to normal operation:
    $ nico-admin-cli managed-host maintenance off --host 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct MaintenanceOff {
    #[clap(long, required(true), help = "Managed Host ID")]
    host: MachineId,
}

impl From<MaintenanceOff> for forgerpc::MaintenanceRequest {
    fn from(args: MaintenanceOff) -> Self {
        Self {
            operation: forgerpc::MaintenanceOperation::Disable.into(),
            host_id: Some(args.host),
            reference: None,
        }
    }
}
