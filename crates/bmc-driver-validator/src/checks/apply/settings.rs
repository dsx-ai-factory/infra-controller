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

//! Settings that take effect on a host reset: BIOS attributes, the boot
//! order, and the serial console. Each applies, restarts the host once,
//! waits for the BMC's job, and then polls the driver's status until the
//! settings are in effect. A BMC can drop queued BIOS work on each BIOS
//! write, so settings checks do not share a restart.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bmc_platform::{BiosStatus, ConsoleState, DriverOutcome, PlatformError};

use crate::checks::context::CheckCx;
use crate::checks::errors::failed;
use crate::checks::model::{Outcome, Step};
use crate::checks::ops::{
    self, Attempt, SETTLE_POLL_INTERVAL, SETTLE_TIMEOUT, how, poll, poll_traced, took,
    until_answered,
};
use crate::checks::text::{boot_order_summary, differences};

/// How long the BMC may take to list the boot interface's boot option after
/// the BIOS settings that create it read back.
const BOOT_OPTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long settings may take to take effect after the first apply before
/// they are applied again.
const APPLY_AGAIN_AFTER: Duration = Duration::from_secs(10 * 60);

/// How long the settings still differing after a partial apply must read
/// the same before they are applied again.
const STALLED_FOR: Duration = Duration::from_secs(2 * 60);

/// Whether every attribute `status` finds out of place is one the BIOS does
/// not report, as when it has reported only part of its attributes since the
/// BMC restarted; the rest return once the host starts again.
fn only_unreported_differ(status: &BiosStatus) -> bool {
    !status.differences.is_empty() && status.differences.iter().all(|diff| diff.actual.is_none())
}

pub(super) async fn bios_apply(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let bios = bmc.bios()?;
    let profile = &cx.inputs().bios_profile;
    let boot_interface = cx.inputs().boot_interface();
    let boot_interface = boot_interface.as_ref();
    let status = bios
        .status(profile, boot_interface)
        .await
        .and_then(|status| {
            if only_unreported_differ(&status) {
                Err(PlatformError::NotReady {
                    message: "the BIOS reports none of the attributes that differ".to_string(),
                })
            } else {
                Ok(())
            }
        });
    restart_unless_bios_reported(
        |step| cx.trace(step),
        status,
        async || cx.restart_host().await,
    )
    .await?;
    apply_until_in_effect(
        cx,
        async || {
            let status = bios
                .status(profile, boot_interface)
                .await
                .map_err(failed("status"))?;
            Ok((!status.is_applied).then(|| NotInEffect {
                summary: differences(
                    status
                        .differences
                        .iter()
                        .map(|diff| (diff.key.as_str(), &diff.expected, diff.actual.as_ref())),
                ),
                settings: status
                    .differences
                    .iter()
                    .map(|diff| diff.key.clone())
                    .collect(),
            }))
        },
        async || {
            bios.apply(profile, boot_interface)
                .await
                .map_err(failed("apply"))
        },
    )
    .await
}

/// Restarts the host when `status` says the BIOS has not reported its
/// attributes: it reports them to the BMC only as the host starts, so a BMC
/// restarted since then has none until the host restarts. The first read of
/// the apply then waits for them.
async fn restart_unless_bios_reported(
    trace: impl Fn(String),
    status: Result<(), PlatformError>,
    restart: impl AsyncFnOnce() -> Result<(), Outcome>,
) -> Result<(), Outcome> {
    let Err(PlatformError::NotReady { message }) = status else {
        return Ok(());
    };
    trace(format!("{message}; restarting the host"));
    restart().await
}

pub(super) async fn boot_order_configure(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let boot_order = bmc.boot_order()?;
    let selector = cx.inputs().required_boot_interface()?;
    apply_until_in_effect(
        cx,
        async || {
            let status = boot_order
                .status(&selector)
                .await
                .map_err(failed("status"))?;
            Ok((!status.is_configured())
                .then(|| NotInEffect::described(boot_order_summary(status))))
        },
        // The BMC lists a boot option only once the host has booted with the
        // BIOS settings that create it, a while after they read back.
        async || {
            poll(
                cx,
                BOOT_OPTION_TIMEOUT,
                SETTLE_POLL_INTERVAL,
                async || match boot_order.configure(&selector).await {
                    Ok(outcome) => Attempt::Done(outcome),
                    Err(PlatformError::MissingBootOption { description }) => {
                        Attempt::NotYet(format!("the BMC does not list {description} yet"))
                    }
                    Err(error) => Attempt::Failed(failed("configure")(error)),
                },
            )
            .await
        },
    )
    .await
}

pub(super) async fn console_setup(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let console = bmc.console()?;
    apply_until_in_effect(
        cx,
        async || {
            let status = console.status().await.map_err(failed("status"))?;
            Ok((status.state != ConsoleState::Enabled).then(|| {
                NotInEffect::described(format!("{:?} ({})", status.state, status.message))
            }))
        },
        async || console.setup().await.map_err(failed("setup")),
    )
    .await
}

/// Why settings are not yet in effect: a summary, and the settings that
/// differ when the status names them.
struct NotInEffect {
    summary: String,
    settings: BTreeSet<String>,
}

impl NotInEffect {
    /// A reason that names no individual settings.
    fn described(summary: String) -> Self {
        Self {
            summary,
            settings: BTreeSet::new(),
        }
    }
}

/// Applies settings and verifies they take effect: `pending` reads why the
/// settings are not yet in effect, or `None` once they are. When the first
/// restart applies only some of the named settings, or they are still not
/// in effect after [`APPLY_AGAIN_AFTER`], they are applied and the host
/// restarted once more. Settings already in effect are applied once more,
/// which must start no work. Reads and applies are asked again while the
/// BMC cannot answer, as after a restart; applies stage the same settings
/// each time, so sending one again is safe.
async fn apply_until_in_effect(
    cx: &CheckCx<'_>,
    pending: impl AsyncFn() -> Result<Option<NotInEffect>, Outcome>,
    apply: impl AsyncFn() -> Result<DriverOutcome, Outcome>,
) -> Step {
    let trace = |step: String| cx.trace(step);
    let Some(before) = until_answered(trace, async || pending().await).await? else {
        cx.trace("already in effect; applying again, which must start no work");
        return reapply_changes_nothing(cx, &apply, "already in effect").await;
    };
    cx.trace(format!("not in effect: {}", before.summary));
    let started = Instant::now();
    let outcome = until_answered(trace, async || apply().await).await?;
    cx.trace(format!("apply: {}", how(&outcome)));
    cx.take_effect(&outcome).await?;
    let mut applied = format!("applying ({}) and a host restart", how(&outcome));
    if let Some(rest) = wait_in_effect(trace, &pending, Some(&before.settings)).await? {
        let took_effect = before
            .settings
            .iter()
            .filter(|setting| !rest.settings.contains(*setting))
            .count();
        cx.trace(format!(
            "{took_effect} of {} settings took effect, but {} still differ; applying again, \
             as a setting can depend on another applying first or need a second boot",
            before.settings.len(),
            rest.summary
        ));
        let again = until_answered(trace, async || apply().await).await?;
        cx.trace(format!("apply again: {}", how(&again)));
        cx.take_effect(&again).await?;
        wait_in_effect(trace, &pending, None).await?;
        applied.push_str(&format!(
            ", then applying again ({}) and a second restart",
            how(&again)
        ));
    }
    cx.trace("in effect; applying again, which must start no work");
    let state = format!("in effect {} after {applied}", took(started));
    reapply_changes_nothing(cx, &apply, &state).await
}

/// Whether some settings that differed `before` took effect while others
/// still differ, which can include settings the BIOS reports only now.
fn partly_in_effect(before: &BTreeSet<String>, differing: &BTreeSet<String>) -> bool {
    !differing.is_empty() && before.iter().any(|setting| !differing.contains(setting))
}

/// Polls `pending` until the settings are in effect, returning `None`.
/// Given the settings that differed `before` applying, it instead returns
/// what still differs once some of them took effect and what differs has
/// read the same for [`STALLED_FOR`], or once [`APPLY_AGAIN_AFTER`] passes:
/// a BIOS applies, or even reports, a setting that depends on another, such
/// as a TPM device behind TPM support, only on the boot after that one
/// applies, and some settings take a second boot of their own.
async fn wait_in_effect(
    trace: impl Fn(String),
    pending: &impl AsyncFn() -> Result<Option<NotInEffect>, Outcome>,
    before: Option<&BTreeSet<String>>,
) -> Result<Option<NotInEffect>, Outcome> {
    let started = tokio::time::Instant::now();
    let mut stalled: Option<(BTreeSet<String>, tokio::time::Instant)> = None;
    poll_traced(
        trace,
        SETTLE_TIMEOUT,
        SETTLE_POLL_INTERVAL,
        async || match pending().await {
            Ok(None) => Attempt::Done(None),
            Ok(Some(rest)) => {
                if before.is_some() && started.elapsed() >= APPLY_AGAIN_AFTER {
                    return Attempt::Done(Some(rest));
                }
                let partial = before.is_some_and(|before| partly_in_effect(before, &rest.settings));
                if !partial {
                    stalled = None;
                } else if let Some((settings, since)) = &stalled
                    && *settings == rest.settings
                {
                    if since.elapsed() >= STALLED_FOR {
                        return Attempt::Done(Some(rest));
                    }
                } else {
                    stalled = Some((rest.settings.clone(), tokio::time::Instant::now()));
                }
                Attempt::NotYet(format!("not in effect: {}", rest.summary))
            }
            // The BMC can refuse BIOS reads while the host POSTs.
            Err(Outcome::Fail(reason) | Outcome::Unreachable(reason)) => Attempt::NotYet(reason),
            Err(other) => Attempt::Failed(other),
        },
    )
    .await
}

async fn reapply_changes_nothing(
    cx: &CheckCx<'_>,
    apply: &impl AsyncFn() -> Result<DriverOutcome, Outcome>,
    state: &str,
) -> Step {
    match apply().await? {
        DriverOutcome::Complete => Ok(format!("{state}; applying again changed nothing")),
        accepted => {
            let uris = ops::uris(&accepted);
            cx.not_restored(format!(
                "work staged by a repeated apply is still pending: {uris}"
            ));
            Err(Outcome::Fail(format!(
                "{state}, yet applying again started {uris}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use bmc_platform::BiosDiff;

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

    /// The host restarts only when the BIOS has not reported its attributes;
    /// any other reading leaves the apply to report it.
    #[tokio::test]
    async fn the_host_restarts_only_when_the_bios_has_not_reported() {
        let cases = [
            ("already reported", Ok(()), 0),
            (
                "not reported",
                Err(PlatformError::NotReady {
                    message: "no attributes".to_string(),
                }),
                1,
            ),
            ("another error", Err(PlatformError::Unsupported), 0),
        ];
        for (scenario, status, restarts) in cases {
            let restarted = Cell::new(0);
            let result = restart_unless_bios_reported(
                |_| {},
                status,
                async || {
                    restarted.set(restarted.get() + 1);
                    Ok(())
                },
            )
            .await;
            assert_eq!(restarted.get(), restarts, "{scenario}: restarts");
            assert!(result.is_ok(), "{scenario}: {result:?}");
        }
    }

    /// A setting still not in effect `APPLY_AGAIN_AFTER` the first apply is
    /// applied again even though nothing took effect; after the second apply
    /// the wait runs to `SETTLE_TIMEOUT` and fails.
    #[tokio::test(start_paused = true)]
    async fn a_setting_still_not_in_effect_is_applied_again_once() {
        let pending = async || {
            Ok(Some(NotInEffect {
                summary: "TPM differs".to_string(),
                settings: BTreeSet::from(["TPM".to_string()]),
            }))
        };
        let before = BTreeSet::from(["TPM".to_string()]);
        for (scenario, before, apply_again, waited) in [
            (
                "after the first apply",
                Some(&before),
                true,
                APPLY_AGAIN_AFTER,
            ),
            ("after the second apply", None, false, SETTLE_TIMEOUT),
        ] {
            let started = tokio::time::Instant::now();
            let result = wait_in_effect(|_| {}, &pending, before).await;
            let elapsed = started.elapsed();
            assert_eq!(
                matches!(result, Ok(Some(_))),
                apply_again,
                "{scenario}: {:?}",
                result.err()
            );
            assert!(
                elapsed >= waited.saturating_sub(SETTLE_POLL_INTERVAL) && elapsed <= waited,
                "{scenario}: returned after {elapsed:?}"
            );
        }
    }

    /// A second apply is worth a restart only once the first applied some
    /// settings; settings the BIOS reports only after it count as differing.
    #[test]
    fn a_partial_apply_is_some_settings_taking_effect() {
        let set = |names: &[&str]| {
            names
                .iter()
                .map(ToString::to_string)
                .collect::<BTreeSet<_>>()
        };
        let before = set(&["QuietBoot", "SecurityDeviceSupport", "IPv4PXESupport"]);
        let cases = [
            (
                "nothing took effect yet",
                set(&["QuietBoot", "SecurityDeviceSupport", "IPv4PXESupport"]),
                false,
            ),
            ("some took effect", set(&["IPv4PXESupport"]), true),
            (
                "some took effect and a hidden setting appeared",
                set(&["DeviceSelect_0034", "DeviceSelect_003D"]),
                true,
            ),
            ("everything took effect", set(&[]), false),
        ];
        for (scenario, differing, partial) in cases {
            assert_eq!(partly_in_effect(&before, &differing), partial, "{scenario}");
        }
    }
}
