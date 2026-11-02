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

mod create;
mod routing;
mod set_virtualizer;
mod show;

// Cross-module re-exports for jump module
use clap::Parser;
pub(crate) use show::args::Args as ShowVpc;
pub(crate) use show::cmd::show;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Create VPC")]
    Create(create::Args),
    #[clap(about = "Display VPC information")]
    Show(show::Args),
    #[clap(about = "Inspect the VPC's persisted routing profile and VNI allocations")]
    RoutingState(routing::Show),
    #[clap(about = "Change the routing profile while retaining the previous VNI")]
    ChangeRoutingProfile(routing::ChangeProfile),
    #[clap(about = "Release the inactive VNI after independently verifying convergence")]
    ReleaseInactiveVni(routing::ReleaseInactiveVni),
    SetVirtualizer(set_virtualizer::Args),
}

impl Cmd {
    pub(crate) fn requires_interactive_confirmation(&self) -> bool {
        match self {
            Self::ChangeRoutingProfile(command) => command.if_version_match.is_none(),
            Self::ReleaseInactiveVni(command) => command.if_version_match.is_none(),
            _ => false,
        }
    }
}
