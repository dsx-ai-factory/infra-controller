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
use rpc::forge::AdminForceDeleteMachineRequest;

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Force delete a machine (by UUID, IPv4, MAC, or hostname):
    $ nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567

Force delete a machine and its interfaces (redeploy kea afterward):
    $ nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 \
    --delete-interfaces

Force delete with a full rediscovery wipe (interfaces, BMC interfaces, \
suppressions, and retained boot targets):
    $ nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 \
    --delete-interfaces --delete-bmc-interfaces --delete-bmc-suppressions \
    --delete-retained-boot-interfaces

")]
pub(crate) struct Args {
    #[clap(
        long,
        help = "UUID, IPv4, MAC or hostname of the host or DPU machine to delete"
    )]
    pub(super) machine: String,

    #[clap(short = 'd', long, action, help = "Delete interfaces.")]
    delete_interfaces: bool,

    #[clap(short = 'b', long, action, help = "Delete BMC interfaces.")]
    delete_bmc_interfaces: bool,

    #[clap(
        short = 'c',
        long,
        action,
        help = "Delete BMC credentials. Only applicable if site explorer has configured credentials for the BMCs associated with this managed host."
    )]
    delete_bmc_credentials: bool,

    #[clap(
        long,
        action,
        help = "Delete Site Explorer and DHCP BMC suppressions for the host/DPU BMC MACs and underlay (OOB) MACs so rediscovery is not skipped."
    )]
    delete_bmc_suppressions: bool,

    #[clap(
        long,
        action,
        help = "Delete retained boot-interface pairs for the host/DPU BMC and interface MACs. Without this, deleted interfaces keep their boot targets for re-ingestion."
    )]
    delete_retained_boot_interfaces: bool,

    #[clap(
        long,
        action,
        help = "Delete machine with allocated instance. This flag acknowledges destroying the user instance as well."
    )]
    pub(super) allow_delete_with_instance: bool,

    #[clap(
        long,
        action,
        help = "Delete machine even if DPF CRDs exist and DPF is disabled at the site level. This flag acknowledges that orphaned DPF resources may remain"
    )]
    allow_delete_with_orphaned_dpf_crds: bool,
}

impl From<&Args> for AdminForceDeleteMachineRequest {
    fn from(args: &Args) -> Self {
        Self {
            host_query: args.machine.clone(),
            delete_interfaces: args.delete_interfaces,
            delete_bmc_interfaces: args.delete_bmc_interfaces,
            delete_bmc_credentials: args.delete_bmc_credentials,
            allow_delete_with_orphaned_dpf_crds: args.allow_delete_with_orphaned_dpf_crds,
            delete_bmc_suppressions: args.delete_bmc_suppressions,
            delete_retained_boot_interfaces: args.delete_retained_boot_interfaces,
        }
    }
}
