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

use clap::{ArgGroup, Parser};
use rpc::forge::AdminForceDeleteMachineRequest;

#[derive(Parser, Debug, Clone)]
#[clap(group(ArgGroup::new("interface_deletion")
    .multiple(true)
    .args(["delete_interfaces", "delete_bmc_interfaces"])))]
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

Force delete and permanently release preserved address reservations \
instead of parking them:
    $ nico-admin-cli machine force-delete --machine 12345678-1234-5678-90ab-cdef01234567 \
    --delete-interfaces --release-preserved-addresses

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
        requires = "interface_deletion",
        help = "Release preserved address reservations for deleted interfaces instead of parking them. Without this, an address marked for preservation is parked so the same MAC can reclaim it on re-ingestion."
    )]
    release_preserved_addresses: bool,

    #[clap(
        long,
        action,
        help = "Delete machine with allocated instance. This flag acknowledges destroying the user instance as well."
    )]
    pub(super) allow_delete_with_instance: bool,

    #[clap(
        long,
        action,
        help = "Wait for all attached DPUs to acknowledge Admin networking for an allocated Instance",
        long_help = "Wait for all attached DPUs to acknowledge the Admin network configuration before deleting a host that has an Instance when force deletion starts. Disabled by default; a fresh deletion without this flag does not wait for DPU acknowledgements. A fresh deletion without an Instance does not wait.\n\n\
            Once recorded, the wait survives retries; omitting this flag cannot cancel it. Only servers supporting this option enforce a recorded wait. An older server can complete deletion without acknowledgement, even if a newer server already recorded the wait.\n\n\
            An unavailable DPU can prevent completion indefinitely. The CLI polls every 5 seconds for up to 20 minutes, then exits with deletion still pending. This flag does not replace --allow-delete-with-instance."
    )]
    wait_for_instance_dpu: bool,

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
            release_preserved_addresses: args.release_preserved_addresses,
            wait_for_instance_dpu: args.wait_for_instance_dpu,
        }
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use clap::CommandFactory;

    use super::*;

    const MACHINE: &str = "12345678-1234-5678-90ab-cdef01234567";

    #[test]
    fn arg_config_is_valid() {
        Args::command().debug_assert();
    }

    #[test]
    fn instance_dpu_wait_is_opt_in_and_does_not_grant_deletion_consent() {
        scenarios!(
            run = |extra: &[&str]| {
                let argv = ["force-delete", "--machine", MACHINE]
                    .into_iter()
                    .chain(extra.iter().copied());
                Args::try_parse_from(argv)
                    .map(|args| {
                        let request = AdminForceDeleteMachineRequest::from(&args);
                        (request.wait_for_instance_dpu, args.allow_delete_with_instance)
                    })
                    .map_err(drop)
            };
            "omitting the wait flag preserves cleanup without acknowledgements" {
                [].as_slice() => Yields((false, false)),
            }
            "waiting does not grant consent to delete an allocated instance" {
                ["--wait-for-instance-dpu"].as_slice() => Yields((true, false)),
            }
            "waiting and deletion consent can be requested together" {
                ["--allow-delete-with-instance", "--wait-for-instance-dpu"].as_slice()
                    => Yields((true, true)),
            }
        );
    }

    #[test]
    fn release_preserved_addresses_requires_an_interface_deletion_mode() {
        scenarios!(
            run = |extra: &[&str]| {
                let mut argv = vec!["force-delete", "--machine", MACHINE];
                argv.extend_from_slice(extra);
                Args::try_parse_from(argv)
                    .map(|args| {
                        (
                            args.release_preserved_addresses,
                            args.delete_interfaces,
                            args.delete_bmc_interfaces,
                        )
                    })
                    .map_err(drop)
            };
            "release without a deletion mode is rejected" {
                ["--release-preserved-addresses"].as_slice() => Fails,
            }
            "release pairs with either deletion mode" {
                ["--delete-interfaces", "--release-preserved-addresses"].as_slice()
                    => Yields((true, true, false)),
                ["--delete-bmc-interfaces", "--release-preserved-addresses"].as_slice()
                    => Yields((true, false, true)),
            }
            "both deletion modes may be combined" {
                ["--delete-interfaces", "--delete-bmc-interfaces", "--release-preserved-addresses"]
                    .as_slice() => Yields((true, true, true)),
            }
            "deletion modes are still valid without the release flag" {
                ["--delete-interfaces"].as_slice() => Yields((false, true, false)),
            }
        );
    }
}
