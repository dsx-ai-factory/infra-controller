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
use std::fmt::{Display, Formatter};

use lazy_static::lazy_static;

lazy_static! {
    pub static ref HOST_UPDATE_HEALTH_PROBE_ID: health_report::HealthProbeId =
        "HostUpdateInProgress".parse().unwrap();
}

/// The name of the Health Override which will be used to indicate an ongoing host update
pub const HOST_UPDATE_HEALTH_REPORT_SOURCE: &str = "host-update";
pub const HOST_FW_UPDATE_HEALTH_REPORT_SOURCE: &str = "host-fw-update";
pub const DPU_FIRMWARE_UPDATE_TARGET: &str = "DpuFirmware";

pub struct AutomaticFirmwareUpdateReference {
    pub from: String,
    pub to: String,
}

impl AutomaticFirmwareUpdateReference {
    pub const REF_NAME: &'static str = "AutomaticDpuFirmwareUpdate";
}

pub enum DpuReprovisionInitiator {
    Automatic(AutomaticFirmwareUpdateReference),
}

impl Display for DpuReprovisionInitiator {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            DpuReprovisionInitiator::Automatic(x) => write!(
                f,
                "{}/{}/{}",
                AutomaticFirmwareUpdateReference::REF_NAME,
                x.from,
                x.to
            ),
        }
    }
}
