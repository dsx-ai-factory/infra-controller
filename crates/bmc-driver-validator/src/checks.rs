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

//! The checks, how a run plans them, and the runner.
//!
//! A check is an id, the capability whose selected driver it exercises, and
//! an async function that calls that driver through the connected BMC the
//! way a controller does. [`reads`] holds the checks a plain run reads with,
//! and [`apply`] checks that change the BMC; [`validate`] arranges both with
//! its own steps into the `--validate` sequence, and `--check` can name any
//! of them.
//! The runner keeps plan order:
//! consecutive read checks run concurrently, every other check runs alone,
//! and a check is skipped when one it follows did not pass.

/// Registers an async check function returning text or a [`model::Detail`].
macro_rules! run {
    ($check:path) => {
        |cx| Box::pin(async move { $check(cx).await.map(Into::into) })
    };
}

mod apply;
mod context;
mod errors;
mod model;
mod ops;
mod plan;
mod reads;
mod runner;
mod text;
mod validate;

pub(crate) use context::{Ctx, Reporter};
pub(crate) use model::{CheckId, Inputs, Kind, Outcome, UefiPassword};
pub(crate) use plan::{Planned, Scope, plan};
pub(crate) use runner::{CheckResult, run};
pub(crate) use validate::EndState;
