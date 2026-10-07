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

// profile/args.rs
// Command-line argument definitions for profile commands.

use carbide_uuid::machine::MachineId;
use clap::Parser;
use rpc::protos::mlx_device as mlx_device_pb;

use crate::cfg::dispatch::Dispatch;

// ProfileCommand are the profile subcommands.
#[derive(Parser, Debug, Dispatch)]
#[command(after_long_help = "\
EXAMPLES:

List all available profiles:
    $ nico-admin-cli mlx profile list

Show a profile's details:
    $ nico-admin-cli mlx profile show my-profile

Sync a profile to a device on a machine:
    $ nico-admin-cli mlx profile sync 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0 \
    --profile-name my-profile

Compare a device against a profile:
    $ nico-admin-cli mlx profile compare 12345678-1234-5678-90ab-cdef01234567 0000:01:00.0 \
    --profile-name my-profile

")]
pub(crate) enum ProfileCommand {
    #[clap(about = "Synchronize a profile to a device on a given machine")]
    Sync(ProfileSyncCommand),

    #[clap(about = "Compare a profile to a device on a given machine")]
    Compare(ProfileCompareCommand),

    #[clap(about = "Show profile details")]
    Show(ProfileShowCommand),

    #[clap(about = "List all available profiles")]
    List(ProfileListCommand),
}

// ProfileSyncCommand synchronizes a profile to a device.
#[derive(Parser, Debug)]
pub(crate) struct ProfileSyncCommand {
    #[arg(help = "Carbide Machine ID")]
    machine_id: MachineId,

    #[arg(help = "Device ID is the PCI or mst path on the target machine")]
    device_id: String,

    #[arg(long, help = "Profile name to sync")]
    profile_name: String,
}

// ProfileCompareCommand compares a profile against a device.
#[derive(Parser, Debug)]
pub(crate) struct ProfileCompareCommand {
    #[arg(help = "Carbide Machine ID")]
    machine_id: MachineId,

    #[arg(help = "Device ID is the PCI or mst path on the target machine")]
    device_id: String,

    #[arg(long, help = "Profile name to compare")]
    profile_name: String,
}

// ProfileShowCommand shows details of a specific profile.
#[derive(Parser, Debug)]
pub(crate) struct ProfileShowCommand {
    #[arg(help = "Profile name to show")]
    profile_name: String,
}

// ProfileListCommand lists all available profiles.
#[derive(Parser, Debug)]
pub(crate) struct ProfileListCommand {}

impl From<ProfileSyncCommand> for mlx_device_pb::MlxAdminProfileSyncRequest {
    fn from(cmd: ProfileSyncCommand) -> Self {
        Self {
            machine_id: cmd.machine_id.into(),
            device_id: cmd.device_id,
            profile_name: cmd.profile_name,
        }
    }
}

impl From<ProfileCompareCommand> for mlx_device_pb::MlxAdminProfileCompareRequest {
    fn from(cmd: ProfileCompareCommand) -> Self {
        Self {
            machine_id: cmd.machine_id.into(),
            device_id: cmd.device_id,
            profile_name: cmd.profile_name,
        }
    }
}

impl From<ProfileShowCommand> for mlx_device_pb::MlxAdminProfileShowRequest {
    fn from(cmd: ProfileShowCommand) -> Self {
        Self {
            profile_name: cmd.profile_name,
        }
    }
}
