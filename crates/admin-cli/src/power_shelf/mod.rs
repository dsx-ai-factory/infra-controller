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

mod decommission;
mod delete;
mod force_delete;
mod health_history;
mod health_report;
mod list;
mod maintenance;
mod metadata;
mod show;

#[cfg(test)]
mod tests;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Show power shelf information")]
    Show(show::Args),
    #[clap(about = "List all power shelves")]
    List(list::Args),
    #[clap(about = "Start decommissioning a managed power shelf")]
    Decommission(decommission::Args),
    #[clap(about = "Delete a power shelf")]
    Delete(delete::Args),
    #[clap(about = "Force delete a power shelf and optionally its interfaces and BMC suppressions")]
    ForceDelete(force_delete::Args),
    #[clap(subcommand, about = "Manage Power Shelf Metadata")]
    Metadata(metadata::Args),
    #[clap(
        subcommand,
        about = "Request a power shelf maintenance operation (PowerOn / PowerOff)",
        visible_alias = "fix"
    )]
    Maintenance(maintenance::Args),
    #[dispatch]
    #[clap(
        about = "Manage health report sources",
        subcommand,
        visible_alias = "hr"
    )]
    HealthReport(health_report::Args),
    #[clap(about = "Show power shelf health history")]
    HealthHistory(health_history::Args),
}
