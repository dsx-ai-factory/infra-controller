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

Force delete a machine and its interfaces:
    $ nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 \
    --delete-interfaces

Force delete with a full rediscovery wipe (interfaces, BMC interfaces, \
suppressions, and retained boot targets):
    $ nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 \
    --delete-interfaces --delete-bmc-interfaces --delete-bmc-suppressions \
    --delete-retained-boot-interfaces

Force delete a machine assigned to an Instance Type:
    $ nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 \
    --allow-delete-with-instance-type

Force delete a machine with an attached Instance. This removes the attached \
Instance control-plane record without first requesting a graceful workload \
shutdown; force-delete cleanup may forcibly restart the host:
    $ nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 \
    --allow-delete-with-instance

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
        help = "Delete Machine with an assigned Instance Type. This flag acknowledges removing the Instance Type association."
    )]
    pub(super) allow_delete_with_instance_type: bool,

    #[clap(
        long,
        action,
        help = "Delete Machine with an attached Instance. This flag also allows removing an assigned Instance Type and removes the attached Instance control-plane record without first requesting a graceful workload shutdown; force-delete cleanup may forcibly restart the host."
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
            allow_delete_with_instance_type: args.allow_delete_with_instance_type
                || args.allow_delete_with_instance,
            allow_delete_with_instance: args.allow_delete_with_instance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instance_override_maps_to_type_and_instance_permissions() {
        for (name, argv, expected_type, expected_instance) in [
            (
                "omitted",
                vec!["force-delete", "--machine", "machine-1"],
                false,
                false,
            ),
            (
                "instance type only",
                vec![
                    "force-delete",
                    "--machine",
                    "machine-1",
                    "--allow-delete-with-instance-type",
                ],
                true,
                false,
            ),
            (
                "instance implies instance type",
                vec![
                    "force-delete",
                    "--machine",
                    "machine-1",
                    "--allow-delete-with-instance",
                ],
                true,
                true,
            ),
        ] {
            let args = Args::try_parse_from(argv).unwrap_or_else(|error| panic!("{name}: {error}"));
            let request = AdminForceDeleteMachineRequest::from(&args);

            assert_eq!(
                request.allow_delete_with_instance_type, expected_type,
                "{name}"
            );
            assert_eq!(
                request.allow_delete_with_instance, expected_instance,
                "{name}"
            );
        }
    }
}
