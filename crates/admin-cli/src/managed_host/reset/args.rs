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
#[command(
    long_about = "Reset a managed host: remove its Instance and DPF resources, then re-ingest it.\n\n\
    If the host still has an Instance, Reset waits for every attached DPU to acknowledge \
    Admin networking before deleting the Instance and releasing its network resources. \
    An unreachable DPU can keep Reset waiting indefinitely. A started Reset cannot be canceled."
)]
#[command(after_long_help = "\
EXAMPLES:

Reset a host wedged mid-ingestion:
    $ nico-admin-cli managed-host reset set --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --update-message \"recovering wedged DPU\"

Reset a host that is assigned to a live instance (destroys the instance):
    $ nico-admin-cli managed-host reset set --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --allow-reset-with-instance --update-message \"forced recovery\"

Clear a reset request that has not started yet:
    $ nico-admin-cli managed-host reset clear --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

List all managed hosts pending reset:
    $ nico-admin-cli managed-host reset list

")]
pub(crate) enum Args {
    #[clap(
        about = "Request a reset of a managed host.",
        long_about = "Request a reset of a managed host.\n\n\
            If the host still has an Instance, including one already terminating, Reset requests \
            Admin networking. The Instance can lose tenant connectivity while Reset waits for \
            every attached DPU to acknowledge the change. Reset retains the Instance and its \
            network resources until then. An unreachable DPU can keep Reset waiting indefinitely. \
            Hosts without an Instance skip this network wait.\n\n\
            After Reset starts, it cannot be canceled with managed-host reset clear, including \
            while waiting for the DPUs."
    )]
    Set(ResetSet),
    #[clap(
        about = "Clear a reset request that has not started yet.",
        long_about = "Clear a reset request that has not started yet.\n\n\
            A started Reset cannot be canceled, including while it waits for DPUs to acknowledge \
            Admin networking. The API rejects attempts to clear a started Reset."
    )]
    Clear(ResetClear),
    #[clap(about = "List all managed hosts pending reset.")]
    List,
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Reset a host wedged mid-ingestion:
    $ nico-admin-cli managed-host reset set --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --update-message \"recovering wedged DPU\"

Reset a host that is assigned to a live instance (destroys the instance):
    $ nico-admin-cli managed-host reset set --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg \
    --allow-reset-with-instance --update-message \"forced recovery\"

")]
pub(crate) struct ResetSet {
    #[clap(long, help = "Managed host machine ID to reset.")]
    pub(super) machine: MachineId,

    #[clap(
        long,
        action,
        help = "Acknowledge destruction of the live Instance. Host cleanup also deletes its \
                data unless --ignore-cleanup is set. Required for a live Instance; does not \
                bypass the Admin network acknowledgement."
    )]
    pub(super) allow_reset_with_instance: bool,

    #[clap(
        long,
        action,
        requires = "allow_reset_with_instance",
        help = "Skip host cleanup after the Instance is deleted. Data from the previous tenant \
                stays on the host. Requires --allow-reset-with-instance and does not bypass \
                the Admin network acknowledgement."
    )]
    pub(super) ignore_cleanup: bool,

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
            ignore_cleanup: args.ignore_cleanup,
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Clear a reset request that has not started yet:
    $ nico-admin-cli managed-host reset clear --machine fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

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
            ignore_cleanup: false,
        }
    }
}
