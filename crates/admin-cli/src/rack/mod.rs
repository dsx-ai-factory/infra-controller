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

mod delete;
mod force_delete;
mod health_history;
mod list;
mod maintenance;
mod metadata;
mod profile;
mod show;
mod state_history;

#[cfg(test)]
mod tests;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Show rack information")]
    Show(show::Args),
    #[clap(about = "List all racks")]
    List(list::Args),
    #[clap(about = "Delete the rack")]
    Delete(delete::Args),
    #[clap(about = "Force delete a rack")]
    ForceDelete(force_delete::Args),
    #[clap(subcommand, about = "Edit Metadata associated with a Rack")]
    Metadata(metadata::Args),
    #[clap(subcommand, about = "Rack profile")]
    Profile(profile::Args),
    #[clap(subcommand, about = "On-demand rack maintenance")]
    Maintenance(maintenance::Args),
    #[clap(about = "Show rack state history")]
    StateHistory(state_history::Args),
    #[clap(about = "Show rack health history")]
    HealthHistory(health_history::Args),
}
