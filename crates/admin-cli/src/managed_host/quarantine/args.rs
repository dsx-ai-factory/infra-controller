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
use clap::Parser;
use rpc::forge as forgerpc;

/// Enable or disable quarantine mode on a managed host.
#[derive(Parser, Debug)]
pub(crate) enum Args {
    /// Put this machine into quarantine. Prevents any network access on the host machine.
    On(QuarantineOn),
    /// Take this machine out of quarantine
    Off(QuarantineOff),
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Quarantine a host (blocks all host network traffic):
    $ nico-admin-cli managed-host quarantine on --host 12345678-1234-5678-90ab-cdef01234567 \
    --reason \"suspected compromise\"

")]
pub(crate) struct QuarantineOn {
    #[clap(long, required(true), help = "Managed Host ID")]
    pub(super) host: HostMachineId,

    #[clap(
        long,
        visible_alias = "reason",
        required(true),
        help = "Reason for quarantining this host"
    )]
    reason: String,
}

impl From<QuarantineOn> for forgerpc::SetManagedHostQuarantineStateRequest {
    fn from(args: QuarantineOn) -> Self {
        Self {
            machine_id: Some(args.host),
            quarantine_state: Some(forgerpc::ManagedHostQuarantineState {
                mode: forgerpc::ManagedHostQuarantineMode::BlockAllTraffic as i32,
                reason: Some(args.reason),
            }),
        }
    }
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Take a host out of quarantine:
    $ nico-admin-cli managed-host quarantine off --host 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct QuarantineOff {
    #[clap(long, required(true), help = "Managed Host ID")]
    pub(super) host: HostMachineId,
}

impl From<QuarantineOff> for forgerpc::ClearManagedHostQuarantineStateRequest {
    fn from(args: QuarantineOff) -> Self {
        Self {
            machine_id: Some(args.host),
        }
    }
}
