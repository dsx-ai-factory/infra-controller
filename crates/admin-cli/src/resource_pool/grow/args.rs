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

use clap::{ArgGroup, Parser};

#[derive(Parser, Debug)]
#[clap(group(
        ArgGroup::new("grow")
        .required(true)
        .args(&["filename"])))]
#[command(after_long_help = "\
EXAMPLES:

Add capacity to resource pools from a TOML file:
    $ nico-admin-cli resource-pool grow --filename ./grow-pools.toml

")]
pub(crate) struct Args {
    #[clap(short, long)]
    filename: String,
}

impl TryFrom<Args> for ::rpc::forge::GrowResourcePoolRequest {
    type Error = std::io::Error;

    fn try_from(args: Args) -> Result<Self, Self::Error> {
        let text = std::fs::read_to_string(&args.filename)?;
        Ok(Self { text })
    }
}
