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

//! Checks that change the BMC or host and verify the effect.
//!
//! BIOS-backed settings take effect on a host reset, so each settings check
//! applies, restarts the host once, waits for the BMC's job, and then polls
//! the driver's status until the settings are in effect. A BMC can drop
//! queued BIOS work on each BIOS write, so settings checks do not share a
//! restart.
//! Power, lockdown, and account checks restore what they change.

use std::collections::BTreeSet;
use std::fmt::Debug;
use std::time::{Duration, Instant};

use bmc_platform::{
    Capability, ConsoleState, DriverOutcome, LockdownDesiredState, LockdownState, OpCx,
    PlatformError,
};
use carbide_secrets::credentials::Credentials;
use nv_redfish::account::ManagerAccountCreate;
use nv_redfish::resource::{PowerState, ResetType};

use super::context::CheckCx;
use super::errors::{describe, failed};
use super::model::{Check, CheckId, Needs, Outcome, Step};
use super::ops::{
    self, Attempt, Effect, IMMEDIATE_TIMEOUT, QUICK_POLL_INTERVAL, SETTLE_POLL_INTERVAL,
    SETTLE_TIMEOUT, how, poll, took,
};
use super::runner::{RETRY_INTERVAL, UNREACHABLE_TIMEOUT};
use super::text::{boot_order_summary, differences};
use crate::connect::Target;

pub(super) const BIOS: CheckId = CheckId::new("bios.apply");
pub(super) const BOOT_ORDER: CheckId = CheckId::new("boot_order.configure");
pub(super) const CONSOLE: CheckId = CheckId::new("console.setup");
pub(super) const POWER_CYCLE: CheckId = CheckId::new("power.cycle");
pub(super) const STORAGE_ERASE: CheckId = CheckId::new("storage.decommission_controller");
const STORAGE_VOLUME: CheckId = CheckId::new("storage.create_volume");
const ACCOUNTS_LIFECYCLE: CheckId = CheckId::new("accounts.lifecycle");
const LOCKDOWN_TOGGLE: CheckId = CheckId::new("lockdown.toggle");

/// The volume NICo's cleanup creates on the boot controller.
const BOOT_VOLUME: &str = "VD_0";

/// How long the BMC may take to list the boot interface's boot option after
/// the BIOS settings that create it read back.
const BOOT_OPTION_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// How long settings may take to take effect after the first apply before
/// they are applied again.
const APPLY_AGAIN_AFTER: Duration = Duration::from_secs(10 * 60);

/// How long the settings still differing after a partial apply must read
/// the same before they are applied again.
const STALLED_FOR: Duration = Duration::from_secs(2 * 60);

pub(super) fn checks() -> Vec<Check> {
    vec![
        bios(),
        boot_order(),
        console(),
        power_cycle(),
        decommission_controller(),
        create_volume(),
        accounts_lifecycle(),
        lockdown_toggle(),
    ]
}

/// Erases the boot controller's drives, as NICo's cleanup does first.
pub(super) fn decommission_controller() -> Check {
    Check::apply(
        STORAGE_ERASE,
        Capability::Storage,
        run!(erase_boot_controller),
    )
    .once()
    .needs(&[Needs::StorageErase])
}

/// Creates the boot volume NICo's cleanup creates after the erase.
pub(super) fn create_volume() -> Check {
    Check::apply(
        STORAGE_VOLUME,
        Capability::Storage,
        run!(create_boot_volume),
    )
    .once()
    .needs(&[Needs::StorageErase])
}

pub(super) fn bios() -> Check {
    Check::apply(BIOS, Capability::Bios, run!(bios_apply))
}

pub(super) fn boot_order() -> Check {
    Check::apply(
        BOOT_ORDER,
        Capability::BootOrder,
        run!(boot_order_configure),
    )
    .needs(&[Needs::BootInterface])
}

pub(super) fn console() -> Check {
    Check::apply(CONSOLE, Capability::Console, run!(console_setup))
}

pub(super) fn power_cycle() -> Check {
    Check::apply(POWER_CYCLE, Capability::Power, run!(cycle_power)).once()
}

pub(super) fn accounts_lifecycle() -> Check {
    Check::apply(
        ACCOUNTS_LIFECYCLE,
        Capability::Accounts,
        run!(account_lifecycle),
    )
    .once()
}

fn lockdown_toggle() -> Check {
    Check::apply(LOCKDOWN_TOGGLE, Capability::Lockdown, run!(toggle_lockdown)).once()
}

async fn bios_apply(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let bios = bmc.bios()?;
    let profile = &cx.inputs().bios_profile;
    let boot_interface = cx.inputs().boot_interface();
    let boot_interface = boot_interface.as_ref();
    let status = bios
        .status(profile, boot_interface)
        .await
        .and_then(|status| {
            if ops::only_unreported_differ(&status) {
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
        async || ops::restart_host(cx).await,
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

async fn boot_order_configure(cx: &CheckCx<'_>) -> Step {
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

async fn console_setup(cx: &CheckCx<'_>) -> Step {
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
/// which must start no work.
async fn apply_until_in_effect(
    cx: &CheckCx<'_>,
    pending: impl AsyncFn() -> Result<Option<NotInEffect>, Outcome>,
    apply: impl AsyncFn() -> Result<DriverOutcome, Outcome>,
) -> Step {
    let trace = |step: String| cx.trace(step);
    let Some(before) = when_answered(&trace, &pending).await? else {
        cx.trace("already in effect; applying again, which must start no work");
        return reapply_changes_nothing(cx, &apply, "already in effect").await;
    };
    cx.trace(format!("not in effect: {}", before.summary));
    let started = Instant::now();
    let outcome = when_answered(&trace, &apply).await?;
    cx.trace(format!("apply: {}", how(&outcome)));
    ops::take_effect(cx, &outcome).await?;
    let mut applied = format!("applying ({}) and a host restart", how(&outcome));
    if let Some(rest) = wait_in_effect(&trace, &pending, Some(&before.settings)).await? {
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
        let again = when_answered(&trace, &apply).await?;
        cx.trace(format!("apply again: {}", how(&again)));
        ops::take_effect(cx, &again).await?;
        wait_in_effect(&trace, &pending, None).await?;
        applied.push_str(&format!(
            ", then applying again ({}) and a second restart",
            how(&again)
        ));
    }
    cx.trace("in effect; applying again, which must start no work");
    let state = format!("in effect {} after {applied}", took(started));
    reapply_changes_nothing(cx, &apply, &state).await
}

/// Runs `request` until the BMC answers it, waiting as the runner does for
/// an unreachable BMC while it cannot: it lists boot options or BIOS
/// attributes again after a restart, or fails with a server error. Any other
/// failure ends it at once. Applies stage the same settings each time, so
/// sending one again is safe.
async fn when_answered<T>(
    trace: impl Fn(String),
    request: &impl AsyncFn() -> Result<T, Outcome>,
) -> Result<T, Outcome> {
    ops::poll_traced(
        trace,
        UNREACHABLE_TIMEOUT,
        RETRY_INTERVAL,
        async || match request().await {
            Ok(answer) => Attempt::Done(answer),
            Err(Outcome::Unreachable(reason)) => Attempt::NotYet(reason),
            Err(other) => Attempt::Failed(other),
        },
    )
    .await
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
    ops::poll_traced(
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

/// Erases the boot controller and waits until it lists no volume.
async fn erase_boot_controller(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let storage = bmc.storage()?;
    let controller = boot_controller(cx).await?;
    let outcome = when_answered(|step| cx.trace(step), &async || {
        storage
            .decommission_controller(&controller)
            .await
            .map_err(failed("decommission_controller"))
    })
    .await?;
    cx.trace(format!("decommission_controller: {}", how(&outcome)));
    ops::read_back(
        cx,
        &outcome,
        Effect::Immediate,
        async || match volume_names(cx, &controller).await {
            Ok(names) if names.is_empty() => Attempt::Done(()),
            Ok(names) => Attempt::NotYet(format!("{controller} lists {}", names.join(", "))),
            Err(error) => Attempt::NotYet(describe("reading volumes", &error)),
        },
    )
    .await?;
    Ok(format!(
        "{controller} erased ({}); it lists no volume",
        how(&outcome)
    ))
}

/// Creates [`BOOT_VOLUME`] on the boot controller, restarts the host so the
/// job runs, and waits until the controller lists it.
async fn create_boot_volume(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let storage = bmc.storage()?;
    let controller = boot_controller(cx).await?;
    let outcome = when_answered(|step| cx.trace(step), &async || {
        storage
            .create_volume(&controller, BOOT_VOLUME)
            .await
            .map_err(failed("create_volume"))
    })
    .await?;
    cx.trace(format!("create_volume: {}", how(&outcome)));
    ops::read_back(
        cx,
        &outcome,
        Effect::OnRestart,
        async || match volume_names(cx, &controller).await {
            Ok(names) if names.iter().any(|name| name == BOOT_VOLUME) => Attempt::Done(()),
            Ok(names) if names.is_empty() => {
                Attempt::NotYet(format!("{controller} lists no volume"))
            }
            Ok(names) => Attempt::NotYet(format!("{controller} lists {}", names.join(", "))),
            Err(error) => Attempt::NotYet(describe("reading volumes", &error)),
        },
    )
    .await?;
    Ok(format!(
        "{BOOT_VOLUME} created on {controller} ({}) and listed after a host restart",
        how(&outcome)
    ))
}

/// The boot controller the driver names; a check is skipped without one.
async fn boot_controller(cx: &CheckCx<'_>) -> Result<String, Outcome> {
    cx.bmc()
        .storage()?
        .boot_controller()
        .await
        .map_err(failed("boot_controller"))?
        .ok_or_else(|| Outcome::Skipped("the driver names no boot controller".to_string()))
}

/// The names of the volumes on storage controller `controller`, read over a
/// fresh connection rather than through the driver under test.
async fn volume_names(cx: &CheckCx<'_>, controller: &str) -> Result<Vec<String>, PlatformError> {
    let connection = cx.target().fresh_connection().await?;
    let bmc = cx.bmc();
    let op = OpCx::new(
        connection.bmc.as_ref(),
        connection.service_root.as_ref(),
        bmc.endpoint().identity(),
    );
    let storage = op
        .system()
        .await?
        .storage_controllers()
        .await
        .map_err(PlatformError::from_redfish)?
        .unwrap_or_default()
        .into_iter()
        .find(|storage| storage.raw().odata_id.last_segment() == Some(controller))
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: format!("the system lists no storage controller {controller}"),
        })?;
    let Some(volumes) = storage.volumes() else {
        return Ok(Vec::new());
    };
    Ok(volumes
        .members()
        .await
        .map_err(PlatformError::from_redfish)?
        .iter()
        .map(|volume| volume.raw().name.clone())
        .collect())
}

/// Cycles host power away from its current state and back.
async fn cycle_power(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let Some(original) = ops::power_state(&bmc).await.map_err(failed("state"))? else {
        return Err(Outcome::Skipped(
            "the platform reports no single power state".to_string(),
        ));
    };
    if matches!(original, PowerState::PoweringOn | PowerState::PoweringOff) {
        return Err(Outcome::Skipped(format!(
            "the host is {original:?}, so there is no settled state to cycle back to"
        )));
    }
    let (away, back) = match original {
        PowerState::Off => (ResetType::On, ResetType::ForceOff),
        _ => (ResetType::ForceOff, ResetType::On),
    };
    there_and_back(
        cx,
        "host power",
        Some(original),
        ops::set_power(cx, &bmc, away),
        ops::set_power(cx, &bmc, back),
        ops::power_state(&bmc),
    )
    .await
}

/// Changes a setting away from `original` and back, then records the change
/// as not restored unless `read` finds `original` again. Ends with both
/// changes' reports, or the first failure.
async fn there_and_back<S: PartialEq + Debug>(
    cx: &CheckCx<'_>,
    setting: &str,
    original: S,
    away: impl Future<Output = Step>,
    back: impl Future<Output = Step>,
    read: impl Future<Output = Result<S, PlatformError>>,
) -> Step {
    let away = away.await;
    let back = back.await;
    match read.await {
        Ok(current) if current == original => {}
        current => cx.not_restored(format!(
            "{setting} was {original:?}; it now reads {current:?}"
        )),
    }
    Ok(format!("{}; {}", away?, back?))
}

/// Creates a temporary account, logs in with it, deletes it, and confirms
/// the login stops working; the run's own account is never changed.
async fn account_lifecycle(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let accounts = bmc.accounts()?;
    let username = format!("nicoval{:04}", rand::random::<u16>() % 10_000);
    let password = Credentials::generate_password();
    cx.secret(&password);
    let temporary = cx.target().as_account(&username, &password);

    if listed(cx, &username).await? {
        return Err(Outcome::Skipped(format!(
            "an account named {username} already exists"
        )));
    }
    let created = accounts
        .create(ManagerAccountCreate::builder(
            password.clone(),
            username.clone(),
            "Administrator".to_string(),
        ))
        .await;
    match &created {
        Ok(outcome) => cx.trace(format!("create {username}: {}", how(outcome))),
        Err(error) => cx.trace(format!("create {username}: {error}")),
    }
    let verified = match created {
        Ok(outcome) => verify_new_account(cx, &outcome, &username, &temporary).await,
        Err(error) => Err(failed("create")(error)),
    };
    if !listed(cx, &username).await.unwrap_or(true) {
        return verified;
    }

    let deleted = accounts.delete(&username).await.map_err(failed("delete"));
    if let Ok(outcome) = &deleted {
        cx.trace(format!("delete {username}: {}", how(outcome)));
    }
    let gone = match deleted {
        Ok(outcome) => verify_account_gone(cx, &outcome, &username, &temporary).await,
        Err(outcome) => Err(outcome),
    };
    if gone.is_err() {
        cx.not_restored(format!("temporary account {username} may still exist"));
    }
    Ok(format!("{}; {}", verified?, gone?))
}

async fn verify_new_account(
    cx: &CheckCx<'_>,
    outcome: &DriverOutcome,
    username: &str,
    temporary: &Target,
) -> Step {
    ops::wait_for_operations(cx, outcome).await?;
    if !listed(cx, username).await? {
        return Err(Outcome::Fail(format!(
            "created {username}, but it is not listed"
        )));
    }
    poll(
        cx,
        IMMEDIATE_TIMEOUT,
        QUICK_POLL_INTERVAL,
        async || match temporary.log_in().await {
            Ok(()) => Attempt::Done(()),
            Err(error) => Attempt::NotYet(format!("logging in as {username} fails: {error}")),
        },
    )
    .await?;
    cx.trace(format!("logged in as {username}"));
    Ok(format!("created {username} and logged in with it"))
}

async fn verify_account_gone(
    cx: &CheckCx<'_>,
    outcome: &DriverOutcome,
    username: &str,
    temporary: &Target,
) -> Step {
    ops::wait_for_operations(cx, outcome).await?;
    if listed(cx, username).await? {
        return Err(Outcome::Fail(format!(
            "deleted {username}, but it is still listed"
        )));
    }
    poll(
        cx,
        IMMEDIATE_TIMEOUT,
        QUICK_POLL_INTERVAL,
        async || match temporary.log_in().await {
            Err(PlatformError::Auth(_)) => Attempt::Done(()),
            Err(error) => Attempt::NotYet(format!(
                "logging in as {username} fails with {error}, not an auth error"
            )),
            Ok(()) => Attempt::NotYet(format!("{username} can still log in")),
        },
    )
    .await?;
    Ok("deleted it, and its login is refused".to_string())
}

async fn listed(cx: &CheckCx<'_>, username: &str) -> Result<bool, Outcome> {
    let accounts = cx.bmc().accounts()?.list().await.map_err(failed("list"))?;
    Ok(accounts
        .iter()
        .any(|account| account.user_name.as_deref() == Some(username)))
}

/// Locks down a host that is not locked down, or unlocks one that is, then
/// restores it.
async fn toggle_lockdown(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let lockdown = bmc.lockdown()?;
    let original = lockdown.status().await.map_err(failed("status"))?.aggregate;
    let (away, back) = match original {
        LockdownState::Enabled => (
            LockdownDesiredState::Disabled,
            LockdownDesiredState::Enabled,
        ),
        _ => (
            LockdownDesiredState::Enabled,
            LockdownDesiredState::Disabled,
        ),
    };
    there_and_back(
        cx,
        "lockdown",
        original,
        ops::set_lockdown(cx, away),
        ops::set_lockdown(cx, back),
        async { lockdown.status().await.map(|status| status.aggregate) },
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::collections::VecDeque;

    use super::*;

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

    /// A request waits while the BMC cannot answer yet, and ends at once on
    /// any other failure.
    #[tokio::test(start_paused = true)]
    async fn a_request_waits_until_the_bmc_answers() {
        let not_ready = || Err(Outcome::Unreachable("BootOrder lists Boot0008".to_string()));
        let answered = || Ok(());
        let cases = [
            (
                "ready after two reads",
                vec![not_ready(), not_ready(), answered()],
                true,
                3,
            ),
            (
                "a failure ends it",
                vec![Err(Outcome::Fail("refused".to_string()))],
                false,
                1,
            ),
            ("never ready", vec![], false, 31),
        ];
        for (scenario, reads, passes, attempts) in cases {
            let reads = RefCell::new(VecDeque::from(reads));
            let made = Cell::new(0);
            let pending = async || {
                made.set(made.get() + 1);
                reads.borrow_mut().pop_front().unwrap_or_else(not_ready)
            };
            let result = when_answered(|_| {}, &pending).await;
            assert_eq!(result.is_ok(), passes, "{scenario}: {:?}", result.err());
            assert_eq!(made.get(), attempts, "{scenario}: reads");
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
