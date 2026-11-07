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

use std::path::Path;

use clap::CommandFactory;

use crate::cfg::cli_options::CliOptions;
use crate::errors::CarbideCliResult;

pub(super) fn generate(out_dir: &Path) -> CarbideCliResult<()> {
    std::fs::create_dir_all(out_dir)?;
    let cmd = CliOptions::command().name("nico-admin-cli");
    clap_mangen::generate_to(cmd, out_dir)?;
    Ok(())
}
