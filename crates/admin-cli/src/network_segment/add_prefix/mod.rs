/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

mod args;
mod cmd;

pub(super) use args::Args;
use eyre::Context;

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        if let Err(error) = self.validate(ctx.config.format) {
            error.exit();
        }
        ctx.assert_cloud_unsafe_op_message()
            .context("confirm the infrastructure prefix mutation")?;
        cmd::add_prefix(self, ctx).await
    }
}
