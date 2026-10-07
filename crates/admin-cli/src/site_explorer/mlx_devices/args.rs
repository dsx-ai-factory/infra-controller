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

List Mellanox/BlueField devices across all explored hosts:
    $ nico-admin-cli site-explorer mlx-devices

List the devices found under one host BMC:
    $ nico-admin-cli site-explorer mlx-devices --host 192.0.2.20

Find devices operating as NICs whose firmware is below the desired version:
    $ nico-admin-cli site-explorer mlx-devices --nic-mode-only --expected-version 32.42.1000

")]
pub(crate) struct Args {
    #[clap(long, help = "Restrict to devices found under this host BMC IP")]
    pub(super) host: Option<String>,
    #[clap(
        long,
        help = "Only devices operating as NICs: their DPU BMC reports NIC mode, or they have a SuperNIC SKU and the mode is unknown"
    )]
    pub(super) nic_mode_only: bool,
    #[clap(
        long,
        help = "Only devices whose NIC firmware is below this version (e.g. 32.42.1000)"
    )]
    pub(super) expected_version: Option<String>,
}
