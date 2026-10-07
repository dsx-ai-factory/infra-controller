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

use clap::Parser;

use super::{add, print_empty_template, remove, show};
use crate::cfg::dispatch::Dispatch;

#[derive(Parser, Debug, Dispatch)]
pub(crate) enum Args {
    #[clap(about = "List health report sources for a switch")]
    Show(show::Args),
    #[clap(about = "Insert a health report source for a switch")]
    Add(add::Args),
    #[clap(about = "Print an empty health report template")]
    PrintEmptyTemplate(print_empty_template::Args),
    #[clap(about = "Remove a health report source from a switch")]
    Remove(remove::Args),
}
