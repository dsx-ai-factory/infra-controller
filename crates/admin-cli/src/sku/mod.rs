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

mod assign;
mod bulk_update_metadata;
mod common;
mod create;
mod delete;
mod generate;
mod replace;
mod show;
mod show_machines;
mod unassign;
mod update_metadata;
mod verify;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Cmd {
    #[clap(about = "Show SKU information", visible_alias = "s")]
    Show(show::Args),
    #[clap(about = "Show what machines are assigned a SKU")]
    ShowMachines(show_machines::Args),
    #[clap(
        about = "Generate SKU information from an existing machine",
        visible_alias = "g"
    )]
    Generate(generate::Args),
    #[clap(about = "Create SKUs from a file", visible_alias = "c")]
    Create(create::Args),
    #[clap(about = "Delete a SKU", visible_alias = "d")]
    Delete(delete::Args),
    #[clap(about = "Assign a SKU to a machine", visible_alias = "a")]
    Assign(assign::Args),
    #[clap(about = "Unassign a SKU from a machine", visible_alias = "u")]
    Unassign(unassign::Args),
    #[clap(about = "Verify a machine against its SKU", visible_alias = "v")]
    Verify(verify::Args),
    #[clap(about = "Update the metadata of a SKU")]
    UpdateMetadata(update_metadata::Args),
    #[clap(about = "Update multiple SKU's metadata from a file")]
    BulkUpdateMetadata(bulk_update_metadata::Args),
    #[clap(about = "Replace the component list of a SKU")]
    Replace(replace::Args),
}
