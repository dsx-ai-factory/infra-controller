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

//! The BMC account role, shared by the `create-bmc-user` commands.
//!
//! Both `redfish create-bmc-user` and `bmc-machine create-bmc-user` take a
//! role from a fixed set. Modeling it as a clap `ValueEnum` (rather than a
//! free `String`) makes clap validate it at parse time and list the choices in
//! `--help`, instead of each command hand-matching the string. The variants
//! map to `libredfish::RoleId` for the direct-Redfish path and to the
//! canonical lowercase role string for the API-request path.

use clap::ValueEnum;
use libredfish::RoleId;

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
#[clap(rename_all = "lower")]
pub(crate) enum BmcRole {
    Administrator,
    Operator,
    ReadOnly,
    NoAccess,
}

impl BmcRole {
    /// The canonical role string accepted by the API `CreateBmcUser` request
    /// (matches clap's `rename_all = "lower"` rendering).
    pub(crate) fn as_api_str(self) -> &'static str {
        match self {
            BmcRole::Administrator => "administrator",
            BmcRole::Operator => "operator",
            BmcRole::ReadOnly => "readonly",
            BmcRole::NoAccess => "noaccess",
        }
    }
}

impl From<BmcRole> for RoleId {
    fn from(role: BmcRole) -> Self {
        match role {
            BmcRole::Administrator => RoleId::Administrator,
            BmcRole::Operator => RoleId::Operator,
            BmcRole::ReadOnly => RoleId::ReadOnly,
            BmcRole::NoAccess => RoleId::NoAccess,
        }
    }
}
