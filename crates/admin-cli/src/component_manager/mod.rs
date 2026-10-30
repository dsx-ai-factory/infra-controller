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

pub(crate) mod common;
mod configure_switch_certificate;
mod power_control;
mod sdn_factory_reset;
mod status;
mod update_firmware;
mod versions;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Queue component firmware updates")]
    UpdateFirmware(update_firmware::Args),

    #[clap(
        about = "Get component firmware update status",
        visible_alias = "status"
    )]
    GetFirmwareUpdateStatus(status::Args),

    #[clap(
        about = "List available component firmware versions",
        visible_alias = "versions"
    )]
    GetFirmwareVersions(versions::Args),

    #[clap(
        about = "Issue a power-control action against components (switches, power shelves, compute trays)",
        visible_alias = "power-control"
    )]
    ComponentPowerControl(power_control::Args),

    #[clap(
        about = "Rotate or reinstall switch NVOS mTLS certificates via the switch Maintenance phase",
        visible_alias = "rotate-switch-certificate"
    )]
    ConfigureSwitchCertificate(configure_switch_certificate::Args),

    #[clap(about = "Reset switch SDN configuration directly through RMS (disrupts fabric traffic)")]
    ResetSwitchSdnFactoryDefault(sdn_factory_reset::ResetArgs),

    #[clap(about = "Get the parent and child job result for a switch SDN factory reset")]
    GetSwitchSdnFactoryResetStatus(sdn_factory_reset::StatusArgs),
}
