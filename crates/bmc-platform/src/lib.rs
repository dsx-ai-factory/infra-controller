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

//! Contracts shared by BMC operation drivers and their runtime.
//!
//! A capability whose operations have a spec-compliant Redfish form defaults
//! every operation to the driver named by its `standard` method, so a vendor
//! or model driver implements only its deviations. The standard behavior
//! itself lives with the drivers.
//!
//! This crate intentionally contains no transport implementation, credentials,
//! endpoint origins, or API-model types.

mod capabilities;
mod error;
mod identity;
mod matcher;
mod operation;
mod quirk;
mod selection;
mod transport;

pub use capabilities::{
    Accounts, Attestation, Bios, BiosDiff, BiosSettings, BiosStatus, BmcControl,
    BootInterfaceSelector, BootOrder, BootOrderStatus, Console, ConsoleFallback, ConsoleSpec,
    ConsoleSpecError, ConsoleState, ConsoleStatus, Dpu, DpuStatus, EscapeSeq, EvidenceProgress,
    Firmware, FirmwareComponent, FirmwareUpload, HostPrivilegeLevel, Lockdown,
    LockdownDesiredState, LockdownScope, LockdownState, LockdownStatus, ManagerSettings,
    ManagerSettingsDiff, ManagerSettingsStatus, NicMode, NonEmptyBytes, NonEmptyBytesError, Power,
    RshimState, SecureBoot, SecureBootStatus, SshShellSpec, Storage, SuperNic, TimeStatus,
};
pub use error::{AuthError, PlatformError};
pub use identity::{
    ChassisIdentity, FirmwareInventoryIdentity, ManagerIdentity, PlatformIdentity,
    ServiceRootIdentity, SystemIdentity,
};
pub use matcher::{
    FirmwareVersionRange, FirmwareVersionRangeError, IdentityField, IdentityMatcher, MatchPattern,
    Precedence, derived_precedence,
};
pub use operation::{
    DriverOutcome, OperationReference, OperationStatus, VendorJobId, VendorJobIdError,
};
pub use quirk::Quirk;
pub use selection::Capability;
pub use transport::{ClassifyBmcError, Fetched, IpmiOps, OpCx};
