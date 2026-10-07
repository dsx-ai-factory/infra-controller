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

mod common;
mod disable;
mod enable;
mod service_sync;
mod service_version;
mod show;
mod snapshot;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Enable DPF")]
    Enable(enable::Args),
    #[clap(about = "Disable DPF")]
    Disable(disable::Args),
    #[clap(about = "Check Status of DPF")]
    Show(show::Args),
    #[clap(about = "Snapshot DPF CRs (DPUNode, DPUDevices, DPUs) for a host")]
    Snapshot(snapshot::Args),
    #[clap(
        alias = "sv",
        about = "Compare configured vs deployed DPF service versions"
    )]
    ServiceVersion(service_version::Args),
    #[clap(
        subcommand,
        alias = "ss",
        about = "Release DPF maintenance holds blocking a DPUService rollout"
    )]
    ServiceSync(service_sync::Args),
}
