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

//! Checks that change the BMC or host and verify the effect. Settings take
//! effect on a host reset; power and account checks restore what they
//! change.

mod accounts;
mod lockdown;
mod settings;
mod storage;

use bmc_platform::Capability;
pub(in crate::checks) use lockdown::{enable_lockdown, lift_lockdown};
use nv_redfish::resource::{PowerState, ResetType};

use super::context::CheckCx;
use super::errors::failed;
use super::model::{Check, CheckId, Needs, Outcome, Step};

pub(super) const BIOS: CheckId = CheckId::new(Capability::Bios, "apply");
pub(super) const BOOT_ORDER: CheckId = CheckId::new(Capability::BootOrder, "configure");
pub(super) const CONSOLE: CheckId = CheckId::new(Capability::Console, "setup");
pub(super) const POWER_CYCLE: CheckId = CheckId::new(Capability::Power, "cycle");
pub(super) const STORAGE_ERASE: CheckId =
    CheckId::new(Capability::Storage, "decommission_controller");
pub(super) const STORAGE_VOLUME: CheckId = CheckId::new(Capability::Storage, "create_volume");
pub(super) const ACCOUNTS_LIFECYCLE: CheckId = CheckId::new(Capability::Accounts, "lifecycle");

/// Every apply check, once each.
pub(super) fn checks() -> Vec<Check> {
    vec![
        Check::apply(BIOS, run!(settings::bios_apply)),
        Check::apply(BOOT_ORDER, run!(settings::boot_order_configure))
            .needs(&[Needs::BootInterface]),
        Check::apply(CONSOLE, run!(settings::console_setup)),
        Check::apply(POWER_CYCLE, run!(cycle_power)).once(),
        // Erases the boot controller's drives, as NICo's cleanup does first.
        Check::apply(STORAGE_ERASE, run!(storage::erase_boot_controller))
            .once()
            .needs(&[Needs::StorageErase]),
        // Creates the boot volume NICo's cleanup creates after the erase.
        Check::apply(STORAGE_VOLUME, run!(storage::create_boot_volume))
            .once()
            .needs(&[Needs::StorageErase]),
        Check::apply(ACCOUNTS_LIFECYCLE, run!(accounts::account_lifecycle)).once(),
    ]
}

/// Cycles host power away from its current state and back, then records
/// the change as not restored unless the host reads its original state.
async fn cycle_power(cx: &CheckCx<'_>) -> Step {
    let Some(original) = cx.power_state().await.map_err(failed("state"))? else {
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
    let away = cx.set_power(away).await;
    let back = cx.set_power(back).await;
    match cx.power_state().await {
        Ok(Some(current)) if current == original => {}
        current => cx.not_restored(format!(
            "host power was {original:?}; it now reads {current:?}"
        )),
    }
    Ok(format!("{}; {}", away?, back?))
}
