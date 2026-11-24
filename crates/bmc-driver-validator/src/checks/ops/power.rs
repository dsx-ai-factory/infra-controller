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

//! Host power through a BMC's power driver: reading it, changing it, and
//! waiting for the state a change leads to.

use std::time::{Duration, Instant};

use bmc_platform::PlatformError;
use bmc_runtime::ConnectedBmc;
use carbide_redfish::nv_redfish::RedfishBmc;
use nv_redfish::resource::{PowerState, ResetType};

use super::poll::{Attempt, QUICK_POLL_INTERVAL, poll};
use super::work::wait_for_operations;
use super::{how, took};
use crate::checks::context::CheckCx;
use crate::checks::errors::{describe, failed};
use crate::checks::model::{Outcome, Step};

/// How long a power transition may take to show in the reported state.
const POWER_TIMEOUT: Duration = Duration::from_secs(10 * 60);

impl CheckCx<'_> {
    /// The host power state through the power driver, read afresh.
    pub(in crate::checks) async fn power_state(&self) -> Result<Option<PowerState>, PlatformError> {
        power_state(&self.bmc()).await
    }

    /// Requests `reset` through the power driver and waits for the state it
    /// leads to.
    pub(in crate::checks) async fn set_power(&self, reset: ResetType) -> Step {
        set_power(self, &self.bmc(), reset).await
    }

    /// Restarts the host, or powers it on when it is off, and waits until it
    /// reports on.
    pub(in crate::checks) async fn restart_host(&self) -> Result<(), Outcome> {
        let reset = match self.power_state().await.map_err(failed("state"))? {
            Some(PowerState::Off) => ResetType::On,
            _ => ResetType::ForceRestart,
        };
        self.set_power(reset).await.map(drop)
    }

    /// Leaves the host on: powers it on when it reads off, otherwise waits
    /// for on. Reads are retried while the BMC is still coming back.
    pub(in crate::checks) async fn power_on(&self) -> Result<(), Outcome> {
        let state = poll(
            self,
            POWER_TIMEOUT,
            QUICK_POLL_INTERVAL,
            async || match self.power_state().await {
                Ok(state) => Attempt::Done(state),
                Err(error) => Attempt::NotYet(describe("power state", &error)),
            },
        )
        .await?;
        match state {
            Some(PowerState::Off) => self.set_power(ResetType::On).await.map(drop),
            _ => wait_for_power(self, &self.bmc(), PowerState::On).await,
        }
    }
}

/// Requests `reset` through `bmc`'s power driver and waits for the state it
/// leads to: on the BMC under test, or another the run reaches, such as a
/// DPU's host BMC.
pub(in crate::checks) async fn set_power(
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
    wait_for_operations(cx, bmc, &outcome).await?;
    wait_for_power(cx, bmc, target).await?;
    cx.trace(format!("power reads {target:?}"));
    Ok(format!("{reset:?} reached {target:?} in {}", took(started)))
}

async fn power_state(bmc: &ConnectedBmc<RedfishBmc>) -> Result<Option<PowerState>, PlatformError> {
    bmc.power()
        .map_err(|_| PlatformError::Unsupported)?
        .state()
        .await
}

async fn wait_for_power(
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
