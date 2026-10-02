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
//! This crate intentionally contains no transport implementation, credentials,
//! endpoint origins, or API-model types.

pub mod capabilities;
mod error;
mod identity;
mod operation;
mod selection;
mod transport;

pub use capabilities::{
    Accounts, Attestation, AttestationEvidence, Bios, BiosDiff, BiosSettings, BiosStatus,
    BmcControl, BootInterfaceSelector, BootOrder, BootOrderStatus, CaCertificate,
    ComponentIntegritySummary, Console, ConsoleFallback, ConsoleSpec, ConsoleSpecError,
    ConsoleState, ConsoleStatus, Dpu, DpuStatus, EscapeSeq, Firmware, HostPrivilegeLevel, Lockdown,
    LockdownDesiredState, LockdownScope, LockdownState, LockdownStatus, NicMode, Power, RshimState,
    SecureBoot, SecureBootStatus, Storage,
};
pub use error::{AuthError, PlatformError};
pub use identity::{
    ChassisIdentity, ManagerIdentity, PlatformIdentity, ServiceRootIdentity, SystemIdentity,
};
pub use operation::{
    ControllerAction, DriverOutcome, ManualInterventionCode, ManualInterventionCodeError,
    OperationReference, VendorJobId, VendorJobIdError,
};
pub use selection::{Capability, CapabilitySelection, DriverId, DriverIdError, DriverMap};
pub use transport::{IpmiOps, OpCx};
