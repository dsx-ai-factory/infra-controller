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

// registry/args.rs
// Command-line argument definitions for registry commands.

use carbide_uuid::machine::MachineId;
use clap::Parser;
use rpc::protos::mlx_device as mlx_device_pb;

use crate::cfg::dispatch::Dispatch;

// RegistryCommand are the registry subcommands.
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

List the variable registries available on a machine:
    $ nico-admin-cli mlx registry list 12345678-1234-5678-90ab-cdef01234567

Show one registry's details:
    $ nico-admin-cli mlx registry show 12345678-1234-5678-90ab-cdef01234567 my-registry

")]
pub(crate) enum RegistryCommand {
    #[clap(about = "List all available registries")]
    List(RegistryListCommand),

    #[clap(about = "Show details of a specific registry")]
    Show(RegistryShowCommand),
}

// RegistryListCommand lists all available registries.
#[derive(Parser, Debug)]
pub(crate) struct RegistryListCommand {
    #[arg(help = "Carbide Machine ID")]
    machine_id: MachineId,
}

// RegistryShowCommand shows details of a specific registry.
#[derive(Parser, Debug)]
pub(crate) struct RegistryShowCommand {
    #[arg(help = "Carbide Machine ID")]
    machine_id: MachineId,

    #[arg(help = "Registry name to show")]
    registry_name: String,
}

impl From<RegistryListCommand> for mlx_device_pb::MlxAdminRegistryListRequest {
    fn from(cmd: RegistryListCommand) -> Self {
        Self {
            machine_id: cmd.machine_id.into(),
        }
    }
}

impl From<RegistryShowCommand> for mlx_device_pb::MlxAdminRegistryShowRequest {
    fn from(cmd: RegistryShowCommand) -> Self {
        Self {
            machine_id: cmd.machine_id.into(),
            registry_name: cmd.registry_name,
        }
    }
}
