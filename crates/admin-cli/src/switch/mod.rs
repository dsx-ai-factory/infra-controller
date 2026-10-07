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

mod force_delete;
mod health_history;
mod health_report;
mod list;
mod metadata;
mod show;

#[cfg(test)]
mod tests;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Show switch information")]
    Show(show::Args),
    #[clap(about = "List all switches")]
    List(list::Args),
    #[clap(about = "Force delete a switch and optionally its interfaces")]
    ForceDelete(force_delete::Args),
    #[clap(subcommand, about = "Manage Switch Metadata")]
    Metadata(metadata::Args),
    #[dispatch]
    #[clap(
        about = "Manage health report sources",
        subcommand,
        visible_alias = "hr"
    )]
    HealthReport(health_report::Args),
    #[clap(about = "Show switch health history")]
    HealthHistory(health_history::Args),
}
