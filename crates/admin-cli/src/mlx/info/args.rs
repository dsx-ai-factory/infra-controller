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

// info/args.rs
// Command-line argument definitions for info commands.

use carbide_uuid::machine::{HostMachineId, MachineId};
use clap::Parser;
use rpc::protos::mlx_device as mlx_device_pb;

use crate::cfg::dispatch::Dispatch;

// InfoCommand are the info subcommands.
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

Get device info for one device on a machine:
    $ nico-admin-cli mlx info device 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0

Get the full device report for a machine:
    $ nico-admin-cli mlx info machine 12345678-1234-5678-90ab-cdef01234567

Show stored NIC identity evidence for a host:
    $ nico-admin-cli mlx info identities fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

")]
pub(crate) enum InfoCommand {
    #[clap(about = "Get MlxDeviceInfo for a device on a machine")]
    Device(InfoDeviceCommand),

    #[clap(about = "Get an MlxDeviceReport for a machine")]
    Machine(InfoMachineCommand),

    #[clap(about = "Show stored NIC identity evidence for a host")]
    Identities(InfoIdentitiesCommand),
}

// InfoDeviceCommand shows device information.
#[derive(Parser, Debug)]
pub(crate) struct InfoDeviceCommand {
    #[arg(help = "Carbide Machine ID")]
    machine_id: MachineId,

    #[arg(help = "Device ID is the PCI or mst path on the target machine")]
    device_id: String,
}

// InfoMachineCommand shows machine information.
#[derive(Parser, Debug)]
pub(crate) struct InfoMachineCommand {
    #[arg(help = "Carbide Machine ID")]
    machine_id: MachineId,
}

/// `InfoIdentitiesCommand` reads a host's stored NIC identity evidence.
#[derive(Parser, Debug)]
#[command(
    long_about = "Show NIC identity evidence from the host's stored Scout observation and \
        current managed-DPU associations. Does not contact Scout.\n\n\
        The observed device fields do not establish physical-card identity or firmware/reset \
        eligibility. An Unknown managed DPU means ownership is unknown, not that the NIC is \
        unmanaged. Conflicting lists all matching managed DPUs.\n\n\
        Supports ASCII, CSV, JSON, and YAML output. CSV contains the table columns only, \
        with just the headers when no observation is stored or no devices were reported.",
    after_long_help = "\
EXAMPLES:

Show stored NIC identity evidence for a host:
    $ nico-admin-cli mlx info identities fm100ht038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg

"
)]
pub(crate) struct InfoIdentitiesCommand {
    #[arg(help = "Host machine ID")]
    machine_id: HostMachineId,
}

impl From<InfoDeviceCommand> for mlx_device_pb::MlxAdminDeviceInfoRequest {
    fn from(cmd: InfoDeviceCommand) -> Self {
        Self {
            machine_id: cmd.machine_id.into(),
            device_id: cmd.device_id,
        }
    }
}

impl From<InfoMachineCommand> for mlx_device_pb::MlxAdminDeviceReportRequest {
    fn from(cmd: InfoMachineCommand) -> Self {
        Self {
            machine_id: cmd.machine_id.into(),
        }
    }
}

impl From<InfoIdentitiesCommand> for mlx_device_pb::MlxAdminDeviceIdentitiesRequest {
    fn from(cmd: InfoIdentitiesCommand) -> Self {
        Self {
            machine_id: Some(cmd.machine_id.into()),
        }
    }
}
