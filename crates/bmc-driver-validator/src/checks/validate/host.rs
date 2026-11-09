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

//! The machine controller's host initialization.

use std::time::Instant;

use bmc_platform::{Capability, ManagerSettings};
use nv_redfish::resource::ResetType;

use super::{BMC_RESET, UEFI_PASSWORD};
use crate::checks::apply::{
    BIOS, BOOT_ORDER, CONSOLE, POWER_CYCLE, STORAGE_ERASE, STORAGE_VOLUME, enable_lockdown,
    lift_lockdown,
};
use crate::checks::context::CheckCx;
use crate::checks::errors::{describe, failed};
use crate::checks::model::{Check, CheckId, Checked, Detail, Needs, Outcome, Sequenced, Step};
use crate::checks::ops::{
    self, Attempt, Effect, SETTLE_POLL_INTERVAL, SETTLE_TIMEOUT, how, poll, requested, took,
};
use crate::checks::text::{counted, difference_detail, reported};

const IPMI_OVER_LAN: CheckId = CheckId::new(Capability::BmcControl, "enable_ipmi_over_lan");
const LOCKDOWN_LIFT: CheckId = CheckId::new(Capability::Lockdown, "lift");
const MANAGER_SETTINGS: CheckId = CheckId::new(Capability::BmcControl, "settings");
const AC_CYCLE: CheckId = CheckId::new(Capability::Power, "ac_cycle");
const LOCKDOWN_ENABLE: CheckId = CheckId::new(Capability::Lockdown, "enable");
const SUPER_NIC_ENABLE: CheckId = CheckId::new(Capability::SuperNic, "enable_east_west");
const SUPER_NIC_IN_EFFECT: CheckId = CheckId::new(Capability::SuperNic, "east_west_in_effect");

/// The host setup checks only the host sequence runs.
pub(super) fn checks() -> Vec<Check> {
    vec![
        Check::apply(IPMI_OVER_LAN, run!(ipmi_over_lan)),
        Check::apply(LOCKDOWN_LIFT, run!(lift_lockdown)),
        Check::apply(MANAGER_SETTINGS, run!(manager_settings)),
        Check::apply(SUPER_NIC_ENABLE, run!(enable_east_west)).needs(&[Needs::SuperNics]),
        Check::apply(AC_CYCLE, run!(ac_cycle))
            .once()
            .needs(&[Needs::AcCycle]),
        Check::apply(SUPER_NIC_IN_EFFECT, run!(east_west_in_effect))
            .needs(&[Needs::SuperNics, Needs::AcCycle]),
        Check::apply(LOCKDOWN_ENABLE, run!(enable_lockdown)),
    ]
}

/// The host setup steps, then lockdown, which follows every one of them:
/// once locked down, the BMC refuses the changes they make.
pub(super) fn setup() -> Vec<Sequenced> {
    let mut steps = vec![
        Sequenced::new(IPMI_OVER_LAN).after(&[BMC_RESET]),
        Sequenced::new(LOCKDOWN_LIFT).after(&[IPMI_OVER_LAN]),
        Sequenced::new(MANAGER_SETTINGS).after(&[LOCKDOWN_LIFT]),
        Sequenced::new(STORAGE_ERASE).after(&[LOCKDOWN_LIFT]),
        Sequenced::new(STORAGE_VOLUME).after(&[STORAGE_ERASE]),
        Sequenced::new(BIOS).after(&[LOCKDOWN_LIFT]),
        Sequenced::new(BOOT_ORDER).after(&[BIOS]),
        Sequenced::new(CONSOLE).after(&[BIOS]),
        Sequenced::new(POWER_CYCLE).after(&[CONSOLE]),
        Sequenced::new(SUPER_NIC_ENABLE).after(&[BMC_RESET]),
        Sequenced::new(AC_CYCLE).after(&[POWER_CYCLE]),
        Sequenced::new(SUPER_NIC_IN_EFFECT).after(&[SUPER_NIC_ENABLE, AC_CYCLE]),
        Sequenced::new(UEFI_PASSWORD).after(&[BIOS]),
    ];
    let earlier: Vec<CheckId> = steps.iter().map(|step| step.id).collect();
    steps.push(Sequenced::new(LOCKDOWN_ENABLE).after(&earlier));
    steps
}

async fn ipmi_over_lan(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let control = bmc.bmc_control()?;
    if control
        .ipmi_over_lan_enabled()
        .await
        .map_err(failed("ipmi_over_lan_enabled"))?
    {
        return Ok("already enabled".to_string());
    }
    let outcome = requested(cx, "set_ipmi_over_lan", control.set_ipmi_over_lan(true)).await?;
    cx.driven()
        .read_back(&outcome, Effect::Immediate, async || {
            match control.ipmi_over_lan_enabled().await {
                Ok(true) => Attempt::Done(()),
                Ok(false) => Attempt::NotYet("IPMI over LAN reads disabled".to_string()),
                Err(error) => Attempt::NotYet(describe("ipmi_over_lan_enabled", &error)),
            }
        })
        .await?;
    Ok(format!(
        "enabled ({}); IPMI over LAN reads enabled",
        how(&outcome)
    ))
}

async fn manager_settings(cx: &CheckCx<'_>) -> Checked {
    let bmc = cx.bmc();
    let control = bmc.bmc_control()?;
    let outcome = requested(
        cx,
        "apply_settings",
        control.apply_settings(&ManagerSettings::default()),
    )
    .await?;
    let status = cx
        .driven()
        .read_back(&outcome, Effect::Immediate, async || {
            match control.settings_status().await {
                Ok(status) if status.is_applied => Attempt::Done(status),
                Ok(status) => {
                    Attempt::NotYet(format!("{} settings differ", status.differences.len()))
                }
                Err(error) => Attempt::NotYet(describe("settings_status", &error)),
            }
        })
        .await?;
    let detail = difference_detail(
        status
            .differences
            .iter()
            .map(|diff| (diff.key.as_str(), &diff.expected, diff.actual.as_ref())),
    );
    Ok(Detail {
        summary: format!("{} ({})", detail.summary, how(&outcome)),
        ..detail
    })
}

/// Requests an AC power cycle, repeating the request while a platform that
/// cycles only a powered-off host is still powering off, and waits for the
/// BMC to lose power with the host, then for it to answer again, for the work
/// the request started, and for the host to be on. A BMC accepts the request
/// before cutting power, so the cycle is only taken as done once the BMC
/// drops.
async fn ac_cycle(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let power = bmc.power()?;
    if !power
        .ac_power_cycle_supported()
        .await
        .map_err(failed("ac_power_cycle_supported"))?
    {
        return Err(Outcome::Unsupported(
            "the driver reports no AC power cycle for this platform".to_string(),
        ));
    }
    let started = Instant::now();
    let outcome = ops::sent_when_ready(cx, "power FullPowerCycle", async || {
        power.set(ResetType::FullPowerCycle).await
    })
    .await?;
    let completed =
        ops::wait_for_ac_cycle_down(cx, &outcome)
            .await
            .map_err(|outcome| match outcome {
                Outcome::Fail(reason) => {
                    Outcome::Fail(format!("no power cycle was observed: {reason}"))
                }
                other => other,
            })?;
    ops::wait_for_bmc_up(cx).await?;
    let gone = if completed {
        Ok(Vec::new())
    } else {
        ops::settle_after_ac_cycle(cx, &outcome).await
    };
    cx.driven().power_on().await?;
    let gone = gone?;
    let unknown = if gone.is_empty() {
        String::new()
    } else {
        format!(
            "; the BMC dropped {} with the restart, so how it ended is unknown",
            gone.join(", ")
        )
    };
    Ok(format!(
        "the BMC lost power, and everything was back on {} after the request ({}){unknown}",
        took(started),
        how(&outcome)
    ))
}

/// Hands east-west traffic on every SuperNIC to its embedded CPU, as NICo's
/// `ConfigureAstra` does before AC-power-cycling the host.
async fn enable_east_west(cx: &CheckCx<'_>) -> Step {
    let count = cx.inputs().required_super_nics()?;
    let bmc = cx.bmc();
    let super_nic = bmc.super_nic()?;
    let mut requests = Vec::with_capacity(count.into());
    for index in 0..count {
        let outcome = requested(
            cx,
            "set_east_west_control",
            super_nic.set_east_west_control(index, true),
        )
        .await?;
        cx.driven().wait_for_operations(&outcome).await?;
        requests.push(format!("{index}: {}", how(&outcome)));
    }
    Ok(format!(
        "requested on {} ({}); it takes effect on the AC power cycle",
        counted(count.into(), "SuperNIC", "SuperNICs"),
        requests.join(", ")
    ))
}

/// Waits until every SuperNIC reads east-west control enabled, after the AC
/// power cycle that applies it.
async fn east_west_in_effect(cx: &CheckCx<'_>) -> Step {
    let count = cx.inputs().required_super_nics()?;
    let bmc = cx.bmc();
    let super_nic = bmc.super_nic()?;
    poll(cx, SETTLE_TIMEOUT, SETTLE_POLL_INTERVAL, async || {
        let mut not_enabled = Vec::new();
        for index in 0..count {
            match super_nic.east_west_control(index).await {
                Ok(Some(true)) => {}
                Ok(state) => not_enabled.push(format!("{index} reads {}", reported(&state))),
                Err(error) => return Attempt::NotYet(describe("east_west_control", &error)),
            }
        }
        if not_enabled.is_empty() {
            Attempt::Done(())
        } else {
            Attempt::NotYet(format!("SuperNIC {}", not_enabled.join(", ")))
        }
    })
    .await?;
    Ok(format!(
        "east-west control reads enabled on {} after the AC power cycle",
        counted(count.into(), "SuperNIC", "SuperNICs")
    ))
}
