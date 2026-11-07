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

//! Machine-identity bounds and serde defaults shared by agent config validation (**carbide-host-support**)
//! and by **carbide-dpu-fmds-shared** (FMDS / agent IMDS identity). IMDS handlers and protobuf mapping live in
//! **carbide-dpu-fmds-shared**.

/// Default numeric values for agent `[machine-identity]` (TOML serde) and
/// `MachineIdentityParams` [`Default`] (**carbide-dpu-fmds-shared**). Each lies within [`limits`].
pub mod defaults {
    pub const REQUESTS_PER_SECOND: u8 = 3;
    pub const BURST: u8 = 8;
    pub const WAIT_TIMEOUT_SECS: u8 = 2;
    pub const SIGN_TIMEOUT_SECS: u8 = 5;
}

/// Numeric bounds for agent `[machine-identity]` (TOML) and `FmdsMachineIdentityConfig` (gRPC).
pub mod limits {
    /// Minimum `requests_per_second` / `requests-per-second`.
    pub const REQUESTS_PER_SECOND_MIN: u8 = 1;
    /// Maximum `requests_per_second` / `requests-per-second`.
    pub const REQUESTS_PER_SECOND_MAX: u8 = 20;

    /// Minimum `burst`.
    pub const BURST_MIN: u8 = 1;
    /// Maximum `burst`.
    pub const BURST_MAX: u8 = 40;

    /// Minimum `wait_timeout_secs` / `wait-timeout-secs`.
    pub const WAIT_TIMEOUT_SECS_MIN: u8 = 1;
    /// Maximum `wait_timeout_secs` / `wait-timeout-secs`.
    pub const WAIT_TIMEOUT_SECS_MAX: u8 = 10;

    /// Minimum `sign_timeout_secs` / `sign-timeout-secs`.
    pub const SIGN_TIMEOUT_SECS_MIN: u8 = 1;
    /// Maximum `sign_timeout_secs` / `sign-timeout-secs`.
    pub const SIGN_TIMEOUT_SECS_MAX: u8 = 60;
}
