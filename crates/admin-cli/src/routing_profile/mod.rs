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

mod show;

#[cfg(test)]
mod tests;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
// These inherited global flags are accepted for compatibility but do not affect this command.
#[command(defer = |cmd| cmd.mut_args(|arg| match arg.get_id().as_str() {
    "extended" | "sort_by" => arg.hide(true),
    _ => arg,
}))]
#[command(
    long_about = "Inspect named profiles from the connected Core server's loaded fnn.routing_profiles configuration. This is site-wide configuration inventory, not a tenant's permitted choices or a VPC's effective policy. The inline admin VPC profile is excluded."
)]
pub(crate) enum Cmd {
    /// Show one or more configured routing profile definitions in name order, preserving unset values
    ///
    /// ASCII and CSV output show one summary row per profile unless exactly one
    /// distinct name is explicitly requested. A single explicit name shows all
    /// properties: labeled lines in ASCII, or one row per property in CSV.
    /// Filtering a list down to one result does not select the detailed view.
    /// Unset values are displayed as unset.
    /// JSON and YAML contain a list, including when one name is supplied.
    /// Omitted names fetch matching profiles in batches. All supplied filters
    /// must match, including when explicit names are supplied. Configuration
    /// can change between calls; names removed after discovery are omitted.
    /// Explicit missing names cause an error before any output is written.
    #[command(about = "Show one or more configured routing profile definitions")]
    Show(show::Args),
}
