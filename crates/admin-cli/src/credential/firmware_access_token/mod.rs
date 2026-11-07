/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
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
            Self::Set(args) => cmd::set(args, &ctx.api_client).await,
            Self::Delete(args) => cmd::delete(args, &ctx.api_client).await,
        }
    }
}
