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

use std::io;
use std::io::Write;

use clap::CommandFactory;

use super::args::Shell;
use crate::cfg::cli_options::CliOptions;
use crate::errors::CarbideCliResult;

pub(super) fn generate(shell: Shell) -> CarbideCliResult<()> {
    let mut cmd = CliOptions::command();
    match shell {
        Shell::Bash => {
            clap_complete::generate(
                clap_complete::shells::Bash,
                &mut cmd,
                "nico-admin-cli",
                &mut io::stdout(),
            );
            // Register completions for the short alias and backward-compat symlinks.
            io::stdout().write_all(
                b"complete -F _nico-admin-cli -o nosort -o bashdefault -o default fa\n\
                  complete -F _nico-admin-cli -o nosort -o bashdefault -o default carbide-admin-cli\n\
                  complete -F _nico-admin-cli -o nosort -o bashdefault -o default forge-admin-cli\n",
            )?;
        }
        Shell::Fish => {
            clap_complete::generate(
                clap_complete::shells::Fish,
                &mut cmd,
                "nico-admin-cli",
                &mut io::stdout(),
            );
            // Register completions for backward-compat symlinks by wrapping the canonical command.
            io::stdout().write_all(
                b"complete -c carbide-admin-cli --wraps nico-admin-cli\n\
                  complete -c forge-admin-cli --wraps nico-admin-cli\n",
            )?;
        }
        Shell::Zsh => {
            clap_complete::generate(
                clap_complete::shells::Zsh,
                &mut cmd,
                "nico-admin-cli",
                &mut io::stdout(),
            );
            // Register completions for backward-compat symlinks using the same function.
            io::stdout()
                .write_all(b"compdef _nico-admin-cli carbide-admin-cli forge-admin-cli\n")?;
        }
    }
    Ok(())
}
