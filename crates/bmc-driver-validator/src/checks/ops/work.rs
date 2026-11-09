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

//! Sending a driver request, waiting for the work it started, and reading
//! the change back.

use std::time::Duration;

use bmc_platform::{DriverOutcome, OperationReference, OperationStatus, PlatformError};

use super::poll::{
    Attempt, IMMEDIATE_TIMEOUT, QUICK_POLL_INTERVAL, SETTLE_POLL_INTERVAL, SETTLE_TIMEOUT, poll,
    poll_traced,
};
use super::{Driven, how};
use crate::checks::context::CheckCx;
use crate::checks::errors::{describe, failed};
use crate::checks::model::Outcome;

/// How long accepted work may stay queued or running before or after a reset.
const JOB_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const JOB_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long a request may stay refused because the host is not yet in the
/// power state it needs.
const READY_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// When a change shows in what the BMC reads back.
#[derive(Clone, Copy)]
pub(in crate::checks) enum Effect {
    /// Once the work the request started completes.
    Immediate,
    /// Once that work completes and the host has restarted, which it is
    /// unless the work already restarted it; reads are retried while the
    /// host POSTs.
    OnRestart,
}

/// Sends the driver request `operation`, traced before and after, and
/// returns its outcome or its error as how the check ends.
pub(in crate::checks) async fn requested(
    cx: &CheckCx<'_>,
    operation: &'static str,
    request: impl Future<Output = Result<DriverOutcome, PlatformError>>,
) -> Result<DriverOutcome, Outcome> {
    cx.trace(format!("{operation} …"));
    let outcome = request.await.map_err(failed(operation))?;
    cx.trace(format!("{operation}: {}", how(&outcome)));
    Ok(outcome)
}

/// The outcome of a request that restarts the BMC, traced, which can go
/// down before it answers: an unreachable BMC then means the request was
/// sent, and the caller waits for the BMC to come back.
pub(in crate::checks) async fn sent_or_dropped(
    cx: &CheckCx<'_>,
    operation: &'static str,
    request: impl Future<Output = Result<DriverOutcome, PlatformError>>,
) -> Result<DriverOutcome, Outcome> {
    cx.trace(format!("{operation} …"));
    match request.await {
        Err(PlatformError::Unreachable) => {
            cx.trace(format!(
                "{operation}: the BMC dropped before answering; waiting for it"
            ));
            Ok(DriverOutcome::complete())
        }
        result => {
            let outcome = result.map_err(failed(operation))?;
            cx.trace(format!("{operation}: {}", how(&outcome)));
            Ok(outcome)
        }
    }
}

/// [`sent_or_dropped`] for a request the driver answers
/// [`PlatformError::NotReady`] until the host reaches the power state it
/// needs, such as an AC power cycle on a host not yet off; the request is
/// repeated until it is sent.
pub(in crate::checks) async fn sent_when_ready(
    cx: &CheckCx<'_>,
    operation: &'static str,
    request: impl AsyncFn() -> Result<DriverOutcome, PlatformError>,
) -> Result<DriverOutcome, Outcome> {
    let result = poll(
        cx,
        READY_TIMEOUT,
        QUICK_POLL_INTERVAL,
        async || match request().await {
            Err(PlatformError::NotReady { message }) => Attempt::NotYet(message),
            result => Attempt::Done(result),
        },
    )
    .await?;
    sent_or_dropped(cx, operation, async { result }).await
}

impl Driven<'_> {
    /// Waits for the change that produced `outcome` to take `effect`, then
    /// polls `read` until it shows the change, returning what it saw.
    pub(in crate::checks) async fn read_back<T>(
        &self,
        outcome: &DriverOutcome,
        effect: Effect,
        read: impl AsyncFnMut() -> Attempt<T>,
    ) -> Result<T, Outcome> {
        let (timeout, interval) = match effect {
            Effect::Immediate => {
                self.wait_for_operations(outcome).await?;
                (IMMEDIATE_TIMEOUT, QUICK_POLL_INTERVAL)
            }
            Effect::OnRestart => {
                self.take_effect(outcome).await?;
                (SETTLE_TIMEOUT, SETTLE_POLL_INTERVAL)
            }
        };
        poll(self.cx, timeout, interval, read).await
    }

    /// Polls `reached` briefly, then, unless a restart already happened,
    /// restarts the host and polls while it POSTs: settings staged for the
    /// BIOS take effect on a reset even when the driver reports them
    /// complete. Returns what `reached` saw and whether a restart happened.
    pub(in crate::checks) async fn converge<T>(
        &self,
        restarted: bool,
        reached: impl AsyncFn() -> Attempt<T>,
    ) -> Result<(T, bool), Outcome> {
        if !restarted {
            match poll(
                self.cx,
                IMMEDIATE_TIMEOUT,
                QUICK_POLL_INTERVAL,
                async || reached().await,
            )
            .await
            {
                Ok(value) => return Ok((value, false)),
                Err(Outcome::Fail(reason)) => self.cx.trace(format!(
                    "{reason}; restarting the host in case it applies on reset"
                )),
                Err(other) => return Err(other),
            }
            self.restart_host().await?;
        }
        let value = poll(self.cx, SETTLE_TIMEOUT, SETTLE_POLL_INTERVAL, async || {
            reached().await
        })
        .await?;
        Ok((value, true))
    }

    /// Settles `outcome`, then restarts the host unless settling already
    /// did, so a change staged for the next reset takes effect.
    pub(in crate::checks) async fn take_effect(
        &self,
        outcome: &DriverOutcome,
    ) -> Result<(), Outcome> {
        if !self.settle(outcome).await? {
            self.cx
                .trace("restarting the host so the change takes effect");
            self.restart_host().await?;
        }
        Ok(())
    }

    /// Waits for an outcome's work to finish, restarting the host once when
    /// some of it runs on the next reset; returns whether it restarted.
    pub(in crate::checks) async fn settle(&self, outcome: &DriverOutcome) -> Result<bool, Outcome> {
        let mut awaiting_reset = false;
        for reference in outcome.references() {
            let seen = self.poll_operation(reference, true, async || false).await?;
            self.cx.trace(format!("{}: {seen:?}", reference.uri()));
            awaiting_reset |= seen == Seen::AwaitingReset;
        }
        if awaiting_reset {
            self.cx
                .trace("work runs on the next host reset; restarting the host");
            self.restart_host().await?;
            self.wait_for_operations(outcome).await?;
        }
        Ok(awaiting_reset)
    }

    /// Waits until every reference in `outcome` completes.
    pub(in crate::checks) async fn wait_for_operations(
        &self,
        outcome: &DriverOutcome,
    ) -> Result<(), Outcome> {
        for reference in outcome.references() {
            self.poll_operation(reference, false, async || false)
                .await?;
            self.cx.trace(format!("{}: Completed", reference.uri()));
        }
        Ok(())
    }

    /// [`poll_operation`] on this BMC.
    pub(super) async fn poll_operation(
        &self,
        reference: &OperationReference,
        reset_ends: bool,
        stop: impl AsyncFn() -> bool,
    ) -> Result<Seen, Outcome> {
        poll_operation(
            |step| self.cx.trace(step),
            reference,
            async || self.bmc().operation_status(reference).await,
            reset_ends,
            stop,
        )
        .await
    }
}

/// How [`poll_operation`] last saw the work at a reference.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Seen {
    Completed,
    /// It runs on the next host reset.
    AwaitingReset,
    /// Its status did not read, and the caller's `stop` ended the poll; the
    /// caller knows what that means, such as the BMC having gone down.
    Unreadable,
}

/// Polls `status` for the work at `reference` until it completes, or, with
/// `reset_ends`, waits for the next host reset. Any other ending fails the
/// poll. A failed read is retried unless `stop` ends the poll there.
pub(super) async fn poll_operation(
    trace: impl Fn(String),
    reference: &OperationReference,
    status: impl AsyncFn() -> Result<OperationStatus, PlatformError>,
    reset_ends: bool,
    stop: impl AsyncFn() -> bool,
) -> Result<Seen, Outcome> {
    poll_traced(
        trace,
        JOB_TIMEOUT,
        JOB_POLL_INTERVAL,
        async || match status().await {
            Ok(OperationStatus::Completed) => Attempt::Done(Seen::Completed),
            Ok(OperationStatus::AwaitingReset) if reset_ends => Attempt::Done(Seen::AwaitingReset),
            Ok(state @ (OperationStatus::Running | OperationStatus::AwaitingReset)) => {
                Attempt::NotYet(format!("{} is {state:?}", reference.uri()))
            }
            Ok(ended) => Attempt::Failed(ended_outcome(reference, ended)),
            Err(_) if stop().await => Attempt::Done(Seen::Unreadable),
            Err(error) => Attempt::NotYet(describe("operation status", &error)),
        },
    )
    .await
}

fn ended_outcome(reference: &OperationReference, status: OperationStatus) -> Outcome {
    Outcome::Fail(match status {
        OperationStatus::Failed { state, message } => format!(
            "{} ended {state}: {}",
            reference.uri(),
            message.unwrap_or_default()
        ),
        OperationStatus::NeedsIntervention { state } => {
            format!("{} waits for an operator ({state})", reference.uri())
        }
        other => format!("{} is {other:?}", reference.uri()),
    })
}
