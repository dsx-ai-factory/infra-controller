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

use clap::{Parser, ValueEnum};

/// Redfish `Manager.Reset` type selectable on the `bmc-reset` commands. Shared
/// by `bmc-machine bmc-reset` and `redfish bmc-reset`. Omitting it leaves the
/// reset type unspecified (the per-vendor default).
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "kebab_case")]
pub(crate) enum ResetTypeArg {
    Graceful,
    Force,
}

impl From<ResetTypeArg> for rpc::forge::admin_bmc_reset_request::ResetType {
    fn from(arg: ResetTypeArg) -> Self {
        match arg {
            ResetTypeArg::Graceful => Self::GracefulRestart,
            ResetTypeArg::Force => Self::ForceRestart,
        }
    }
}

impl From<ResetTypeArg> for libredfish::ManagerResetType {
    fn from(arg: ResetTypeArg) -> Self {
        match arg {
            ResetTypeArg::Graceful => Self::GracefulRestart,
            ResetTypeArg::Force => Self::ForceRestart,
        }
    }
}

#[derive(ValueEnum, Parser, Debug, Clone)]
pub(super) enum AdminPowerControlAction {
    On,
    GracefulShutdown,
    ForceOff,
    GracefulRestart,
    ForceRestart,
    ACPowercycle,
}

impl From<AdminPowerControlAction> for rpc::forge::admin_power_control_request::SystemPowerControl {
    fn from(c_type: AdminPowerControlAction) -> Self {
        match c_type {
            AdminPowerControlAction::On => {
                rpc::forge::admin_power_control_request::SystemPowerControl::On
            }
            AdminPowerControlAction::GracefulShutdown => {
                rpc::forge::admin_power_control_request::SystemPowerControl::GracefulShutdown
            }
            AdminPowerControlAction::ForceOff => {
                rpc::forge::admin_power_control_request::SystemPowerControl::ForceOff
            }
            AdminPowerControlAction::GracefulRestart => {
                rpc::forge::admin_power_control_request::SystemPowerControl::GracefulRestart
            }
            AdminPowerControlAction::ForceRestart => {
                rpc::forge::admin_power_control_request::SystemPowerControl::ForceRestart
            }
            AdminPowerControlAction::ACPowercycle => {
                rpc::forge::admin_power_control_request::SystemPowerControl::AcPowercycle
            }
        }
    }
}

#[derive(Parser, Debug, Clone)]
pub(super) struct InfiniteBootArgs {
    #[clap(long, help = "ID of the machine to enable/query infinite boot")]
    pub(super) machine: String,
    #[clap(short, long, help = "Issue reboot to apply BIOS change")]
    pub(super) reboot: bool,
}
