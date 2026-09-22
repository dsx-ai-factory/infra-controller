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

mod clear_error;
mod common;
mod copy_bfb_to_dpu_rshim;
mod delete;
mod explore;
mod get_report;
mod have_credentials;
mod is_bmc_in_managed_host;
mod mlx_devices;
mod re_explore;
mod refresh_endpoint;
mod remediation;

use clap::Parser;
// Re-export for cross-module use by jump/cmds.rs
pub(crate) use get_report::args::{Args as GetReportMode, EndpointInfo};
pub(crate) use get_report::cmd::show_discovered_managed_host as show_site_explorer_discovered_managed_host;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Retrieves the latest site exploration report", subcommand)]
    GetReport(get_report::Args),
    #[clap(about = "Report Mellanox/BlueField device NIC firmware from explored Redfish data.")]
    MlxDevices(mlx_devices::Args),
    #[clap(
        about = "Asks carbide-api to explore a single host and prints the report. Does not store it."
    )]
    Explore(explore::Args),
    #[clap(
        about = "Asks carbide-api to explore a single host in the next exploration cycle. The results will be stored."
    )]
    ReExplore(re_explore::Args),
    #[clap(
        name = "refresh",
        about = "Immediately probes a BMC endpoint and persists the report."
    )]
    RefreshEndpoint(refresh_endpoint::Args),
    #[clap(about = "Clear the last known error for the BMC in the latest site exploration report.")]
    ClearError(clear_error::Args),
    #[clap(about = "Delete an explored endpoint from the database.")]
    Delete(delete::Args),
    #[clap(about = "Control remediation actions for an explored endpoint.")]
    Remediation(remediation::Args),
    IsBmcInManagedHost(is_bmc_in_managed_host::Args),
    HaveCredentials(have_credentials::Args),
    CopyBfbToDpuRshim(copy_bfb_to_dpu_rshim::Args),
}
