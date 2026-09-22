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

mod ensure;
mod show;

// Cross-module re-exports for jump module.
use clap::Parser;
pub(crate) use show::args::Args as ShowDpa;
pub(crate) use show::cmd::show;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Create/ensure a DPA interface")]
    Ensure(ensure::Args),
    #[clap(about = "Display Dpa information")]
    Show(show::Args),
}
