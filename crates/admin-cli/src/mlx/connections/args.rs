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

// connections/args.rs
// Command-line argument definitions for mlx connections commands.

use carbide_uuid::machine::MachineId;
use clap::Parser;
use rpc::protos::forge as forge_pb;

use crate::cfg::dispatch::Dispatch;

// ConnectionsCommand are the connections subcommands.
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

Show all active scout stream connections:
    $ nico-admin-cli mlx connections show

Disconnect a machine's scout stream connection:
    $ nico-admin-cli mlx connections disconnect 12345678-1234-5678-90ab-cdef01234567

")]
pub(crate) enum ConnectionsCommand {
    #[clap(about = "Show all active scout stream connections")]
    Show(ConnectionsShowCommand),
    #[clap(about = "Disconnect a scout stream connection")]
    Disconnect(ConnectionsDisconnectCommand),
}

// ConnectionsShowCommand shows all active scout stream connections.
#[derive(Parser, Debug)]
pub(crate) struct ConnectionsShowCommand {}

// ConnectionsDisconnectCommand disconnects a machine based on machine ID.
#[derive(Parser, Debug)]
pub(crate) struct ConnectionsDisconnectCommand {
    machine_id: MachineId,
}

impl From<ConnectionsDisconnectCommand> for forge_pb::ScoutStreamDisconnectRequest {
    fn from(cmd: ConnectionsDisconnectCommand) -> Self {
        Self {
            machine_id: cmd.machine_id.into(),
        }
    }
}
