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

mod accounts;
mod attestation;
mod bios;
mod bmc_control;
mod boot_order;
mod console;
mod dpu;
mod firmware;
mod lockdown;
mod power;
mod secure_boot;
mod storage;
mod super_nic;

pub use accounts::Accounts;
pub use attestation::{Attestation, EvidenceProgress};
pub use bios::{Bios, BiosDiff, BiosSettings, BiosStatus};
pub use bmc_control::{
    BmcControl, ManagerSettings, ManagerSettingsDiff, ManagerSettingsStatus, TimeStatus,
};
pub use boot_order::{BootInterfaceSelector, BootOrder, BootOrderStatus};
pub use console::{
    Console, ConsoleFallback, ConsoleSpec, ConsoleSpecError, ConsoleState, ConsoleStatus,
    EscapeSeq, NonEmptyBytes, NonEmptyBytesError, SshShellSpec,
};
pub use dpu::{Dpu, DpuStatus, HostPrivilegeLevel, NicMode, RshimState};
pub use firmware::{Firmware, FirmwareComponent, FirmwareUpload};
pub use lockdown::{Lockdown, LockdownDesiredState, LockdownScope, LockdownState, LockdownStatus};
pub use power::Power;
pub use secure_boot::{SecureBoot, SecureBootStatus};
pub use storage::Storage;
pub use super_nic::SuperNic;
