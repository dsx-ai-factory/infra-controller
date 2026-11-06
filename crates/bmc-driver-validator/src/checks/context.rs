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

//! The state one run shares between its checks, and the view of it one
//! check runs with.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use arc_swap::ArcSwap;
use bmc_runtime::ConnectedBmc;
use carbide_redfish::nv_redfish::RedfishBmc;

use super::model::{CheckId, Inputs, Outcome};
use super::runner::CheckResult;
use crate::connect::Target;
use crate::selection::Selection;

/// Where the runner and its checks report progress.
pub(crate) trait Reporter {
    fn started(&self, check: CheckId, started: Instant);

    /// One step of `check`, which started at `started`.
    fn step(&self, check: CheckId, started: Instant, step: &str);

    fn finished(&self, check: CheckId);

    /// A check's result, in plan order.
    fn result(&self, result: &CheckResult);

    /// A secret a check made, such as a generated password, to keep out of
    /// what is reported.
    fn secret(&self, secret: &str);
}

/// State shared by every check of one run.
pub(crate) struct Ctx {
    /// The connection checks drive drivers through, replaced when a check
    /// reconnects after the BMC went down.
    bmc: ArcSwap<ConnectedBmc<RedfishBmc>>,
    pub(super) selection: Selection,
    pub(super) target: Target,
    pub(super) inputs: Inputs,
    pub(super) reporter: Arc<dyn Reporter>,
    /// Changes a check made and could not undo.
    not_restored: Mutex<Vec<String>>,
}

impl Ctx {
    pub(crate) fn new(
        bmc: ConnectedBmc<RedfishBmc>,
        selection: Selection,
        target: Target,
        inputs: Inputs,
        reporter: Arc<dyn Reporter>,
    ) -> Self {
        Self {
            bmc: ArcSwap::from_pointee(bmc),
            selection,
            target,
            inputs,
            reporter,
            not_restored: Mutex::new(Vec::new()),
        }
    }

    pub(super) fn not_restored(&self, change: String) {
        self.not_restored
            .lock()
            .expect("not-restored lock")
            .push(change);
    }

    pub(crate) fn unrestored(&self) -> Vec<String> {
        self.not_restored.lock().expect("not-restored lock").clone()
    }
}

/// What one check runs with: the run's state, and the check's own identity
/// for what it reports.
pub(super) struct CheckCx<'a> {
    ctx: &'a Ctx,
    id: CheckId,
    started: Instant,
}

impl<'a> CheckCx<'a> {
    pub(super) fn new(ctx: &'a Ctx, id: CheckId, started: Instant) -> Self {
        Self { ctx, id, started }
    }

    /// The current connection; a handle taken from it keeps the connection
    /// it was taken from across a reconnect.
    pub(super) fn bmc(&self) -> Arc<ConnectedBmc<RedfishBmc>> {
        self.ctx.bmc.load_full()
    }

    pub(super) fn selection(&self) -> &'a Selection {
        &self.ctx.selection
    }

    pub(super) fn target(&self) -> &'a Target {
        &self.ctx.target
    }

    pub(super) fn inputs(&self) -> &'a Inputs {
        &self.ctx.inputs
    }

    /// Reports one step of this check.
    pub(super) fn trace(&self, step: impl AsRef<str>) {
        self.ctx.reporter.step(self.id, self.started, step.as_ref());
    }

    pub(super) fn not_restored(&self, change: String) {
        self.ctx.not_restored(change);
    }

    pub(super) fn secret(&self, secret: &str) {
        self.ctx.reporter.secret(secret);
    }

    /// Connects afresh to the same endpoint, as a controller's next run does
    /// once a BMC that went down answers again, so no session or cached
    /// resource from before carries over.
    pub(super) async fn reconnect(&self) -> Result<(), Outcome> {
        let endpoint = self.bmc().endpoint().clone();
        let bmc = self
            .ctx
            .target
            .connect(endpoint)
            .await
            .map_err(Outcome::Fail)?;
        self.ctx.bmc.store(Arc::new(bmc));
        self.trace("connected afresh");
        Ok(())
    }
}
