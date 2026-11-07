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
mod create;
mod delete;
mod show;
mod update;

// Cross-module re-exports for jump module
use clap::Parser;
pub(crate) use show::args::Args as ShowComputeAllocation;
pub(crate) use show::cmd::show;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Clone, Dispatch)]
#[clap(rename_all = "kebab_case")]
pub(crate) enum Cmd {
    #[clap(about = "Create a compute allocation", visible_alias = "c")]
    Create(create::Args),

    #[clap(about = "Show one or more compute allocations", visible_alias = "s")]
    Show(show::Args),

    #[clap(about = "Delete a compute allocation", visible_alias = "d")]
    Delete(delete::Args),

    #[clap(about = "Update a compute allocation", visible_alias = "u")]
    Update(update::Args),
}
