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

use carbide_uuid::machine::{MachineInterfaceId, StableHostMachineId};
use clap::Parser;
use rpc::forge as forgerpc;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Make a host interface the primary (boot) interface:
    $ nico-admin-cli managed-host set-primary-interface 12345678-1234-5678-90ab-cdef01234567 \
    abcdef01-2345-6789-abcd-ef0123456789

Request another reconciliation for the selected interface:
    $ nico-admin-cli managed-host set-primary-interface 12345678-1234-5678-90ab-cdef01234567 \
    abcdef01-2345-6789-abcd-ef0123456789 --force-reconcile

Tip: list a host's interface ids with 'managed-host show <HOST_MACHINE_ID>'.
")]
pub(crate) struct Args {
    #[clap(help = "ID of the host machine")]
    host_machine_id: StableHostMachineId,
    #[clap(help = "ID of the machine interface to make primary (the boot device)")]
    interface_id: MachineInterfaceId,
    #[clap(
        long,
        help = "Request a fresh machine-controller reconciliation even when this interface is already selected. Sends only force_reconcile=true; servers without force_reconcile support ignore it, while supporting servers leave any required restart to machine-controller"
    )]
    force_reconcile: bool,
    #[clap(
        long,
        help = "Deprecated compatibility option for servers without force_reconcile support. Sends reboot=true and force_reconcile=true; supporting servers treat it as reconciliation, while older servers force-restart the host after changing the target"
    )]
    reboot: bool,
}

#[allow(deprecated)] // Keep `--reboot` functional when this CLI calls an older server.
impl From<Args> for forgerpc::SetPrimaryInterfaceRequest {
    fn from(args: Args) -> Self {
        Self {
            host_machine_id: Some(args.host_machine_id),
            interface_id: Some(args.interface_id),
            reboot: args.reboot,
            force_reconcile: args.force_reconcile || args.reboot,
        }
    }
}
