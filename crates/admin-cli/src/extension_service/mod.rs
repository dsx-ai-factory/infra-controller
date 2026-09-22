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
mod create;
mod delete;
mod get_version;
pub(crate) mod show;
mod show_instances;
mod update;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Create an extension service")]
    Create(create::Args),
    #[clap(about = "Update an extension service")]
    Update(update::Args),
    #[clap(about = "Delete an extension service")]
    Delete(delete::Args),
    #[clap(about = "Show extension service information")]
    Show(show::Args),
    #[clap(about = "Get extension service version information")]
    GetVersion(get_version::Args),
    #[clap(about = "Show instances using an extension service")]
    ShowInstances(show_instances::Args),
}
