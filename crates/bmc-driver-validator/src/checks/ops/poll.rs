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

//! Waiting for a reading to change, and asking again while the BMC cannot
//! answer.

use std::time::Duration;

use crate::checks::context::CheckCx;
use crate::checks::model::Outcome;

/// How long a request keeps being asked again while the BMC cannot answer,
/// and how often.
pub(in crate::checks) const UNREACHABLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub(in crate::checks) const RETRY_INTERVAL: Duration = Duration::from_secs(30);

/// How long settings may take to read back as in effect after a restart,
/// while the host POSTs.
pub(in crate::checks) const SETTLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub(in crate::checks) const SETTLE_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// How long a change that needs no restart, such as a new account, may take
/// to read back.
pub(in crate::checks) const IMMEDIATE_TIMEOUT: Duration = Duration::from_secs(60);
pub(in crate::checks) const QUICK_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// One attempt of a poll.
pub(in crate::checks) enum Attempt<T> {
    Done(T),
    /// Not there yet; the reason is reported if the poll times out.
    NotYet(String),
    Failed(Outcome),
}

/// Retries `attempt` every `interval` until it is done, fails, or
/// `timeout` passes, tracing each reading that is not there yet.
pub(in crate::checks) async fn poll<T>(
    cx: &CheckCx<'_>,
    timeout: Duration,
    interval: Duration,
    attempt: impl AsyncFnMut() -> Attempt<T>,
) -> Result<T, Outcome> {
    poll_traced(|step| cx.trace(step), timeout, interval, attempt).await
}

/// [`poll`], tracing through `trace`.
pub(in crate::checks) async fn poll_traced<T>(
    trace: impl Fn(String),
    timeout: Duration,
    interval: Duration,
    mut attempt: impl AsyncFnMut() -> Attempt<T>,
) -> Result<T, Outcome> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let reason = match attempt().await {
            Attempt::Done(value) => return Ok(value),
            Attempt::Failed(outcome) => return Err(outcome),
            Attempt::NotYet(reason) => reason,
        };
        trace(format!("waiting: {reason}"));
        if tokio::time::Instant::now() + interval > deadline {
            return Err(Outcome::Fail(format!(
                "{reason} after {}s",
                timeout.as_secs()
            )));
        }
        tokio::time::sleep(interval).await;
    }
}

/// Runs `request` again every [`RETRY_INTERVAL`] while it ends unreachable,
/// until it ends otherwise or [`UNREACHABLE_TIMEOUT`] passes. A BMC that
/// cannot answer yet, as while a reset, a settings change, or a host POST
/// occupies it, gets the same wait from the runner and from a check.
pub(in crate::checks) async fn until_answered<T>(
    trace: impl Fn(String),
    mut request: impl AsyncFnMut() -> Result<T, Outcome>,
) -> Result<T, Outcome> {
    let deadline = tokio::time::Instant::now() + UNREACHABLE_TIMEOUT;
    loop {
        let reason = match request().await {
            Err(Outcome::Unreachable(reason)) => reason,
            ended => return ended,
        };
        if tokio::time::Instant::now() + RETRY_INTERVAL > deadline {
            return Err(Outcome::Fail(format!(
                "{reason}; still unavailable after {}s",
                UNREACHABLE_TIMEOUT.as_secs()
            )));
        }
        trace(format!(
            "{reason}; asking again in {}s",
            RETRY_INTERVAL.as_secs()
        ));
        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}
