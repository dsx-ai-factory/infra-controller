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

use crate::expected_machine::ExpectedMachine;
use crate::expected_power_shelf::ExpectedPowerShelf;
use crate::expected_switch::ExpectedSwitch;

pub enum ExpectedEntity {
    Machine(ExpectedMachine),
    PowerShelf(ExpectedPowerShelf),
    Switch(ExpectedSwitch),
}

impl ExpectedEntity {
    pub fn bmc_credentials_data(&self) -> BmcCredentialsData<'_> {
        match self {
            Self::Machine(v) => BmcCredentialsData {
                username: &v.data.bmc_username,
                password: &v.data.bmc_password,
                retain_credentials: v.data.bmc_retain_credentials.unwrap_or(false),
            },
            Self::PowerShelf(v) => BmcCredentialsData {
                username: &v.bmc_username,
                password: &v.bmc_password,
                retain_credentials: v.bmc_retain_credentials.unwrap_or(false),
            },
            Self::Switch(v) => BmcCredentialsData {
                username: &v.bmc_username,
                password: &v.bmc_password,
                retain_credentials: v.bmc_retain_credentials.unwrap_or(false),
            },
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Machine(_) => "machine",
            Self::PowerShelf(_) => "power shelf",
            Self::Switch(_) => "switch",
        }
    }
}

#[derive(Clone, Copy)]
pub struct BmcCredentialsData<'a> {
    pub username: &'a str,
    pub password: &'a str,
    pub retain_credentials: bool,
}
