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

//! Operations checks share: polling, making a change and reading it back,
//! waiting on the work a driver started, host restarts and power
//! transitions, BMC resets, and settings that take effect on a restart.

use std::cell::Cell;
use std::time::{Duration, Instant};

use bmc_platform::{
    BiosStatus, DriverOutcome, LockdownDesiredState, LockdownScope, LockdownState, LockdownStatus,
    OperationReference, OperationStatus, PlatformError,
};
use bmc_runtime::ConnectedBmc;
use carbide_redfish::nv_redfish::RedfishBmc;
use nv_redfish::resource::{PowerState, ResetType};

use super::context::CheckCx;
use super::errors::{describe, failed};
use super::model::{Outcome, Step};
use super::runner::{RETRY_INTERVAL, UNREACHABLE_TIMEOUT};

/// How long accepted work may stay queued or running before or after a reset.
const JOB_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const JOB_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// How long a power transition may take to show in the reported state.
const POWER_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How long settings may take to read back as in effect after a restart,
/// while the host POSTs.
pub(super) const SETTLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);
pub(super) const SETTLE_POLL_INTERVAL: Duration = Duration::from_secs(15);

/// How long a change that needs no restart, such as a new account, may take
/// to read back.
pub(super) const IMMEDIATE_TIMEOUT: Duration = Duration::from_secs(60);
pub(super) const QUICK_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// How long a BMC may take to lose power after accepting an AC power cycle,
/// or after the work the request started completes; a BMC can accept the
/// request well before it cuts power.
const AC_CYCLE_DOWN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long a reset BMC may keep answering, and then take to answer again.
const BMC_DOWN_TIMEOUT: Duration = Duration::from_secs(2 * 60);
const BMC_UP_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const BMC_POLL_INTERVAL: Duration = Duration::from_secs(10);

/// Whether every attribute `status` finds out of place is one the BIOS does
/// not report, as when it has reported only part of its attributes since the
/// BMC restarted; the rest return once the host starts again.
pub(super) fn only_unreported_differ(status: &BiosStatus) -> bool {
    !status.differences.is_empty() && status.differences.iter().all(|diff| diff.actual.is_none())
}

/// One attempt of a poll.
pub(super) enum Attempt<T> {
    Done(T),
    /// Not there yet; the reason is reported if the poll times out.
    NotYet(String),
    Failed(Outcome),
}

/// When a change shows in what the BMC reads back.
#[derive(Clone, Copy)]
pub(super) enum Effect {
    /// Once the work the request started completes.
    Immediate,
    /// Once that work completes and the host has restarted, which it is
    /// unless the work already restarted it; reads are retried while the
    /// host POSTs.
    OnRestart,
}

/// Retries `attempt` every `interval` until it is done, fails, or
/// `timeout` passes, tracing each reading that is not there yet.
pub(super) async fn poll<T>(
    cx: &CheckCx<'_>,
    timeout: Duration,
    interval: Duration,
    attempt: impl AsyncFnMut() -> Attempt<T>,
) -> Result<T, Outcome> {
    poll_traced(|step| cx.trace(step), timeout, interval, attempt).await
}

/// [`poll`], tracing through `trace`.
pub(super) async fn poll_traced<T>(
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

/// Sends the driver request `operation`, traced before and after, and
/// returns its outcome or its error as how the check ends.
pub(super) async fn requested(
    cx: &CheckCx<'_>,
    operation: &'static str,
    request: impl Future<Output = Result<DriverOutcome, PlatformError>>,
) -> Result<DriverOutcome, Outcome> {
    cx.trace(format!("{operation} …"));
    let outcome = request.await.map_err(failed(operation))?;
    cx.trace(format!("{operation}: {}", how(&outcome)));
    Ok(outcome)
}

/// Waits for the change that produced `outcome` to take `effect`, then
/// polls `read` until it shows the change, returning what it saw.
pub(super) async fn read_back<T>(
    cx: &CheckCx<'_>,
    outcome: &DriverOutcome,
    effect: Effect,
    read: impl AsyncFnMut() -> Attempt<T>,
) -> Result<T, Outcome> {
    let (timeout, interval) = match effect {
        Effect::Immediate => {
            wait_for_operations(cx, outcome).await?;
            (IMMEDIATE_TIMEOUT, QUICK_POLL_INTERVAL)
        }
        Effect::OnRestart => {
            take_effect(cx, outcome).await?;
            (SETTLE_TIMEOUT, SETTLE_POLL_INTERVAL)
        }
    };
    poll(cx, timeout, interval, read).await
}

/// Polls `reached` briefly, then, unless a restart already happened,
/// restarts the host and polls while it POSTs: settings staged for the BIOS
/// take effect on a reset even when the driver reports them complete.
/// Returns what `reached` saw and whether a restart happened.
pub(super) async fn converge<T>(
    cx: &CheckCx<'_>,
    restarted: bool,
    reached: impl AsyncFn() -> Attempt<T>,
) -> Result<(T, bool), Outcome> {
    if !restarted {
        match poll(cx, IMMEDIATE_TIMEOUT, QUICK_POLL_INTERVAL, async || {
            reached().await
        })
        .await
        {
            Ok(value) => return Ok((value, false)),
            Err(Outcome::Fail(reason)) => cx.trace(format!(
                "{reason}; restarting the host in case it applies on reset"
            )),
            Err(other) => return Err(other),
        }
        restart_host(cx).await?;
    }
    let value = poll(cx, SETTLE_TIMEOUT, SETTLE_POLL_INTERVAL, async || {
        reached().await
    })
    .await?;
    Ok((value, true))
}

/// Settles `outcome`, then restarts the host unless settling already did,
/// so a change staged for the next reset takes effect.
pub(super) async fn take_effect(cx: &CheckCx<'_>, outcome: &DriverOutcome) -> Result<(), Outcome> {
    if !settle(cx, outcome).await? {
        cx.trace("restarting the host so the change takes effect");
        restart_host(cx).await?;
    }
    Ok(())
}

/// Waits for an outcome's work to finish, restarting the host once when
/// some of it runs on the next reset; returns whether it restarted.
pub(super) async fn settle(cx: &CheckCx<'_>, outcome: &DriverOutcome) -> Result<bool, Outcome> {
    let mut awaiting_reset = false;
    for reference in outcome.references() {
        let seen = poll_operation(
            |step| cx.trace(step),
            reference,
            async || cx.bmc().operation_status(reference).await,
            true,
            async || false,
        )
        .await?;
        cx.trace(format!("{}: {seen:?}", reference.uri()));
        awaiting_reset |= seen == Seen::AwaitingReset;
    }
    if awaiting_reset {
        cx.trace("work runs on the next host reset; restarting the host");
        restart_host(cx).await?;
        wait_for_operations(cx, outcome).await?;
    }
    Ok(awaiting_reset)
}

/// Waits until every reference in `outcome` completes.
pub(super) async fn wait_for_operations(
    cx: &CheckCx<'_>,
    outcome: &DriverOutcome,
) -> Result<(), Outcome> {
    for reference in outcome.references() {
        poll_operation(
            |step| cx.trace(step),
            reference,
            async || cx.bmc().operation_status(reference).await,
            false,
            async || false,
        )
        .await?;
        cx.trace(format!("{}: Completed", reference.uri()));
    }
    Ok(())
}

/// How [`poll_operation`] last saw the work at a reference.
#[derive(Debug, PartialEq, Eq)]
enum Seen {
    Completed,
    /// It runs on the next host reset.
    AwaitingReset,
    /// A read failed and the BMC no longer answers.
    BmcDown,
}

/// Polls `status` for the work at `reference` until it completes, or, with
/// `reset_ends`, waits for the next host reset. Any other ending fails the
/// poll. A failed read is retried unless `bmc_down` says the BMC stopped
/// answering, which ends the poll.
async fn poll_operation(
    trace: impl Fn(String),
    reference: &OperationReference,
    status: impl AsyncFn() -> Result<OperationStatus, PlatformError>,
    reset_ends: bool,
    bmc_down: impl AsyncFn() -> bool,
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
            Err(_) if bmc_down().await => Attempt::Done(Seen::BmcDown),
            Err(error) => Attempt::NotYet(describe("operation status", &error)),
        },
    )
    .await
}

/// Restarts the host through the selected power driver, or powers it on
/// when it is off, and waits until it reports on.
pub(super) async fn restart_host(cx: &CheckCx<'_>) -> Result<(), Outcome> {
    let bmc = cx.bmc();
    let reset = match power_state(&bmc).await.map_err(failed("state"))? {
        Some(PowerState::Off) => ResetType::On,
        _ => ResetType::ForceRestart,
    };
    set_power(cx, &bmc, reset).await.map(drop)
}

/// The host power state through `bmc`'s power driver, read afresh.
pub(super) async fn power_state(
    bmc: &ConnectedBmc<RedfishBmc>,
) -> Result<Option<PowerState>, PlatformError> {
    bmc.power()
        .map_err(|_| PlatformError::Unsupported)?
        .state()
        .await
}

/// Requests `reset` through `bmc`'s power driver and waits for the state it
/// leads to.
pub(super) async fn set_power(
    cx: &CheckCx<'_>,
    bmc: &ConnectedBmc<RedfishBmc>,
    reset: ResetType,
) -> Step {
    let target = match reset {
        ResetType::ForceOff | ResetType::GracefulShutdown => PowerState::Off,
        _ => PowerState::On,
    };
    let started = Instant::now();
    let outcome = bmc.power()?.set(reset).await.map_err(failed("set"))?;
    cx.trace(format!("power {reset:?}: {}", how(&outcome)));
    wait_for_operations(cx, &outcome).await?;
    wait_for_power(cx, bmc, target).await?;
    cx.trace(format!("power reads {target:?}"));
    Ok(format!("{reset:?} reached {target:?} in {}", took(started)))
}

pub(super) async fn wait_for_power(
    cx: &CheckCx<'_>,
    bmc: &ConnectedBmc<RedfishBmc>,
    target: PowerState,
) -> Result<(), Outcome> {
    poll(
        cx,
        POWER_TIMEOUT,
        QUICK_POLL_INTERVAL,
        async || match power_state(bmc).await {
            Ok(Some(state)) if state == target => Attempt::Done(()),
            Ok(state) => Attempt::NotYet(format!("host reports power {state:?}, not {target:?}")),
            Err(error) => Attempt::NotYet(describe("power state", &error)),
        },
    )
    .await
}

/// Resets the BMC, waits for it to go down and come back, and requires
/// rediscovery to select the same drivers.
pub(super) async fn reset_bmc(cx: &CheckCx<'_>) -> Step {
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

/// The outcome of a request that restarts the BMC, traced, which can go
/// down before it answers: an unreachable BMC then means the request was
/// sent, and the caller waits for the BMC to come back.
pub(super) async fn sent_or_dropped(
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
pub(super) async fn sent_when_ready(
    cx: &CheckCx<'_>,
    operation: &'static str,
    request: impl AsyncFn() -> Result<DriverOutcome, PlatformError>,
) -> Result<DriverOutcome, Outcome> {
    let result = poll(
        cx,
        POWER_TIMEOUT,
        QUICK_POLL_INTERVAL,
        async || match request().await {
            Err(PlatformError::NotReady { message }) => Attempt::NotYet(message),
            result => Attempt::Done(result),
        },
    )
    .await?;
    sent_or_dropped(cx, operation, async { result }).await
}

/// Leaves the host on: powers it on when it reads off, otherwise waits for
/// on. Reads are retried while the BMC is still coming back.
pub(super) async fn power_on(cx: &CheckCx<'_>) -> Result<(), Outcome> {
    let bmc = cx.bmc();
    let state = poll(
        cx,
        POWER_TIMEOUT,
        QUICK_POLL_INTERVAL,
        async || match power_state(&bmc).await {
            Ok(state) => Attempt::Done(state),
            Err(error) => Attempt::NotYet(describe("power state", &error)),
        },
    )
    .await?;
    match state {
        Some(PowerState::Off) => set_power(cx, &bmc, ResetType::On).await.map(drop),
        _ => wait_for_power(cx, &bmc, PowerState::On).await,
    }
}

/// Waits for the BMC to stop answering, as after it resets or loses power,
/// failing after `timeout`.
pub(super) async fn wait_for_bmc_down(cx: &CheckCx<'_>, timeout: Duration) -> Result<(), Outcome> {
    poll(cx, timeout, QUICK_POLL_INTERVAL, async || {
        if bmc_answers(cx).await {
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
async fn bmc_answers(cx: &CheckCx<'_>) -> bool {
    matches!(
        cx.target().log_in().await,
        Ok(()) | Err(PlatformError::Auth(_))
    )
}

/// Waits for the BMC to lose power after accepting an AC power cycle that
/// produced `outcome`. Work the request started, such as a Dell BIOS job that
/// runs during the next POST, cuts power only once it runs, so it is waited
/// for first: a job that fails ends the wait, and the BMC going down while the
/// job runs is the cycle itself.
pub(super) async fn wait_for_ac_cycle_down(
    cx: &CheckCx<'_>,
    outcome: &DriverOutcome,
) -> Result<(), Outcome> {
    for reference in outcome.references() {
        let went_down = work_or_bmc_down(
            |step| cx.trace(step),
            reference,
            async || cx.bmc().operation_status(reference).await,
            async || bmc_answers(cx).await,
        )
        .await?;
        if went_down {
            cx.trace("the BMC stopped answering");
            return Ok(());
        }
        cx.trace(format!("{}: Completed", reference.uri()));
    }
    wait_for_bmc_down(cx, AC_CYCLE_DOWN_TIMEOUT).await
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
        .map(|seen| seen == Seen::BmcDown)
}

/// Waits until the BMC logs in again after going down and rediscovery selects
/// the drivers the run started with, then reconnects. A BMC can accept logins
/// before its ServiceRoot reports the product the rules match on, and
/// nv-redfish chooses its quirks from the ServiceRoot it connects with.
pub(super) async fn wait_for_bmc_up(cx: &CheckCx<'_>) -> Result<(), Outcome> {
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

/// Sets lockdown, settles, and converges on the driver's aggregate reading
/// `desired`, as controllers decide; the halves are reported as read. A BMC
/// can refuse the change for minutes after earlier setup steps, so a refusal
/// is asked again as long as an unreachable BMC is, as the machine
/// controller retries until it takes.
pub(super) async fn set_lockdown(cx: &CheckCx<'_>, desired: LockdownDesiredState) -> Step {
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
    let restarted = settle(cx, &outcome).await?;
    let (status, restarted) = converge(cx, restarted, async || match lockdown.status().await {
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

/// How the driver says a request completed: at once, or by starting work.
pub(super) fn how(outcome: &DriverOutcome) -> String {
    match outcome {
        DriverOutcome::Complete => "applied at once".to_string(),
        accepted => format!("started {}", uris(accepted)),
    }
}

pub(super) fn took(started: Instant) -> String {
    format!("{:.1}s", started.elapsed().as_secs_f64())
}

pub(super) fn uris(outcome: &DriverOutcome) -> String {
    outcome
        .references()
        .map(|reference| reference.uri().to_string())
        .collect::<Vec<_>>()
        .join(", ")
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;

    use bmc_platform::BiosDiff;
    use nv_redfish::core::ODataId;

    use super::*;

    /// A BIOS that reports none of the attributes that differ has not
    /// reported yet; one that reports any of them, or where nothing differs,
    /// has.
    #[test]
    fn only_unreported_attributes_differing_means_the_bios_has_not_reported() {
        let diff = |actual: Option<&str>| BiosDiff {
            key: "Attribute".to_string(),
            expected: serde_json::json!("Expected"),
            actual: actual.map(|value| serde_json::json!(value)),
        };
        let status = |differences: Vec<BiosDiff>| BiosStatus {
            is_applied: differences.is_empty(),
            differences,
        };
        for (scenario, differences, unreported) in [
            ("nothing differs", vec![], false),
            ("none reported", vec![diff(None), diff(None)], true),
            ("one reported", vec![diff(None), diff(Some("Other"))], false),
        ] {
            assert_eq!(
                only_unreported_differ(&status(differences)),
                unreported,
                "{scenario}"
            );
        }
    }

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
