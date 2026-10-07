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

use std::path::PathBuf;

use carbide_uuid::rack::RackId;
use clap::Parser;

#[derive(Parser, Debug)]
pub(crate) enum Args {
    #[clap(about = "Start on-demand rack maintenance (full rack or partial)")]
    Start(MaintenanceOptions),
    #[clap(
        about = "Terminate active rack maintenance and transition the rack to Error",
        long_about = "Request that the rack controller terminate active maintenance and transition the rack to Error. The command is rejected unless the rack is in Maintenance; repeating an accepted request before it is consumed is safe. Device requests and current phase status are cleaned up. This terminates NICo rack maintenance orchestration, but does not guarantee that work already submitted to an external backend is stopped."
    )]
    Terminate(TerminateOptions),
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Terminate active maintenance on a rack:
    $ nico-admin-cli rack maintenance terminate --rack rack-42-us-west

")]
pub(crate) struct TerminateOptions {
    #[clap(
        short,
        long,
        help = "Rack ID whose active maintenance should be terminated"
    )]
    pub(super) rack: RackId,
}

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Start maintenance on a full rack (all activities, all components):
    $ nico-admin-cli rack maintenance start --rack 12345678-1234-5678-90ab-cdef01234567

Run only a firmware upgrade on specific machines:
    $ nico-admin-cli rack maintenance start --rack 12345678-1234-5678-90ab-cdef01234567 \
    --machine-ids m1,m2 --activities firmware-upgrade

Firmware upgrade from a SOT JSON file, forcing the update:
    $ nico-admin-cli rack maintenance start --rack 12345678-1234-5678-90ab-cdef01234567 \
    --activities firmware-upgrade --sot-json-file ./sot.json --access-token \"$TOKEN\" --force-update

")]
pub(crate) struct MaintenanceOptions {
    #[clap(short, long, help = "Rack ID to start maintenance on")]
    pub(super) rack: RackId,

    #[clap(
        long,
        help = "Machine IDs to include (omit for full rack)",
        num_args = 1..,
        value_delimiter = ','
    )]
    pub(super) machine_ids: Option<Vec<String>>,

    #[clap(
        long,
        help = "Switch IDs to include (omit for full rack)",
        num_args = 1..,
        value_delimiter = ','
    )]
    pub(super) switch_ids: Option<Vec<String>>,

    #[clap(
        long,
        help = "Power shelf IDs to include (omit for full rack)",
        num_args = 1..,
        value_delimiter = ','
    )]
    pub(super) power_shelf_ids: Option<Vec<String>>,

    #[clap(
        long,
        help = "Maintenance activities to perform: firmware-upgrade, nvos-update, configure-nmx-cluster, power-sequence (omit for all)",
        num_args = 1..,
        value_delimiter = ','
    )]
    pub(super) activities: Option<Vec<String>>,

    #[clap(
        long,
        help = "Raw SOT JSON for firmware-upgrade activity (prefer --sot-json-file)"
    )]
    pub(super) firmware_version: Option<String>,

    #[clap(
        long = "sot-json-file",
        value_name = "PATH",
        help = "SOT JSON file for RMS ApplyFirmwareObject"
    )]
    pub(super) sot_json_file: Option<PathBuf>,

    #[clap(
        long = "access-token",
        help = "Artifact access token for RMS SOT JSON downloads; omit or pass empty for NOAUTH"
    )]
    pub(super) access_token: Option<String>,

    #[clap(long = "force-update", help = "Force firmware update when supported")]
    pub(super) force_update: bool,

    #[clap(
        long,
        help = "Firmware components to update, e.g. BMC,CPLD,BIOS (omit for all components)",
        num_args = 1..,
        value_delimiter = ','
    )]
    pub(super) components: Option<Vec<String>>,
}
