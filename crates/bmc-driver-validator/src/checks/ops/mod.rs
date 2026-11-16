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
//! waiting on the work a driver started, host power, and the BMC under test
//! going down and coming back.

mod bmc;
mod poll;
mod power;
mod work;

use std::time::Instant;

pub(in crate::checks) use bmc::{
    reset_bmc, settle_after_ac_cycle, wait_for_ac_cycle_down, wait_for_bmc_down, wait_for_bmc_up,
};
use bmc_platform::DriverOutcome;
pub(in crate::checks) use poll::{
    Attempt, IMMEDIATE_TIMEOUT, QUICK_POLL_INTERVAL, RETRY_INTERVAL, SETTLE_POLL_INTERVAL,
    SETTLE_TIMEOUT, UNREACHABLE_TIMEOUT, poll, poll_traced, until_answered,
};
pub(in crate::checks) use power::set_power;
pub(in crate::checks) use work::{Effect, requested, sent_or_dropped, sent_when_ready};

/// How the driver says a request completed: at once, or by starting work.
pub(in crate::checks) fn how(outcome: &DriverOutcome) -> String {
    match outcome {
        DriverOutcome::Complete => "applied at once".to_string(),
        accepted => format!("started {}", uris(accepted)),
    }
}

pub(in crate::checks) fn took(started: Instant) -> String {
    format!("{:.1}s", started.elapsed().as_secs_f64())
}

pub(in crate::checks) fn uris(outcome: &DriverOutcome) -> String {
    outcome
        .references()
        .map(|reference| reference.uri().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}
