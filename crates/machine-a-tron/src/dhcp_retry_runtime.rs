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

use rand::RngExt;

use crate::dhcp_retry_fsm::{DHCP_RETRY_MAX_JITTER, Milliseconds};
use crate::machine_fsm::Event as MachineEvent;
use crate::power_shelf_fsm::Event as PowerShelfEvent;
use crate::switch_fsm::Event as SwitchEvent;

pub(super) fn sample_dhcp_retry_jitter() -> Milliseconds {
    let max_jitter_milliseconds = DHCP_RETRY_MAX_JITTER.value();
    Milliseconds::new(rand::rng().random_range(-max_jitter_milliseconds..=max_jitter_milliseconds))
}

impl MachineEvent {
    pub(super) fn dhcp_failed() -> Self {
        Self::DhcpFailed(sample_dhcp_retry_jitter())
    }
}

impl SwitchEvent {
    pub(super) fn dhcp_failed() -> Self {
        Self::DhcpFailed(sample_dhcp_retry_jitter())
    }
}

impl PowerShelfEvent {
    pub(super) fn dhcp_failed() -> Self {
        Self::DhcpFailed(sample_dhcp_retry_jitter())
    }
}
