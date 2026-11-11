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

//! Runs a plan in order, reporting each result as it finishes.

use std::time::{Duration, Instant};

use futures::StreamExt;
use tokio::time::error::Elapsed;

use super::context::{CheckCx, Ctx};
use super::model::{Check, CheckId, Checked, Kind, Outcome};
use super::ops::until_answered;
use super::plan::Planned;

#[derive(Clone)]
pub(crate) struct CheckResult {
    pub(crate) id: CheckId,
    pub(crate) driver: String,
    pub(crate) outcome: Outcome,
    pub(crate) elapsed: Duration,
}

/// Runs the plan in order, consecutive read checks `jobs` at a time.
pub(crate) async fn run(ctx: &Ctx, plan: Vec<Planned>, jobs: usize) -> Vec<CheckResult> {
    let mut results: Vec<CheckResult> = Vec::new();
    let mut plan = plan.into_iter().peekable();
    while let Some(first) = plan.next() {
        let mut batch = vec![gated(first, &results)];
        if batch[0].check.kind == Kind::Read {
            while let Some(next) = plan.next_if(|planned| planned.check.kind == Kind::Read) {
                batch.push(gated(next, &results));
            }
        }
        let earlier = results.clone();
        let mut running = futures::stream::iter(
            batch
                .into_iter()
                .map(|planned| run_one(ctx, &earlier, planned)),
        )
        .buffered(jobs);
        while let Some(result) = running.next().await {
            ctx.reporter.result(&result);
            results.push(result);
        }
    }
    results
}

/// Blocks `planned` when a check it follows failed or was blocked.
fn gated(planned: Planned, results: &[CheckResult]) -> Planned {
    if planned.skip.is_some() {
        return planned;
    }
    let blocker = planned.after.iter().find(|id| {
        results
            .iter()
            .any(|result| result.id == **id && result.outcome.blocks())
    });
    match blocker {
        Some(id) => Planned {
            skip: Some(Outcome::Blocked(format!("{id} did not pass"))),
            ..planned
        },
        None => planned,
    }
}

async fn run_one(ctx: &Ctx, earlier: &[CheckResult], planned: Planned) -> CheckResult {
    let Planned { check, skip, .. } = planned;
    let started = Instant::now();
    let outcome = match skip {
        Some(outcome) => outcome,
        None => {
            ctx.reporter.started(check.id, started);
            let cx = CheckCx::new(ctx, earlier, check.id, started);
            let answered = run_until_answered(
                &check,
                async || (check.run)(&cx).await,
                |step| cx.trace(step),
            )
            .await;
            ctx.reporter.finished(check.id);
            answered.unwrap_or_else(|_| {
                if check.kind == Kind::Apply {
                    ctx.not_restored(format!(
                        "{} was cut off; anything it changed may not be restored",
                        check.id
                    ));
                }
                Outcome::Fail(format!(
                    "did not finish within {}s",
                    check.timeout.as_secs()
                ))
            })
        }
    };
    CheckResult {
        id: check.id,
        driver: ctx.selection.driver(check.id.capability),
        outcome,
        elapsed: started.elapsed(),
    }
}

/// Runs `attempt` until it answers, as [`until_answered`] runs a request,
/// unless `check` is not safe to repeat: then an unreachable BMC leaves it
/// unconfirmed. The check's own call is the probe: a BMC can accept logins
/// well before its other services answer. Fails with [`Elapsed`] when an
/// attempt outlasts the check's timeout.
async fn run_until_answered(
    check: &Check,
    attempt: impl AsyncFn() -> Checked,
    trace: impl Fn(String),
) -> Result<Outcome, Elapsed> {
    let mut cut_off = None;
    let answered = until_answered(trace, async || {
        match tokio::time::timeout(check.timeout, attempt()).await {
            Ok(Err(Outcome::Unreachable(reason))) if !check.rerun => Err(Outcome::Unconfirmed(
                format!("{reason}; not run again, since repeating it could redo a change"),
            )),
            Ok(checked) => checked,
            Err(elapsed) => {
                let reason = elapsed.to_string();
                cut_off = Some(elapsed);
                Err(Outcome::Fail(reason))
            }
        }
    })
    .await;
    match cut_off {
        Some(elapsed) => Err(elapsed),
        None => Ok(answered.map_or_else(|outcome| outcome, Outcome::Pass)),
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use bmc_platform::Capability;

    use super::*;
    use crate::checks::model::Step;

    const FIRST: CheckId = CheckId::new(Capability::Power, "first");

    async fn unused(_: &CheckCx<'_>) -> Step {
        unreachable!("the runner tests drive attempts themselves")
    }

    fn check() -> Check {
        Check::read(CheckId::new(Capability::Power, "check"), run!(unused))
    }

    fn unreachable() -> Checked {
        Err(Outcome::Unreachable("HTTP 503".to_string()))
    }

    /// A check that finds the BMC unreachable runs again until it answers or
    /// the wait runs out, unless repeating it could redo a change, which
    /// leaves it unconfirmed; any other ending is final.
    #[tokio::test(start_paused = true)]
    async fn unreachable_checks_run_again_until_answered() {
        let cases: [(&str, Check, Vec<Checked>, &str, usize); 4] = [
            (
                "answers on the third attempt",
                check(),
                vec![unreachable(), unreachable(), Ok("up".to_string().into())],
                "Pass",
                3,
            ),
            (
                "a check that is not safe to repeat",
                check().once(),
                vec![unreachable()],
                "Unconfirmed(\"HTTP 503; not run again",
                1,
            ),
            (
                "unreachable for the whole wait",
                check(),
                Vec::new(),
                "still unavailable after 900s",
                31,
            ),
            (
                "a failure ends it at once",
                check(),
                vec![Err(Outcome::Fail("HTTP 400".to_string()))],
                "HTTP 400",
                1,
            ),
        ];
        for (scenario, check, script, expected, attempts) in cases {
            let script = RefCell::new(VecDeque::from(script));
            let made = Cell::new(0);
            let outcome = run_until_answered(
                &check,
                async || {
                    made.set(made.get() + 1);
                    script.borrow_mut().pop_front().unwrap_or_else(unreachable)
                },
                drop,
            )
            .await
            .expect("no attempt outlasts the timeout");
            assert!(
                format!("{outcome:?}").contains(expected),
                "{scenario}: {outcome:?}"
            );
            assert_eq!(made.get(), attempts, "{scenario}");
        }
    }

    /// An attempt that outlasts the check's timeout is cut off, so the
    /// runner can report that whatever it changed may not be restored.
    #[tokio::test(start_paused = true)]
    async fn an_attempt_past_the_timeout_is_cut_off() {
        let check = check().timeout(Duration::from_secs(60));
        let result = run_until_answered(
            &check,
            async || {
                tokio::time::sleep(Duration::from_secs(61)).await;
                Ok("late".to_string().into())
            },
            drop,
        )
        .await;
        assert!(result.is_err(), "{result:?}");
    }

    /// A check runs unless one it follows failed, was left unconfirmed, or
    /// was blocked; a skip decided at planning stands.
    #[test]
    fn checks_after_a_failure_or_block_are_blocked() {
        let follower = || Planned {
            check: check(),
            after: vec![FIRST],
            skip: None,
        };
        let cases = [
            (
                "follows a failure",
                Outcome::Fail(String::new()),
                follower(),
                "blocked",
            ),
            (
                "follows an unconfirmed change",
                Outcome::Unconfirmed(String::new()),
                follower(),
                "blocked",
            ),
            (
                "follows a block",
                Outcome::Blocked(String::new()),
                follower(),
                "blocked",
            ),
            (
                "follows a pass",
                Outcome::Pass(Default::default()),
                follower(),
                "runs",
            ),
            (
                "follows a skip",
                Outcome::Skipped(String::new()),
                follower(),
                "runs",
            ),
            (
                "already skipped",
                Outcome::Fail(String::new()),
                Planned {
                    skip: Some(Outcome::Skipped("needs --boot-mac".to_string())),
                    ..follower()
                },
                "skipped",
            ),
        ];
        for (scenario, earlier, planned, expected) in cases {
            let results = [CheckResult {
                id: FIRST,
                driver: String::new(),
                outcome: earlier,
                elapsed: Duration::ZERO,
            }];
            let ended = match gated(planned, &results).skip {
                None => "runs",
                Some(Outcome::Blocked(_)) => "blocked",
                Some(Outcome::Skipped(_)) => "skipped",
                Some(other) => panic!("{scenario}: {other:?}"),
            };
            assert_eq!(ended, expected, "{scenario}");
        }
    }
}
