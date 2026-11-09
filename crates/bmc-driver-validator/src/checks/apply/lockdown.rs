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

//! Lockdown, set as controllers set it.

use std::cell::Cell;
use std::time::Instant;

use bmc_platform::{
    LockdownDesiredState, LockdownScope, LockdownState, LockdownStatus, PlatformError,
};

use crate::checks::context::CheckCx;
use crate::checks::errors::{describe, failed};
use crate::checks::model::Step;
use crate::checks::ops::{Attempt, RETRY_INTERVAL, UNREACHABLE_TIMEOUT, how, poll, took};

pub(in crate::checks) async fn lift_lockdown(cx: &CheckCx<'_>) -> Step {
    set_lockdown(cx, LockdownDesiredState::Disabled).await
}

pub(in crate::checks) async fn enable_lockdown(cx: &CheckCx<'_>) -> Step {
    set_lockdown(cx, LockdownDesiredState::Enabled).await
}

/// Sets lockdown, settles, and converges on the driver's aggregate reading
/// `desired`, as controllers decide; the halves are reported as read. A BMC
/// can refuse the change for minutes after earlier setup steps, so a refusal
/// is asked again as long as an unreachable BMC is, as the machine
/// controller retries until it takes.
async fn set_lockdown(cx: &CheckCx<'_>, desired: LockdownDesiredState) -> Step {
    let bmc = cx.bmc();
    let lockdown = bmc.lockdown()?;
    let target = match desired {
        LockdownDesiredState::Enabled => LockdownState::Enabled,
        LockdownDesiredState::Disabled => LockdownState::Disabled,
    };
    let reached = |status: &LockdownStatus| status.aggregate == target;
    let halves = |status: &LockdownStatus| {
        format!(
            "{:?} · host {:?} · bmc {:?}",
            status.aggregate, status.host, status.bmc
        )
    };
    let status = lockdown.status().await.map_err(failed("status"))?;
    if reached(&status) {
        return Ok(format!("already {}", halves(&status)));
    }
    cx.trace(format!("lockdown reads {}", halves(&status)));
    let started = Instant::now();
    let refusals = Cell::new(0);
    let outcome = poll(
        cx,
        UNREACHABLE_TIMEOUT,
        RETRY_INTERVAL,
        async || match lockdown.set(LockdownScope::All, desired).await {
            Ok(outcome) => Attempt::Done(outcome),
            Err(
                error @ PlatformError::Bmc {
                    status: 400..=499, ..
                },
            ) => {
                refusals.set(refusals.get() + 1);
                Attempt::NotYet(describe("set", &error))
            }
            Err(error) => Attempt::Failed(failed("set")(error)),
        },
    )
    .await?;
    cx.trace(format!("set lockdown {desired:?}: {}", how(&outcome)));
    let refused = match refusals.get() {
        0 => String::new(),
        count => format!(
            ", after the BMC refused it {count} times over {}",
            took(started)
        ),
    };
    let driven = cx.driven();
    let restarted = driven.settle(&outcome).await?;
    let (status, restarted) = driven
        .converge(restarted, async || match lockdown.status().await {
            Ok(status) if reached(&status) => Attempt::Done(status),
            Ok(status) => Attempt::NotYet(format!("lockdown reads {}", halves(&status))),
            Err(error) => Attempt::NotYet(describe("status", &error)),
        })
        .await?;
    Ok(format!(
        "{} in {} ({}{}{refused})",
        halves(&status),
        took(started),
        how(&outcome),
        if restarted {
            ", after a host restart"
        } else {
            ""
        }
    ))
}
