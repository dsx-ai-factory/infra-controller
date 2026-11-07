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

mod add;
pub(crate) mod common;
mod delete;
mod erase;
mod replace_all;
mod show;
mod update;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Show expected switch")]
    Show(show::Args),
    #[clap(about = "Add expected switch")]
    Add(add::Args),
    #[clap(about = "Delete expected switch")]
    Delete(delete::Args),
    #[clap(about = "Update expected switch")]
    Update(update::Args),
    #[clap(about = "Replace all expected switches")]
    ReplaceAll(replace_all::Args),
    #[clap(about = "Erase all expected switches")]
    Erase(erase::Args),
}
