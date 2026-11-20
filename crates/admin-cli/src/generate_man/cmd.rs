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
use clap_mangen::roff::{Roff, roman};
use eyre::WrapErr;

use crate::cfg::cli_options::CliOptions;
use crate::errors::CarbideCliResult;

pub(super) fn generate(out_dir: &Path) -> CarbideCliResult<()> {
    std::fs::create_dir_all(out_dir)?;
    let mut cmd = CliOptions::command()
        .name("nico-admin-cli")
        .disable_help_subcommand(true);
    cmd.build();
    clap_mangen::generate_to(cmd.clone(), out_dir).wrap_err("generate CLI man pages")?;

    // Use the explicit Clap usages to preserve the required root acknowledgement and selector groups.
    let instance = cmd
        .find_subcommand("instance")
        .ok_or_else(|| eyre::eyre!("missing instance command"))?;
    for name in ["allocate", "update-extension-services"] {
        let mut command = instance
            .find_subcommand(name)
            .cloned()
            .ok_or_else(|| eyre::eyre!("missing instance {name} command"))?;
        let usage = command.render_usage().to_string();
        let usage = usage.strip_prefix("Usage: ").unwrap_or(&usage);
        let man = clap_mangen::Man::new(command);
        let path = out_dir.join(man.get_filename());
        let generated = std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("read instance {name} man page"))?;
        let (before, sections) = generated
            .split_once(".SH SYNOPSIS\n")
            .ok_or_else(|| eyre::eyre!("missing synopsis in {}", path.display()))?;
        let (_, description) = sections
            .split_once(".SH DESCRIPTION\n")
            .ok_or_else(|| eyre::eyre!("missing description in {}", path.display()))?;
        let synopsis = Roff::new().text([roman(usage)]).to_roff();
        std::fs::write(
            &path,
            format!("{before}.SH SYNOPSIS\n{synopsis}.SH DESCRIPTION\n{description}"),
        )
        .wrap_err_with(|| format!("write instance {name} man page synopsis"))?;
    }
    Ok(())
}
