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

mod attach_vpc;
mod create;
mod delete;
mod show;

// Cross-module re-exports for jump module
pub(crate) use show::args::Args as ShowNetworkSegment;
pub(crate) use show::cmd::handle_show;

#[cfg(test)]
mod tests;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Display Network Segment information")]
    Show(show::Args),
    #[clap(about = "Attach Network Segment to VPC")]
    AttachVpc(attach_vpc::Args),
    #[clap(about = "Delete Network Segment")]
    Delete(delete::Args),
    #[clap(about = "Create Network Segment")]
    Create(create::Args),
}
