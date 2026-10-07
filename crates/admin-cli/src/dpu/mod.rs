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

mod agent_upgrade_policy;
mod health_report;
mod network;
mod reprovision;
mod set_uefi_password;
mod status;
mod versions;

// Cross-module re-exports for machine module
pub(crate) use network::cmd::show_dpu_status;

#[cfg(test)]
mod tests;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(subcommand, about = "DPU Reprovisioning handling")]
    Reprovision(reprovision::Args),
    #[clap(about = "Get or set forge-dpu-agent upgrade policy")]
    AgentUpgradePolicy(agent_upgrade_policy::Args),
    #[clap(about = "View DPU firmware status")]
    Versions(versions::Args),
    #[clap(about = "View DPU Status")]
    Status(status::Args),
    #[clap(subcommand, about = "Networking information")]
    Network(network::Args),
    #[clap(
        about = "Manage DPU health report sources",
        subcommand,
        visible_alias = "hr"
    )]
    HealthReport(health_report::Args),
    #[clap(about = "Set DPU UEFI password directly on the device (via Redfish)")]
    SetUefiPassword(set_uefi_password::Args),
}
