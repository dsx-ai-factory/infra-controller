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

use crate::PlatformError;

/// Restricted IPMI operations available to capability drivers.
#[async_trait]
pub trait IpmiOps: Send + Sync {
    async fn bmc_cold_reset(&self) -> Result<(), PlatformError>;

    async fn chassis_power_reset(&self) -> Result<(), PlatformError>;

    async fn dpu_legacy_power_reset(&self) -> Result<(), PlatformError>;
}
