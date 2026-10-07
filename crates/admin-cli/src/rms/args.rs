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

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Get the full RMS inventory (RMS URL taken from --url or config):
    $ nico-admin-cli rms --url https://rms.example.com:8443 inventory

Get a rack's power-on sequence (URL from config):
    $ nico-admin-cli rms power-on-sequence rack-1

Talk to RMS over mTLS with explicit certs:
    $ nico-admin-cli rms --url https://rms.example.com:8443 \
    --root-ca /etc/rms/ca.crt --client-cert /etc/rms/client.crt \
    --client-key /etc/rms/client.key inventory

The --url, --root-ca, --client-cert and --client-key flags are global and
may be given before or after the subcommand.

")]
pub(crate) struct RmsAction {
    #[clap(subcommand)]
    pub(super) command: Cmd,

    #[clap(long, global = true, help = "URL of RMS API endpoint (required).")]
    pub(super) url: Option<String>,

    #[clap(long, global = true, help = "Root CA path")]
    pub(super) root_ca: Option<String>,

    #[clap(long, global = true, help = "Client certificate path")]
    pub(super) client_cert: Option<String>,

    #[clap(long, global = true, help = "Client key path")]
    pub(super) client_key: Option<String>,
}

#[derive(Parser, Debug, Clone)]
#[clap(rename_all = "kebab_case")]
pub(super) enum Cmd {
    #[clap(about = "Get the full RMS inventory")]
    Inventory,
    #[clap(about = "Get the power on sequence")]
    PowerOnSequence(PowerOnSequence),
    #[clap(about = "Get the power state for a given node")]
    PowerState(PowerState),
    #[clap(about = "Get the firmware inventory for a given node")]
    FirmwareInventory(FirmwareInventory),
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Get the power-on sequence for a rack:
    $ nico-admin-cli rms power-on-sequence rack-1

")]
pub(super) struct PowerOnSequence {
    #[clap(help = "Rack ID to get power sequence for")]
    rack_id: String,
}

impl From<PowerOnSequence> for librms::protos::rack_manager::GetRackPowerOnSequenceRequest {
    fn from(args: PowerOnSequence) -> Self {
        Self {
            rack_id: args.rack_id,
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Get the power state of a node in a rack:
    $ nico-admin-cli rms power-state rack-1 node-1

")]
pub(super) struct PowerState {
    #[clap(help = "Rack ID to get power sequence for")]
    rack_id: String,
    #[clap(help = "Node ID to get power state for")]
    node_id: String,
}

impl From<PowerState> for librms::protos::rack_manager::GetPowerStateRequest {
    fn from(args: PowerState) -> Self {
        Self {
            node_id: args.node_id,
            rack_id: args.rack_id,
        }
    }
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Get the firmware inventory for a node in a rack:
    $ nico-admin-cli rms firmware-inventory rack-1 node-1

")]
pub(super) struct FirmwareInventory {
    #[clap(help = "Rack ID to get power sequence for")]
    rack_id: String,
    #[clap(help = "Node ID to get firmware inventory for")]
    node_id: String,
}

impl From<FirmwareInventory> for librms::protos::rack_manager::GetNodeFirmwareInventoryRequest {
    fn from(args: FirmwareInventory) -> Self {
        Self {
            node_id: args.node_id,
            rack_id: args.rack_id,
        }
    }
}
