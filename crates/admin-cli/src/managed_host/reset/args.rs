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

use ::rpc::forge::managed_host_reset_request::Mode;
use ::rpc::forge::{ManagedHostResetRequest, UpdateInitiator};
use carbide_uuid::machine::MachineId;
use clap::Parser;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Reset a host wedged mid-ingestion:
    $ nico-admin-cli managed-host reset set --machine 12345678-1234-5678-90ab-cdef01234567 \
    --update-message \"recovering wedged DPU\"

Reset a host that is assigned to a live instance (destroys the instance):
    $ nico-admin-cli managed-host reset set --machine 12345678-1234-5678-90ab-cdef01234567 \
    --allow-reset-with-instance --update-message \"forced recovery\"

Clear a reset request that has not started yet:
    $ nico-admin-cli managed-host reset clear --machine 12345678-1234-5678-90ab-cdef01234567

List all managed hosts pending reset:
    $ nico-admin-cli managed-host reset list

")]
pub(crate) enum Args {
    #[clap(about = "Request a reset of a managed host.")]
    Set(ResetSet),
    #[clap(about = "Clear a reset request that has not started yet.")]
    Clear(ResetClear),
    #[clap(about = "List all managed hosts pending reset.")]
    List,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Reset a host wedged mid-ingestion:
    $ nico-admin-cli managed-host reset set --machine 12345678-1234-5678-90ab-cdef01234567 \
    --update-message \"recovering wedged DPU\"

Reset a host that is assigned to a live instance (destroys the instance):
    $ nico-admin-cli managed-host reset set --machine 12345678-1234-5678-90ab-cdef01234567 \
    --allow-reset-with-instance --update-message \"forced recovery\"

")]
pub(crate) struct ResetSet {
    #[clap(long, help = "Managed host machine ID to reset.")]
    pub(super) machine: MachineId,

    #[clap(
        long,
        action,
        help = "Acknowledge that resetting an assigned host destroys the live instance and its \
                data. Required when the host has an instance."
    )]
    pub(super) allow_reset_with_instance: bool,

    #[clap(
        long,
        help = "If set, a HostUpdateInProgress health alert with this message is applied to the \
                host. The alert is a precondition for the reset."
    )]
    pub(super) update_message: Option<String>,
}

impl From<&ResetSet> for ManagedHostResetRequest {
    fn from(args: &ResetSet) -> Self {
        Self {
            machine_id: Some(args.machine),
            mode: Mode::Set as i32,
            initiator: UpdateInitiator::AdminCli as i32,
            allow_reset_with_instance: args.allow_reset_with_instance,
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Clear a reset request that has not started yet:
    $ nico-admin-cli managed-host reset clear --machine 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) struct ResetClear {
    #[clap(
        long,
        help = "Managed host machine ID whose reset request should be cleared."
    )]
    machine: MachineId,
}

impl From<ResetClear> for ManagedHostResetRequest {
    fn from(args: ResetClear) -> Self {
        Self {
            machine_id: Some(args.machine),
            mode: Mode::Clear as i32,
            initiator: UpdateInitiator::AdminCli as i32,
            allow_reset_with_instance: false,
        }
    }
}
