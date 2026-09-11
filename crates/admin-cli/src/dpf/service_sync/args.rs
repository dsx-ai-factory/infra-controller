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

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::HostMachineId;
use clap::{ArgGroup, Parser};
use rpc::forge::ReleaseDpuServiceSyncHoldRequest;
use rpc::forge::release_dpu_service_sync_hold_request::Target;

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List every machine waiting on a DPUService rollout, longest wait first:
    $ nico-admin-cli dpf service-sync list

Show one host's recorded sync history, including who released each one:
    $ nico-admin-cli dpf service-sync list --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80

Release the hold for one or more hosts:
    $ nico-admin-cli dpf service-sync release --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80

Release the host running an instance, accepting that its tenant is disrupted:
    $ nico-admin-cli dpf service-sync release --instance-id 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum Args {
    #[clap(about = "List machines DPF is waiting on before a DPUService rollout")]
    List(List),
    #[clap(about = "Release the DPF maintenance hold blocking a DPUService rollout")]
    Release(Release),
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

List every machine waiting on a DPUService rollout, longest wait first:
    $ nico-admin-cli dpf service-sync list

Show one host's recorded sync history, including who released each one:
    $ nico-admin-cli dpf service-sync list --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80

")]
pub(crate) struct List {
    #[clap(
        long = "machine-id",
        visible_alias = "id",
        help = "Show this host's recorded history instead of the outstanding worklist"
    )]
    pub(super) machine_id: Option<HostMachineId>,
}

#[derive(Parser, Debug)]
#[command(group(ArgGroup::new("target").required(true).multiple(false)))]
#[command(after_long_help = "\
EXAMPLES:

Release one host:
    $ nico-admin-cli dpf service-sync release --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80

Release several hosts, passing the ids after one flag:
    $ nico-admin-cli dpf service-sync release --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Release several hosts, repeating the flag instead:
    $ nico-admin-cli dpf service-sync release --machine-id fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80 \
    --machine-id fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

Release the host running an instance, accepting that its tenant is disrupted:
    $ nico-admin-cli dpf service-sync release --instance-id 12345678-1234-5678-90ab-cdef01234567

Release the hosts running several instances:
    $ nico-admin-cli dpf service-sync release --instance-id 12345678-1234-5678-90ab-cdef01234567 abcdef01-2345-6789-abcd-ef0123456789

")]
pub(crate) struct Release {
    /// Hosts to release. A host with a tenant on it is skipped: name that
    /// tenant's instance instead, so the disruption is asked for rather than
    /// stumbled into.
    #[clap(
        long = "machine-id",
        visible_alias = "id",
        num_args = 1..,
        value_name = "MACHINE_ID",
        group = "target",
        help = "One or more host machine IDs to release"
    )]
    pub(super) machine_ids: Vec<HostMachineId>,

    /// Releases the hosts currently running these instances even though they
    /// are assigned. Naming an instance is the acknowledgement that its tenant
    /// will be disrupted, and each one covers only the instance named.
    #[clap(
        long = "instance-id",
        num_args = 1..,
        value_name = "INSTANCE_ID",
        group = "target",
        help = "Release the hosts running these instances, disrupting their tenants"
    )]
    pub(super) instance_ids: Vec<InstanceId>,
}

impl From<Release> for ReleaseDpuServiceSyncHoldRequest {
    fn from(args: Release) -> Self {
        // The ArgGroup makes these mutually exclusive and one of them required,
        // so a non-empty instance list is the only way to reach the instance
        // target.
        let target = if args.instance_ids.is_empty() {
            Target::MachineIds(::rpc::common::HostMachineIdList {
                machine_ids: args.machine_ids,
            })
        } else {
            Target::InstanceIds(::rpc::forge::InstanceIdList {
                instance_ids: args.instance_ids,
            })
        };
        Self {
            target: Some(target),
        }
    }
}
