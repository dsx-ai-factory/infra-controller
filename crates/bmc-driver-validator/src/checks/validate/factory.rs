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

use super::host::lift_lockdown;
use crate::checks::context::CheckCx;
use crate::checks::errors::describe;
use crate::checks::model::{Check, CheckId, Needs, Outcome, Step};
use crate::checks::ops::{self, Attempt, Effect, SETTLE_POLL_INTERVAL, how, poll, requested, took};
use crate::checks::text::counted;

/// How long a BMC may keep answering after accepting a factory reset, and
/// then take before the factory password works.
const FACTORY_DOWN_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const FACTORY_RESET_TIMEOUT: Duration = Duration::from_secs(20 * 60);

const FACTORY_LOCKDOWN: CheckId = CheckId::new("factory.lockdown");
const FACTORY_UEFI_PASSWORD: CheckId = CheckId::new("factory.uefi_password");
const FACTORY_BIOS: CheckId = CheckId::new("factory.bios");
const FACTORY_BMC: CheckId = CheckId::new("factory.bmc");
const FACTORY_RESET_LOCKDOWN: CheckId = CheckId::new("factory_reset.lockdown");
const FACTORY_RESET_UEFI_PASSWORD: CheckId = CheckId::new("factory_reset.uefi_password");
const FACTORY_RESET_BIOS: CheckId = CheckId::new("factory_reset.bios");
const FACTORY_RESET_BMC: CheckId = CheckId::new("factory_reset.bmc");

/// Back to factory state; each step runs whatever came before.
pub(super) fn reset() -> Vec<Check> {
    vec![
        Check::apply(FACTORY_LOCKDOWN, Capability::Lockdown, run!(lift_lockdown)),
        Check::apply(
            FACTORY_UEFI_PASSWORD,
            Capability::Bios,
            run!(clear_uefi_password),
        )
        .once()
        .needs(&[Needs::UefiPassword]),
        Check::apply(FACTORY_BIOS, Capability::Bios, run!(reset_bios)),
        Check::apply(FACTORY_BMC, Capability::BmcControl, run!(reset_to_factory)).once(),
    ]
}

/// Back to factory state before validating, for a BMC an earlier run left
/// configured: lockdown off so the BIOS takes the reset, the UEFI password
/// an earlier run set cleared before it, since a BIOS reset keeps it and the
/// host then stops at its prompt, and the BMC last since it removes the
/// account the run logged in with.
pub(in crate::checks) fn factory_reset_first() -> Vec<Check> {
    vec![
        Check::apply(
            FACTORY_RESET_LOCKDOWN,
            Capability::Lockdown,
            run!(lift_lockdown),
        ),
        Check::apply(
            FACTORY_RESET_UEFI_PASSWORD,
            Capability::Bios,
            run!(clear_uefi_password),
        )
        .once()
        .needs(&[Needs::UefiPassword])
        .after(&[FACTORY_RESET_LOCKDOWN]),
        Check::apply(FACTORY_RESET_BIOS, Capability::Bios, run!(reset_bios))
            .after(&[FACTORY_RESET_LOCKDOWN, FACTORY_RESET_UEFI_PASSWORD]),
        Check::apply(
            FACTORY_RESET_BMC,
            Capability::BmcControl,
            run!(reset_to_factory),
        )
        .once()
        .after(&[FACTORY_RESET_BIOS]),
    ]
}

async fn clear_uefi_password(cx: &CheckCx<'_>) -> Step {
    let password = cx.inputs().required_uefi_password()?;
    let outcome = requested(
        cx,
        "clear_uefi_password",
        cx.bmc().bios()?.clear_uefi_password(&password.new),
    )
    .await?;
    ops::take_effect(cx, &outcome).await?;
    Ok(format!("cleared ({}) and restarted", how(&outcome)))
}

/// Restores the BIOS defaults and restarts the host; the reset shows as the
/// settings the validator applies no longer being in effect.
async fn reset_bios(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let bios = bmc.bios()?;
    let outcome = requested(cx, "reset", bios.reset()).await?;
    let inputs = cx.inputs();
    let boot_interface = inputs.boot_interface();
    let differing = ops::read_back(cx, &outcome, Effect::OnRestart, async || {
        match bios
            .status(&inputs.bios_profile, boot_interface.as_ref())
            .await
        {
            Ok(status) if ops::only_unreported_differ(&status) => {
                Attempt::NotYet("the BIOS reports none of the attributes that differ".to_string())
            }
            Ok(status) if !status.is_applied => Attempt::Done(status.differences.len()),
            Ok(_) => {
                Attempt::NotYet("the validator's BIOS settings are still in effect".to_string())
            }
            Err(error) => Attempt::NotYet(describe("status", &error)),
        }
    })
    .await?;
    Ok(format!(
        "reset to defaults ({}) and restarted; {} now differ from what the validator applies",
        how(&outcome),
        counted(differing, "setting", "settings")
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
