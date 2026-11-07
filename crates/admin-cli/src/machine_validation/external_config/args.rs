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

#[derive(Parser, Debug)]
#[command(after_long_help = "\
EXAMPLES:

Show external config entries by name:
    $ nico-admin-cli machine-validation external-config show --name my-config

Add or update an external config from a file:
    $ nico-admin-cli machine-validation external-config add-update --name my-config \
    --file-name ./config.toml --description \"validation overrides\"

Remove an external config:
    $ nico-admin-cli machine-validation external-config remove --name my-config

")]
pub(crate) enum Args {
    #[clap(about = "Show External config")]
    Show(ExternalConfigShowOptions),

    #[clap(about = "Update External config")]
    AddUpdate(ExternalConfigAddOptions),

    #[clap(about = "Remove External config")]
    Remove(ExternalConfigRemoveOptions),
}

#[derive(Parser, Debug)]
pub(crate) struct ExternalConfigShowOptions {
    #[clap(short, long, help = "Machine validation external config names")]
    pub(super) name: Vec<String>,
}

#[derive(Parser, Debug)]
pub(crate) struct ExternalConfigAddOptions {
    #[clap(short, long, help = "Name of the file to update")]
    pub(super) file_name: String,
    #[clap(short, long, help = "Name of the config")]
    pub(super) name: String,
    #[clap(short, long, help = "description of the file to update")]
    pub(super) description: String,
}

#[derive(Parser, Debug)]
pub(crate) struct ExternalConfigRemoveOptions {
    #[clap(short, long, help = "Machine validation external config name")]
    pub(super) name: String,
}
