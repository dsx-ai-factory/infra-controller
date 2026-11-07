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

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Load bash completions into the current shell (also enables carbide-admin-cli and forge-admin-cli):
    $ source <(nico-admin-cli generate-shell-complete bash)

Load zsh completions into the current shell (also enables carbide-admin-cli and forge-admin-cli):
    $ source <(nico-admin-cli generate-shell-complete zsh)

Write zsh completions to a file on the fpath (run once; re-source your shell):
    $ nico-admin-cli generate-shell-complete zsh > ~/.zfunc/_nico-admin-cli && \
    echo 'compdef _nico-admin-cli carbide-admin-cli forge-admin-cli' >> ~/.zshrc

Write fish completions to the fish completions directory:
    $ nico-admin-cli generate-shell-complete fish > ~/.config/fish/completions/nico-admin-cli.fish

")]
pub(crate) struct Cmd {
    #[clap(subcommand)]
    pub(super) shell: Shell,
}

#[derive(Parser, Debug, Clone)]
#[clap(rename_all = "kebab_case")]
pub(super) enum Shell {
    Bash,
    Fish,
    Zsh,
}
