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

//! The steps that return a BMC to factory state.

use std::time::{Duration, Instant};

use bmc_platform::{AuthError, Capability, PlatformError};

use super::UEFI_PASSWORD;
use crate::checks::apply::lift_lockdown;
use crate::checks::context::CheckCx;
use crate::checks::errors::describe;
use crate::checks::model::{Check, CheckFn, CheckId, Needs, Outcome, Sequenced, Step};
use crate::checks::ops::{self, Attempt, Effect, SETTLE_POLL_INTERVAL, how, poll, requested, took};
use crate::checks::text::counted;

/// How long a BMC may keep answering after accepting a factory reset, and
/// then take before the factory password works.
const FACTORY_DOWN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const FACTORY_RESET_TIMEOUT: Duration = Duration::from_secs(20 * 60);

/// The steps back to factory state, as one pass of them reports them.
struct FactorySteps {
    lockdown: CheckId,
    uefi_password: CheckId,
    bios: CheckId,
    bmc: CheckId,
}

/// The pass after validating, with `--validate factory`.
const AFTER: FactorySteps = FactorySteps {
    lockdown: CheckId::new(Capability::Lockdown, "factory"),
    uefi_password: CheckId::new(Capability::Bios, "factory_uefi_password"),
    bios: CheckId::new(Capability::Bios, "factory"),
    bmc: CheckId::new(Capability::BmcControl, "factory"),
};

/// The pass before validating, with `--factory-reset-first`.
const FIRST: FactorySteps = FactorySteps {
    lockdown: CheckId::new(Capability::Lockdown, "factory_first"),
    uefi_password: CheckId::new(Capability::Bios, "factory_first_uefi_password"),
    bios: CheckId::new(Capability::Bios, "factory_first"),
    bmc: CheckId::new(Capability::BmcControl, "factory_first"),
};

impl FactorySteps {
    /// Lockdown off so the BIOS takes the reset, the UEFI password cleared
    /// by `clear_uefi_password`, the BIOS reset, then the BMC.
    fn checks(&self, clear_uefi_password: CheckFn) -> [Check; 4] {
        [
            Check::apply(self.lockdown, run!(lift_lockdown)),
            Check::apply(self.uefi_password, clear_uefi_password)
                .once()
                .needs(&[Needs::UefiPassword]),
            Check::apply(self.bios, run!(reset_bios)),
            Check::apply(self.bmc, run!(reset_to_factory)).once(),
        ]
    }
}

/// Both passes' checks: after validating, the UEFI password is cleared only
/// when this run set it; before, the one an earlier run left is.
pub(super) fn checks() -> Vec<Check> {
    AFTER
        .checks(run!(clear_uefi_password_this_run_set))
        .into_iter()
        .chain(FIRST.checks(run!(clear_uefi_password)))
        .collect()
}

/// Back to factory state; each step runs whatever came before.
pub(super) fn reset() -> Vec<Sequenced> {
    vec![
        Sequenced::new(AFTER.lockdown),
        Sequenced::new(AFTER.uefi_password),
        Sequenced::new(AFTER.bios).after(&[AFTER.uefi_password]),
        Sequenced::new(AFTER.bmc),
    ]
}

/// Back to factory state before validating, for a BMC an earlier run left
/// configured: lockdown off so the BIOS takes the reset, the UEFI password
/// an earlier run set cleared before it, since a BIOS reset keeps it and the
/// host then stops at its prompt, and the BMC last since it removes the
/// account the run logged in with.
pub(in crate::checks) fn factory_reset_first() -> Vec<Sequenced> {
    vec![
        Sequenced::new(FIRST.lockdown),
        Sequenced::new(FIRST.uefi_password).after(&[FIRST.lockdown]),
        Sequenced::new(FIRST.bios).after(&[FIRST.lockdown, FIRST.uefi_password]),
        Sequenced::new(FIRST.bmc).after(&[FIRST.bios]),
    ]
}

/// Clears the UEFI password when this run's change of it may have taken:
/// it passed, or the firmware's answer left it unconfirmed. A clear that
/// finds the new password unset fails, which blocks the BIOS reset that
/// would otherwise leave the host at the firmware's password prompt.
async fn clear_uefi_password_this_run_set(cx: &CheckCx<'_>) -> Step {
    match cx.result_of(UEFI_PASSWORD) {
        Some(Outcome::Pass(_) | Outcome::Unconfirmed(_)) => clear_uefi_password(cx).await,
        _ => Err(Outcome::Skipped(
            "this run left the UEFI password unchanged".to_string(),
        )),
    }
}

/// Clears the UEFI password the new one set: before the validation, the one
/// an earlier run left, and after it, the one this run set.
async fn clear_uefi_password(cx: &CheckCx<'_>) -> Step {
    let password = cx.inputs().required_uefi_password()?;
    let outcome = requested(
        cx,
        "clear_uefi_password",
        cx.bmc().bios()?.clear_uefi_password(&password.new),
    )
    .await?;
    cx.take_effect(&outcome).await?;
    Ok(format!("cleared ({}) and restarted", how(&outcome)))
}

/// Restores the BIOS defaults and restarts the host, done once the driver's
/// work completes and the BIOS reads back. The defaults can match what the
/// validator applies, so settings still in effect are a warning, not a
/// failure.
async fn reset_bios(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let bios = bmc.bios()?;
    let outcome = requested(cx, "reset", bios.reset()).await?;
    let inputs = cx.inputs();
    let boot_interface = inputs.boot_interface();
    let status = cx
        .read_back(&outcome, Effect::OnRestart, async || {
            match bios
                .status(&inputs.bios_profile, boot_interface.as_ref())
                .await
            {
                Ok(status) => Attempt::Done(status),
                Err(error) => Attempt::NotYet(describe("status", &error)),
            }
        })
        .await?;
    let after = if status.is_applied {
        "warning: the validator's BIOS settings are still in effect, which the defaults may match"
            .to_string()
    } else {
        format!(
            "{} now differ from what the validator applies",
            counted(status.differences.len(), "setting", "settings")
        )
    };
    Ok(format!(
        "reset to defaults ({}) and restarted; {after}",
        how(&outcome)
    ))
}

/// Resets the BMC to factory defaults, then waits for it to go down and
/// come back answering to the factory password.
async fn reset_to_factory(cx: &CheckCx<'_>) -> Step {
    let factory = cx.inputs().factory.as_ref().ok_or_else(|| {
        Outcome::Skipped("needs --new-password, so --password is the factory one".to_string())
    })?;
    let started = Instant::now();
    let outcome = ops::sent_or_dropped(
        cx,
        "reset_to_factory_defaults",
        cx.bmc().bmc_control()?.reset_to_factory_defaults(),
    )
    .await?;
    ops::wait_for_bmc_down(cx, FACTORY_DOWN_TIMEOUT).await?;
    poll(
        cx,
        FACTORY_RESET_TIMEOUT,
        SETTLE_POLL_INTERVAL,
        async || match factory.log_in().await {
            Ok(()) | Err(PlatformError::Auth(AuthError::PasswordChangeRequired { .. })) => {
                Attempt::Done(())
            }
            Err(error) => Attempt::NotYet(format!("the factory password fails: {error}")),
        },
    )
    .await?;
    Ok(format!(
        "the BMC went down and answered to the factory password {} after the request ({})",
        took(started),
        how(&outcome)
    ))
}
