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

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Download a debug bundle for a host over a time range:
    $ nico-admin-cli managed-host debug-bundle 12345678-1234-5678-90ab-cdef01234567 \
    --start-time \"2026-01-02 03:04:05\" --end-time \"2026-01-02 04:00:00\"

Collect from a start time to now, interpreting times as UTC:
    $ nico-admin-cli managed-host debug-bundle 12345678-1234-5678-90ab-cdef01234567 \
    --start-time 03:04:05 --utc

")]
pub(crate) struct Args {
    #[clap(help = "The host machine ID to collect logs for")]
    pub(crate) host_id: String,

    #[clap(
        long,
        help = "Start time: 'YYYY-MM-DD HH:MM:SS' or 'HH:MM:SS' (uses today's date). Default: local timezone, use --utc for UTC"
    )]
    pub(crate) start_time: String,

    #[clap(
        long,
        help = "End time: 'YYYY-MM-DD HH:MM:SS' or 'HH:MM:SS' (uses today's date). Defaults to current time if not provided. Default: local timezone, use --utc for UTC"
    )]
    pub(crate) end_time: Option<String>,

    #[clap(
        long,
        help = "Interpret start-time and end-time as UTC instead of local timezone"
    )]
    pub(crate) utc: bool,

    #[clap(
        long,
        default_value = "/tmp",
        help = "Output directory path for the debug bundle (default: /tmp)"
    )]
    pub(crate) output_path: String,

    #[clap(
        long,
        help = "Grafana base URL (e.g., https://grafana.example.com). If not provided, log collection is skipped."
    )]
    pub(crate) grafana_url: Option<String>,

    #[clap(
        long,
        default_value = "5000",
        help = "Batch size for log collection (default: 5000, max: 5000)"
    )]
    pub(crate) batch_size: u32,
}
