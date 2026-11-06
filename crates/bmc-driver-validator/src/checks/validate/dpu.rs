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

//! DPU discovery and initialization, with the DPU's own restarts.

use std::time::Instant;

use bmc_platform::{
    Capability, HostPrivilegeLevel, NicMode, OpCx, PlatformError, RshimState, SecureBootStatus,
};
use bmc_runtime::ConnectedBmc;
use carbide_redfish::nv_redfish::RedfishBmc;
use nv_redfish::resource::ResetType;
use nv_redfish::schema::computer_system::{BootSource, BootSourceOverrideEnabled, BootUpdate};
use nv_redfish::schema::secure_boot::SecureBootUpdate;

use super::{BMC_RESET, uefi_password};
use crate::checks::apply::{self, BIOS};
use crate::checks::context::CheckCx;
use crate::checks::errors::{describe, failed};
use crate::checks::model::{Check, CheckId, Needs, Outcome, Step};
use crate::checks::ops::{
    self, Attempt, Effect, SETTLE_POLL_INTERVAL, SETTLE_TIMEOUT, how, poll, requested, took,
};
use crate::checks::text::reported;
use crate::connect::Target;
use crate::selection::Selection;

const BMC_RSHIM: CheckId = CheckId::new("dpu.bmc_rshim");
const NIC_MODE: CheckId = CheckId::new("dpu.nic_mode");
const HOST_PRIVILEGE: CheckId = CheckId::new("dpu.host_privilege");
const SECURE_BOOT: CheckId = CheckId::new("secure_boot.enable");
const HTTP_BOOT_ONCE: CheckId = CheckId::new("boot_order.http_boot_once");
const HOST_RSHIM: CheckId = CheckId::new("dpu.host_rshim");

pub(super) fn setup() -> Vec<Check> {
    vec![
        Check::apply(BMC_RSHIM, Capability::Dpu, run!(bmc_rshim)).after(&[BMC_RESET]),
        Check::apply(NIC_MODE, Capability::Dpu, run!(nic_mode))
            .once()
            .needs(&[Needs::HostBmc])
            .after(&[BMC_RESET]),
        Check::apply(HOST_PRIVILEGE, Capability::Dpu, run!(host_privilege)).after(&[NIC_MODE]),
        apply::bios().after(&[HOST_PRIVILEGE]),
        Check::apply(SECURE_BOOT, Capability::SecureBoot, run!(secure_boot)).after(&[BIOS]),
        Check::apply(HTTP_BOOT_ONCE, Capability::BootOrder, run!(http_boot_once))
            .after(&[SECURE_BOOT]),
        Check::apply(HOST_RSHIM, Capability::Dpu, run!(host_rshim)).after(&[NIC_MODE]),
        apply::console(),
        uefi_password().after(&[BIOS]),
    ]
}

async fn bmc_rshim(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let dpu = bmc.dpu()?;
    let outcome = requested(cx, "enable_bmc_rshim", dpu.enable_bmc_rshim()).await?;
    ops::read_back(cx, &outcome, Effect::Immediate, async || {
        match dpu.status().await {
            Ok(status) if status.bmc_rshim == Some(RshimState::Enabled) => Attempt::Done(()),
            Ok(status) => {
                Attempt::NotYet(format!("BMC rshim reads {}", reported(&status.bmc_rshim)))
            }
            Err(error) => Attempt::NotYet(describe("status", &error)),
        }
    })
    .await?;
    Ok(format!("BMC rshim reads Enabled ({})", how(&outcome)))
}

/// Switches the DPU to NIC mode and back to DPU mode, so both directions are
/// exercised and the DPU ends in DPU mode as NICo leaves it; a DPU found in
/// NIC mode goes to DPU mode first. Each switch takes a host power cycle.
async fn nic_mode(cx: &CheckCx<'_>) -> Step {
    let original = cx
        .bmc()
        .dpu()?
        .status()
        .await
        .map_err(failed("status"))?
        .nic_mode;
    let host = cx.inputs().required_host_bmc()?;
    let modes: &[NicMode] = if original == Some(NicMode::Dpu) {
        &[NicMode::Nic, NicMode::Dpu]
    } else {
        &[NicMode::Dpu, NicMode::Nic, NicMode::Dpu]
    };
    let mut switched = Vec::new();
    for mode in modes {
        match switch_nic_mode(cx, host, *mode).await {
            Ok(step) => switched.push(step),
            Err(outcome) => {
                if original == Some(NicMode::Dpu) {
                    cx.not_restored(format!(
                        "nic mode was Dpu; switching to {mode:?} failed, so it may not be restored"
                    ));
                }
                return Err(outcome);
            }
        }
    }
    Ok(switched.join("; "))
}

/// Sets `mode`, power-cycles the host through its BMC, and waits for the DPU
/// to report `mode`.
async fn switch_nic_mode(cx: &CheckCx<'_>, host: &Target, mode: NicMode) -> Step {
    let started = Instant::now();
    let bmc = cx.bmc();
    let outcome = requested(cx, "set_nic_mode", bmc.dpu()?.set_nic_mode(mode)).await?;
    ops::settle(cx, &outcome).await?;
    let host = connect_host(host).await?;
    cx.trace("power-cycling the host through its BMC");
    ops::set_power(cx, &host, ResetType::ForceOff).await?;
    ops::set_power(cx, &host, ResetType::On).await?;
    ops::wait_for_bmc_up(cx).await?;
    let bmc = cx.bmc();
    let dpu = bmc.dpu()?;
    poll(
        cx,
        SETTLE_TIMEOUT,
        SETTLE_POLL_INTERVAL,
        async || match dpu.status().await {
            Ok(status) if status.nic_mode == Some(mode) => Attempt::Done(()),
            Ok(status) => Attempt::NotYet(format!(
                "the DPU reports nic mode {}",
                reported(&status.nic_mode)
            )),
            Err(error) => Attempt::NotYet(describe("status", &error)),
        },
    )
    .await?;
    Ok(format!(
        "{mode:?} mode {} after a host power cycle ({})",
        took(started),
        how(&outcome)
    ))
}

/// Discovers the host BMC and connects to it with the drivers its own
/// identity selects.
async fn connect_host(host: &Target) -> Result<ConnectedBmc<RedfishBmc>, Outcome> {
    let reach = async {
        let identity = host.discover().await?;
        let selection = Selection::resolve(&identity, &[])?;
        host.connect(selection.endpoint(host.address(), identity)?)
            .await
    };
    reach
        .await
        .map_err(|error| Outcome::Fail(format!("host BMC {}: {error}", host.address())))
}

async fn host_privilege(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let dpu = bmc.dpu()?;
    let set = async || {
        dpu.set_host_privilege_level(HostPrivilegeLevel::Restricted)
            .await
    };
    // The BIOS reports its attributes only as the DPU starts, so after a BMC
    // restart the write waits for a DPU restart.
    cx.trace("set_host_privilege_level …");
    let outcome = match set().await {
        Err(PlatformError::NotReady { message }) => {
            cx.trace(format!("{message}; restarting the DPU"));
            ops::restart_host(cx).await?;
            poll(
                cx,
                SETTLE_TIMEOUT,
                SETTLE_POLL_INTERVAL,
                async || match set().await {
                    Ok(outcome) => Attempt::Done(outcome),
                    Err(PlatformError::NotReady { message }) => Attempt::NotYet(message),
                    Err(error) => Attempt::Failed(failed("set_host_privilege_level")(error)),
                },
            )
            .await?
        }
        result => result.map_err(failed("set_host_privilege_level"))?,
    };
    cx.trace(format!("set_host_privilege_level: {}", how(&outcome)));
    ops::read_back(cx, &outcome, Effect::OnRestart, async || {
        match dpu.status().await {
            Ok(status) if status.host_privilege == Some(HostPrivilegeLevel::Restricted) => {
                Attempt::Done(())
            }
            Ok(status) => Attempt::NotYet(format!(
                "host privilege reads {}",
                reported(&status.host_privilege)
            )),
            Err(error) => Attempt::NotYet(describe("status", &error)),
        }
    })
    .await?;
    Ok(format!(
        "host privilege reads Restricted after a DPU restart ({})",
        how(&outcome)
    ))
}

async fn secure_boot(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let secure_boot = bmc.secure_boot()?;
    let mut outcome = requested(
        cx,
        "set",
        secure_boot.set(
            &SecureBootUpdate::builder()
                .with_secure_boot_enable(true)
                .build(),
        ),
    )
    .await?;
    let platform_key = cx.inputs().platform_key.as_ref();
    if let Some(key) = platform_key {
        outcome = outcome
            .merge(requested(cx, "add_platform_key", secure_boot.add_platform_key(key)).await?);
    }
    let want_key = platform_key.is_some();
    ops::read_back(cx, &outcome, Effect::OnRestart, async || {
        let status = secure_boot.status().await;
        let key = secure_boot.has_platform_key().await;
        match (status, key) {
            (Ok(SecureBootStatus::Enabled), Ok(key)) if key || !want_key => Attempt::Done(()),
            (Ok(status), Ok(key)) => Attempt::NotYet(format!(
                "secure boot reads {status:?}; platform key enrolled: {key}"
            )),
            (Err(error), _) | (_, Err(error)) => {
                Attempt::NotYet(describe("secure boot status", &error))
            }
        }
    })
    .await?;
    Ok(format!(
        "Enabled{} after a DPU restart ({})",
        if want_key {
            ", platform key enrolled"
        } else {
            ""
        },
        how(&outcome)
    ))
}

/// The system's boot override as `(target, enabled)`, read over a fresh
/// connection from the system the drivers act on.
async fn boot_override(
    cx: &CheckCx<'_>,
) -> Result<(Option<BootSource>, Option<BootSourceOverrideEnabled>), PlatformError> {
    let connection = cx.target().fresh_connection().await?;
    let bmc = cx.bmc();
    let op = OpCx::new(
        connection.bmc.as_ref(),
        connection.service_root.as_ref(),
        bmc.endpoint().identity(),
    );
    let raw = op.system().await?.raw();
    let boot = raw.boot.as_ref();
    Ok((
        boot.and_then(|boot| boot.boot_source_override_target.flatten()),
        boot.and_then(|boot| boot.boot_source_override_enabled.flatten()),
    ))
}

/// Sets a one-time UEFI HTTP boot, restarts the DPU, and checks the
/// override was consumed.
async fn http_boot_once(cx: &CheckCx<'_>) -> Step {
    let http_once = (
        Some(BootSource::UefiHttp),
        Some(BootSourceOverrideEnabled::Once),
    );
    let outcome = requested(
        cx,
        "set_override",
        cx.bmc().boot_order()?.set_override(
            &BootUpdate::builder()
                .with_boot_source_override_target(BootSource::UefiHttp)
                .with_boot_source_override_enabled(BootSourceOverrideEnabled::Once)
                .build(),
        ),
    )
    .await?;
    ops::read_back(
        cx,
        &outcome,
        Effect::Immediate,
        async || match boot_override(cx).await {
            Ok(set) if set == http_once => Attempt::Done(()),
            Ok((target, enabled)) => Attempt::NotYet(format!(
                "the override reads {} {}",
                reported(&target),
                reported(&enabled)
            )),
            Err(error) => Attempt::NotYet(describe("reading ComputerSystem.Boot", &error)),
        },
    )
    .await?;
    ops::restart_host(cx).await?;
    poll(
        cx,
        SETTLE_TIMEOUT,
        SETTLE_POLL_INTERVAL,
        async || match boot_override(cx).await {
            Ok(set) if set == http_once => {
                Attempt::NotYet("the one-time override is still set".to_string())
            }
            Ok(_) => Attempt::Done(()),
            Err(error) => Attempt::NotYet(describe("reading ComputerSystem.Boot", &error)),
        },
    )
    .await?;
    Ok(format!(
        "UefiHttp once was set ({}) and consumed by a DPU restart",
        how(&outcome)
    ))
}

async fn host_rshim(cx: &CheckCx<'_>) -> Step {
    let bmc = cx.bmc();
    let dpu = bmc.dpu()?;
    let outcome = requested(
        cx,
        "set_host_rshim",
        dpu.set_host_rshim(RshimState::Enabled),
    )
    .await?;
    ops::read_back(cx, &outcome, Effect::Immediate, async || {
        match dpu.status().await {
            Ok(status) if status.host_rshim == Some(RshimState::Enabled) => Attempt::Done(()),
            Ok(status) => {
                Attempt::NotYet(format!("host rshim reads {}", reported(&status.host_rshim)))
            }
            Err(error) => Attempt::NotYet(describe("status", &error)),
        }
    })
    .await?;
    Ok(format!("host rshim Enabled ({})", how(&outcome)))
}
