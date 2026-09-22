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

mod auto_update;
mod common;
mod force_delete;
mod hardware_info;
mod health_history;
mod health_report;
mod metadata;
pub(crate) mod network;
mod nvlink_info;
mod positions;
mod reboot;
mod show;

#[cfg(test)]
mod tests;

// Cross-module re-exports.
pub(crate) use auto_update::args::Args as MachineAutoupdate;
use clap::Parser;
pub(crate) use common::{MachineQuery, NetworkConfigQuery};
pub(crate) use health_report::args::HealthReportTemplates;
pub(crate) use health_report::cmd::{get_empty_template, get_health_report};
pub(crate) use show::args::Args as ShowMachine;
pub(crate) use show::cmd::{get_next_free_machine, get_next_free_machine_prefetched, handle_show};

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Display Machine information")]
    Show(show::Args),
    // Hidden: the view moved to the top-level `boot-interface show`. This
    // variant keeps `machine boot-interfaces` parsing for existing scripts and
    // runs the exact same handler (the payload type is the new command's Args).
    #[clap(
        hide = true,
        about = "Moved: use `boot-interface show` (kept as a hidden alias)"
    )]
    BootInterfaces(crate::boot_interface::show::Args),
    #[clap(subcommand, about = "Networking information")]
    Network(network::Args),
    #[clap(
        about = "Manage health report sources",
        subcommand,
        visible_alias = "hr",
        alias = "health-override"
    )]
    HealthReport(health_report::Args),
    #[clap(about = "Reboot a machine")]
    Reboot(reboot::Args),
    #[clap(about = "Force delete a machine")]
    ForceDelete(force_delete::Args),
    #[clap(about = "Set individual machine firmware autoupdate (host only)")]
    AutoUpdate(auto_update::Args),
    #[clap(subcommand, about = "Edit Metadata associated with a Machine")]
    Metadata(metadata::Args),
    #[clap(subcommand, about = "Update/show machine hardware info")]
    HardwareInfo(hardware_info::Args),
    #[clap(
        about = "Show physical location info for machines in rack-based systems",
        long_about = "Show physical location info for machines in rack-based systems.\n\n\
            Returns rack topology information including:\n\
            - Physical slot number: The slot position in the rack\n\
            - Compute tray index: The compute tray containing this machine\n\
            - Topology ID: Identifier for the rack topology configuration\n\
            - Revision ID: Hardware revision identifier\n\
            - Switch ID: Associated network switch\n\
            - Power shelf ID: Associated power shelf"
    )]
    Positions(positions::Args),
    #[clap(subcommand, about = "Show NVLink info for an MNNVL machine")]
    NvlinkInfo(nvlink_info::Args),
    #[clap(about = "Show machine health history")]
    HealthHistory(health_history::Args),
}
