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

mod accounts;
mod bios;
mod bmc_control;
mod connector;
mod convert;
mod dpu;
mod lockdown;
mod power;
mod secure_boot;
mod status;
mod storage;

pub use accounts::{BackendAccountBmc, BackendAccounts};
pub use bios::BackendBios;
pub use bmc_control::BackendBmcControl;
use bmc_runtime::ConnectedBmc;
use carbide_redfish::nv_redfish::RedfishBmc;
pub use connector::{BmcConnectError, BmcConnector};
pub use convert::platform_error;
pub use dpu::BackendDpu;
use libredfish::Redfish;
pub use lockdown::BackendLockdown;
pub use power::BackendPower;
pub use secure_boot::BackendSecureBoot;
pub use status::{
    idrac_job, idrac_outcome, libredfish_operation_status, redfish_task, redfish_task_id,
};
pub use storage::BackendStorage;

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
