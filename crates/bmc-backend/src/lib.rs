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

//! Routes migrated BMC operations to libredfish or to the driver system, as
//! the site's `bmc_backend` chooses. This crate is removed with libredfish.
//!
//! Each capability's file adds its accessor to [`BackendBmc`] and mirrors the
//! name and signature of every [`ConnectedBmc`] counterpart, so callers keep
//! their code when they hold a [`ConnectedBmc`] instead.

mod connector;
mod convert;
mod power;

use bmc_runtime::ConnectedBmc;
use carbide_redfish::nv_redfish::RedfishBmc;
pub use connector::{BmcConnectError, BmcConnector};
use libredfish::Redfish;
pub use power::BackendPower;

/// A connected BMC on the configured backend.
pub enum BackendBmc {
    Libredfish(Box<dyn Redfish>),
    Drivers(Box<ConnectedBmc<RedfishBmc>>),
}

impl From<Box<dyn Redfish>> for BackendBmc {
    fn from(client: Box<dyn Redfish>) -> Self {
        Self::Libredfish(client)
    }
}

impl From<ConnectedBmc<RedfishBmc>> for BackendBmc {
    fn from(bmc: ConnectedBmc<RedfishBmc>) -> Self {
        Self::Drivers(Box::new(bmc))
    }
}
