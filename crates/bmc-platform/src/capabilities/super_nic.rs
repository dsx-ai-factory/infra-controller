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

use async_trait::async_trait;
use nv_redfish::core::Bmc;

use crate::{DriverOutcome, OpCx, PlatformError};

/// The host's SuperNICs, addressed by their index among the host's CX9 NICs.
#[async_trait]
pub trait SuperNic<B: Bmc>: Send + Sync {
    /// Whether the NIC's embedded CPU (ECPF) manages its east-west traffic;
    /// `None` when the NIC does not report it.
    async fn east_west_control(
        &self,
        cx: &OpCx<'_, B>,
        index: u8,
    ) -> Result<Option<bool>, PlatformError>;

    /// Hands the NIC's east-west traffic to its embedded CPU, or back to the
    /// host. NICo AC-power-cycles the host for the change to take effect.
    async fn set_east_west_control(
        &self,
        cx: &OpCx<'_, B>,
        index: u8,
        enabled: bool,
    ) -> Result<DriverOutcome, PlatformError>;

    /// The MAC address of the NIC's host interface, as the BMC reports it;
    /// `None` when the interface reports none.
    async fn mac_address(
        &self,
        cx: &OpCx<'_, B>,
        index: u8,
    ) -> Result<Option<String>, PlatformError>;
}
