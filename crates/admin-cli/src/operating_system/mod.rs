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
mod get_artifacts;
mod set_cached_url;
mod show;
mod update;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Clone, Dispatch)]
#[clap(rename_all = "kebab_case")]
pub(crate) enum Cmd {
    #[clap(
        about = "Show operating system definitions (all, or one by ID).",
        visible_alias = "s"
    )]
    Show(show::Args),
    #[clap(
        about = "Create a new operating system definition.",
        long_about = "Create a new operating system definition.\n\nExactly one OS variant must be specified: --ipxe-script or --ipxe-template-id.\n\nFor templated iPXE requirements, artifact configuration, and synchronization rules, see docs/configuration/templated-ipxe-operating-systems.md.",
        visible_alias = "c"
    )]
    Create(create::Args),
    #[clap(
        about = "Update an existing operating system definition.",
        long_about = "Update an existing operating system definition.\n\nFor templated iPXE requirements, artifact configuration, and synchronization rules, see docs/configuration/templated-ipxe-operating-systems.md.",
        visible_alias = "u"
    )]
    Update(update::Args),
    #[clap(about = "Delete an operating system definition.", visible_alias = "d")]
    Delete(delete::Args),
    #[clap(
        about = "Get the artifact list for an OS definition.",
        visible_alias = "ga"
    )]
    GetArtifacts(get_artifacts::Args),
    #[clap(
        about = "Set or clear cached_url on OS artifacts.",
        long_about = "Set or clear cached_url on OS artifacts.\n\nFor cache strategies, readiness behavior, and the complete templated iPXE workflow, see docs/configuration/templated-ipxe-operating-systems.md.",
        visible_alias = "scu"
    )]
    SetCachedUrl(set_cached_url::Args),
}
