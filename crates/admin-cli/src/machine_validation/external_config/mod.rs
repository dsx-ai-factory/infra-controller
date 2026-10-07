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

mod args;
mod cmd;

pub(super) use args::Args;

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        match self {
            Args::Show(opts) => {
                cmd::external_config_show(
                    &ctx.api_client,
                    opts.name,
                    ctx.config.extended,
                    ctx.config.format,
                )
                .await
            }
            Args::AddUpdate(opts) => {
                cmd::external_config_add_update(
                    &ctx.api_client,
                    opts.name,
                    opts.file_name,
                    opts.description,
                )
                .await
            }
            Args::Remove(opts) => cmd::remove_external_config(&ctx.api_client, opts.name).await,
        }
    }
}
