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

use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;

// Dispatch is a trait implemented by all CLI command types.
// It provides a unified interface for executing commands with
// the runtime context.
pub(crate) trait Dispatch {
    fn dispatch(
        self,
        ctx: RuntimeContext,
    ) -> impl std::future::Future<Output = CarbideCliResult<()>>;
}

// Re-export the derive macro so modules can import both the
// trait and derive with: use crate::cfg::dispatch::Dispatch;
pub(crate) use carbide_macros::Dispatch;

/// Bridge a subcommand-less top-level command from `Run` to `Dispatch`, so
/// `main` can dispatch every top-level command the same way.
///
/// Command *groups* derive `Dispatch` (the derive routes each variant to a
/// `Run` leaf, or to a nested `#[dispatch]` group). A top-level command with
/// no subcommands -- `ping`, `version`, `inventory`, `jump`, the `generate-*`
/// trio -- is itself a leaf, so it implements `Run`; this gives it the trivial
/// `Dispatch` impl that just runs it. Reach for this instead of hand-writing
/// the same `self.run(&mut ctx)` wrapper on each one.
macro_rules! dispatch_via_run {
    ($ty:ty) => {
        impl $crate::cfg::dispatch::Dispatch for $ty {
            async fn dispatch(
                self,
                mut ctx: $crate::cfg::runtime::RuntimeContext,
            ) -> $crate::errors::CarbideCliResult<()> {
                $crate::cfg::run::Run::run(self, &mut ctx).await
            }
        }
    };
}
pub(crate) use dispatch_via_run;

#[cfg(test)]
mod tests {
    use super::Dispatch;
    use crate::cfg::run::Run;
    use crate::cfg::runtime::RuntimeContext;
    use crate::errors::CarbideCliResult;

    // Stub leaf command type that implements Run for the purpose
    // of testing our Dispatch + Run trait handling flow.
    struct StubRunArgs;

    impl Run for StubRunArgs {
        async fn run(self, _ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
            Ok(())
        }
    }

    // Stub nested command group that implements Dispatch, also for
    // the purpose of testing our Dispatch + Run trait handling flow.
    struct StubNestedCmd;

    impl Dispatch for StubNestedCmd {
        async fn dispatch(self, _ctx: RuntimeContext) -> CarbideCliResult<()> {
            Ok(())
        }
    }

    // Deriving `Dispatch` on these enums is itself the test: the derive only
    // compiles if it generates a valid `Dispatch` impl. `AllRunCmd` covers the
    // all-leaf case (every variant is a `Run` command); `MixedCmd` covers mixing
    // a leaf command with a nested `#[dispatch]` group. If either impl failed to
    // generate, this module would not compile.
    #[derive(Dispatch)]
    #[allow(dead_code)]
    enum AllRunCmd {
        CmdA(StubRunArgs),
        CmdB(StubRunArgs),
        CmdC(StubRunArgs),
    }

    #[derive(Dispatch)]
    #[allow(dead_code)]
    enum MixedCmd {
        SimpleRunCommand(StubRunArgs),
        #[dispatch]
        NestedCommandGroup(StubNestedCmd),
    }
}
