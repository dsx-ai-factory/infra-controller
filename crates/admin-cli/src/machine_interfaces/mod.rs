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

mod assign_address;
mod delete;
mod remove_address;
mod show;
mod show_addresses;

// Cross-module re-exports for jump module
use clap::Parser;
pub(crate) use show::args::Args as ShowMachineInterfaces;
pub(crate) use show::cmd::handle_show;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "List of all Machine interfaces")]
    Show(show::Args),
    #[clap(about = "Delete Machine interface.")]
    Delete(delete::Args),
    #[clap(about = "Show addresses for a machine interface")]
    ShowAddresses(show_addresses::Args),
    #[clap(about = "Assign a static address to a machine interface")]
    AssignAddress(assign_address::Args),
    #[clap(about = "Remove a static address from a machine interface")]
    RemoveAddress(remove_address::Args),
}
