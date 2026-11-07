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

mod allocate;
mod reboot;
mod release;
mod show;
mod update_ib_config;
mod update_nvlink_config;
mod update_os;
mod update_spx_config;

// Cross-module re-exports for jump module
// Cross-module re-export for rpc module
pub(crate) use allocate::args::Args as AllocateInstance;
use clap::Parser;
pub(crate) use show::args::Args as ShowInstance;
pub(crate) use show::cmd::handle_show;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Display instance information")]
    Show(show::Args),
    #[clap(about = "Reboot instance, potentially applying firmware updates")]
    Reboot(reboot::Args),
    #[clap(about = "De-allocate instance")]
    Release(release::Args),
    #[clap(about = "Allocate instance")]
    Allocate(allocate::Args),
    #[clap(about = "Update instance OS")]
    UpdateOS(update_os::Args),
    #[clap(about = "Update instance IB configuration")]
    UpdateIbConfig(update_ib_config::Args),
    #[clap(about = "Update instance NVLink configuration")]
    UpdateNvLinkConfig(update_nvlink_config::Args),
    #[clap(about = "Update instance SPX configuration")]
    UpdateSpxConfig(update_spx_config::Args),
}
