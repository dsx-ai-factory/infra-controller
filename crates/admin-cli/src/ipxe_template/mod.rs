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

mod show;

use clap::Parser;

use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Clone, Dispatch)]
#[clap(rename_all = "kebab_case")]
pub(crate) enum Cmd {
    #[clap(
        about = "Show iPXE templates (all, or one by ID).",
        long_about = "Show iPXE templates (all, or one by ID).\n\nFor the relationship between templates, Operating System definitions, Sites, and Instances, see docs/configuration/templated-ipxe-operating-systems.md.",
        visible_alias = "s"
    )]
    Show(show::Args),
}
