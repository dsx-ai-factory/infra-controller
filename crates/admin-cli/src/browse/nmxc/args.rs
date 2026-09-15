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

use carbide_uuid::rack::RackId;
use clap::{Parser, ValueEnum};
use rpc::forge as forgerpc;

// NMX-C browse is operation-based rather than path-based: you pick one of a
// fixed set of operations against a chassis. This CLI-side enum mirrors the
// RPC's `NmxcBrowseOperation` (minus the `Unspecified` sentinel) so clap can
// render and validate the choices.
#[derive(ValueEnum, Debug, Clone)]
#[clap(rename_all = "kebab-case")]
pub(super) enum NmxcOperationArg {
    ComputeNodeInfoList,
    SwitchNodeInfoList,
    GpuInfo,
    GpuInfoList,
    PartitionInfoList,
    GetDomainProperties,
}

impl From<NmxcOperationArg> for forgerpc::NmxcBrowseOperation {
    fn from(op: NmxcOperationArg) -> Self {
        match op {
            NmxcOperationArg::ComputeNodeInfoList => {
                forgerpc::NmxcBrowseOperation::ComputeNodeInfoList
            }
            NmxcOperationArg::SwitchNodeInfoList => {
                forgerpc::NmxcBrowseOperation::SwitchNodeInfoList
            }
            NmxcOperationArg::GpuInfo => forgerpc::NmxcBrowseOperation::GpuInfo,
            NmxcOperationArg::GpuInfoList => forgerpc::NmxcBrowseOperation::GpuInfoList,
            NmxcOperationArg::PartitionInfoList => forgerpc::NmxcBrowseOperation::PartitionInfoList,
            NmxcOperationArg::GetDomainProperties => {
                forgerpc::NmxcBrowseOperation::GetDomainProperties
            }
        }
    }
}

#[derive(Parser, Debug)]
#[command(
    long_about = "Run an NMX-C browse operation via the API server.\n\n--operation is required. Exactly one endpoint selector must be specified: --chassis-serial or --rack-id. Providing both selectors is rejected.",
    after_long_help = "\
EXAMPLES:

List the GPUs on a chassis via NMX-C:
    $ nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation gpu-info-list

List the GPUs in a rack via NMX-C:
    $ nico-admin-cli browse nmxc --rack-id rack_vr_min_1 --operation gpu-info-list

List the compute nodes on a chassis:
    $ nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation compute-node-info-list

List the switch nodes on a chassis:
    $ nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation switch-node-info-list

Get info for a specific GPU UID:
    $ nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation gpu-info --gpu-uid 42

List NMX-C partitions:
    $ nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation partition-info-list

Get NMX-C domain properties:
    $ nico-admin-cli browse nmxc --chassis-serial 1234567890 --operation get-domain-properties

"
)]
pub(crate) struct Args {
    #[clap(
        long,
        help = "Chassis serial number (mutually exclusive with --rack-id)",
        conflicts_with = "rack_id",
        required_unless_present = "rack_id"
    )]
    pub(super) chassis_serial: Option<String>,

    #[clap(
        long,
        help = "Rack ID; resolves the NMX-C endpoint from the rack's ready control-plane switch (mutually exclusive with --chassis-serial)",
        conflicts_with = "chassis_serial",
        required_unless_present = "chassis_serial"
    )]
    pub(super) rack_id: Option<RackId>,

    #[clap(long, value_enum, help = "NMX-C browse operation to run")]
    pub(super) operation: NmxcOperationArg,

    #[clap(
        long,
        default_value = "0",
        help = "GPU UID (used by the gpu-info operation)"
    )]
    pub(super) gpu_uid: u64,
}
