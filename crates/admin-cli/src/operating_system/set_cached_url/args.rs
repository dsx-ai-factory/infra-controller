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

use ::rpc::forge::IpxeTemplateArtifactUpdateRequest;
use clap::Parser;

fn parse_cached_url_update(s: &str) -> Result<IpxeTemplateArtifactUpdateRequest, String> {
    let (name, url) = s
        .split_once('=')
        .ok_or_else(|| format!("expected NAME=URL (or NAME= to clear), got '{s}'"))?;
    let cached_url = if url.is_empty() {
        None
    } else {
        Some(url.to_string())
    };
    Ok(IpxeTemplateArtifactUpdateRequest {
        name: name.to_string(),
        cached_url,
    })
}

#[derive(Parser, Debug, Clone)]
#[command(after_long_help = "\
EXAMPLES:

Set the cached_url for an artifact:
    $ nico-admin-cli operating-system set-cached-url 12345678-1234-5678-90ab-cdef01234567 \
    --set kernel=https://cache.example.com/vmlinuz

Clear the cached_url for an artifact (NAME= with empty URL):
    $ nico-admin-cli operating-system set-cached-url 12345678-1234-5678-90ab-cdef01234567 \
    --set kernel=

")]
pub(crate) struct Args {
    #[clap(help = "UUID of the operating system definition.")]
    pub(super) id: String,

    #[clap(
        long = "set",
        value_name = "NAME=URL",
        value_parser = parse_cached_url_update,
        required = true,
        help = "Set cached_url for an artifact. Use NAME=URL to set, NAME= to clear. May be repeated."
    )]
    pub(super) updates: Vec<IpxeTemplateArtifactUpdateRequest>,
}
