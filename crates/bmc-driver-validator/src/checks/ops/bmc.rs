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

//! The BMC under test going down and coming back: resets, AC power cycles,
//! and reconnecting once rediscovery selects the drivers the run started
//! with. These log in as the run's target and replace its connection, so
//! they apply only to the BMC under test.

use std::time::{Duration, Instant};

use bmc_platform::{DriverOutcome, OperationReference, OperationStatus, PlatformError};

use super::poll::{Attempt, QUICK_POLL_INTERVAL, poll};
use super::work::{Seen, poll_operation, sent_or_dropped};
use super::{how, took};
use crate::checks::context::CheckCx;
use crate::checks::model::{Outcome, Step};

/// How long a BMC may take to lose power after accepting an AC power cycle,
/// or after the work the request started completes; a BMC can accept the
/// request well before it cuts power.
const AC_CYCLE_DOWN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long a reset BMC may keep answering, and then take to answer again.
pub(in crate::checks) const BMC_DOWN_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const BMC_UP_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const BMC_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Resets the BMC, waits for it to go down and come back, and requires
/// rediscovery to select the same drivers.
pub(in crate::checks) async fn reset_bmc(cx: &CheckCx<'_>) -> Step {
    let started = Instant::now();
    let outcome = sent_or_dropped(cx, "reset", cx.bmc().bmc_control()?.reset()).await?;
    wait_for_bmc_down(cx, BMC_DOWN_TIMEOUT).await?;
    wait_for_bmc_up(cx).await?;
    Ok(format!(
        "back {} after the reset ({}); rediscovery selects the same drivers",
        took(started),
        how(&outcome)
    ))
}

/// Waits for the BMC to stop answering, as after it resets or loses power,
/// failing after `timeout`.
pub(in crate::checks) async fn wait_for_bmc_down(
    cx: &CheckCx<'_>,
    timeout: Duration,
) -> Result<(), Outcome> {
    poll(cx, timeout, QUICK_POLL_INTERVAL, async || {
        if answers(cx).await {
            Attempt::NotYet("the BMC still answers".to_string())
        } else {
            Attempt::Done(())
        }
    })
    .await?;
    cx.trace("the BMC stopped answering");
    Ok(())
}

/// Whether the BMC answers a login, refusing it included.
async fn answers(cx: &CheckCx<'_>) -> bool {
    matches!(
        cx.target().log_in().await,
        Ok(()) | Err(PlatformError::Auth(_))
    )
}

/// Waits for the BMC to lose power after accepting an AC power cycle that
/// produced `outcome`. Work the request started, such as a Dell BIOS job that
/// runs during the next POST, cuts power only once it runs, so it is waited
/// for first: a job that fails ends the wait, and the BMC going down while the
/// job runs is the cycle itself. Returns whether all of that work completed
/// before the BMC went down.
pub(in crate::checks) async fn wait_for_ac_cycle_down(
    cx: &CheckCx<'_>,
    outcome: &DriverOutcome,
) -> Result<bool, Outcome> {
    let bmc = cx.driven();
    for reference in outcome.references() {
        let went_down = work_or_bmc_down(
            |step| cx.trace(step),
            reference,
            async || bmc.bmc().operation_status(reference).await,
            async || answers(cx).await,
        )
        .await?;
        if went_down {
            cx.trace("the BMC stopped answering");
            return Ok(false);
        }
        cx.trace(format!("{}: Completed", reference.uri()));
    }
    wait_for_bmc_down(cx, AC_CYCLE_DOWN_TIMEOUT).await?;
    Ok(true)
}

/// Waits for the work an AC power cycle started once the BMC is back. The
/// restart can drop a reference, so one whose status no longer reads while
/// the BMC answers ends with its outcome unknown. Returns those references.
pub(in crate::checks) async fn settle_after_ac_cycle(
    cx: &CheckCx<'_>,
    outcome: &DriverOutcome,
) -> Result<Vec<String>, Outcome> {
    let bmc = cx.driven();
    let mut gone = Vec::new();
    for reference in outcome.references() {
        match bmc
            .poll_operation(reference, false, async || answers(cx).await)
            .await?
        {
            Seen::Unreadable => {
                cx.trace(format!("{}: gone after the BMC restarted", reference.uri()));
                gone.push(reference.uri().to_string());
            }
            _ => cx.trace(format!("{}: Completed", reference.uri())),
        }
    }
    Ok(gone)
}

/// Polls `status` for the work at `reference` until it completes, returning
/// `false`, or until a read fails because the BMC stopped answering, as
/// `answers` tells, returning `true`.
async fn work_or_bmc_down(
    trace: impl Fn(String),
    reference: &OperationReference,
    status: impl AsyncFn() -> Result<OperationStatus, PlatformError>,
    answers: impl AsyncFn() -> bool,
) -> Result<bool, Outcome> {
    poll_operation(trace, reference, status, false, async || !answers().await)
        .await
        .map(|seen| seen == Seen::Unreadable)
}

/// Waits until the BMC logs in again after going down and rediscovery selects
/// the drivers the run started with, then reconnects. A BMC can accept logins
/// before its ServiceRoot reports the product the rules match on, and
/// nv-redfish chooses its quirks from the ServiceRoot it connects with.
pub(in crate::checks) async fn wait_for_bmc_up(cx: &CheckCx<'_>) -> Result<(), Outcome> {
    poll(cx, BMC_UP_TIMEOUT, BMC_POLL_INTERVAL, async || {
        match cx.target().log_in().await {
            Ok(()) => Attempt::Done(()),
            Err(error) => Attempt::NotYet(format!("logging in fails: {error}")),
        }
    })
    .await?;
    poll(cx, BMC_UP_TIMEOUT, BMC_POLL_INTERVAL, async || {
        let discovered = cx
            .target()
            .discover_settled(|reason| cx.trace(format!("waiting: {reason}")))
            .await;
        match discovered {
            Ok((identity, _)) => match cx.selection().changes(&identity) {
                Ok(changes) if changes.is_empty() => Attempt::Done(()),
                Ok(changes) => Attempt::NotYet(format!(
                    "rediscovery selects different drivers: {}",
                    changes.join("; ")
                )),
                Err(reason) => Attempt::Failed(Outcome::Fail(reason)),
            },
            Err(reason) => Attempt::NotYet(format!("rediscovery fails: {reason}")),
        }
    })
    .await?;
    cx.reconnect().await
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use nv_redfish::core::ODataId;

    use super::*;

    /// The work an AC power cycle started is followed until it completes, a
    /// failed read with the BMC gone ends it as the cycle itself, and a failed
    /// job ends it at once.
    #[tokio::test(start_paused = true)]
    async fn the_work_an_ac_cycle_started_is_waited_for() {
        let reference = OperationReference::RedfishTask {
            uri: ODataId::from("/redfish/v1/TaskService/Tasks/JID_1".to_string()),
            retry_after_seconds: None,
            task: None,
        };
        let unreachable = || Err(PlatformError::Unreachable);
        let cases = [
            (
                "completes, the BMC still up",
                vec![Ok(OperationStatus::Running), Ok(OperationStatus::Completed)],
                true,
                Some(false),
            ),
            (
                "the BMC drops while it runs",
                vec![Ok(OperationStatus::Running), unreachable()],
                false,
                Some(true),
            ),
            (
                "a failed read with the BMC up keeps waiting",
                vec![unreachable(), Ok(OperationStatus::Completed)],
                true,
                Some(false),
            ),
            (
                "the job fails",
                vec![Ok(OperationStatus::Failed {
                    state: "Failed".to_string(),
                    message: None,
                })],
                true,
                None,
            ),
        ];
        for (scenario, reads, bmc_answers, expected) in cases {
            let reads = RefCell::new(VecDeque::from(reads));
            let result = work_or_bmc_down(
                |_| {},
                &reference,
                async || {
                    reads
                        .borrow_mut()
                        .pop_front()
                        .unwrap_or(Ok(OperationStatus::Running))
                },
                async || bmc_answers,
            )
            .await;
            assert_eq!(result.ok(), expected, "{scenario}");
        }
    }
}
