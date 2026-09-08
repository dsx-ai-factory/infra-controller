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

mod external_config;
mod on_demand;
mod plugins;
mod results;
mod runs;
mod tests_cmd;

#[cfg(test)]
mod tests;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "External config", subcommand, visible_alias = "mve")]
    ExternalConfig(external_config::Args),
    #[clap(about = "Ondemand Validation", subcommand, visible_alias = "mvo")]
    OnDemand(on_demand::Args),
    #[clap(
        about = "Display machine validation results of individual runs",
        subcommand,
        visible_alias = "mvr"
    )]
    Results(results::Args),
    #[clap(
        about = "Display all machine validation runs",
        subcommand,
        visible_alias = "mvt"
    )]
    Runs(runs::Args),
    #[clap(about = "Supported Tests ", subcommand, visible_alias = "mvs")]
    Tests(tests_cmd::Args),
    #[clap(about = "Manage OCI Machine Validation plugins", subcommand)]
    Plugins(plugins::Args),
}
