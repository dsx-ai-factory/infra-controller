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

//! What a check is, what it may need from the operator, and how it ends.

use std::fmt;
use std::pin::Pin;
use std::time::Duration;

use bmc_platform::{BiosSettings, BootInterfaceSelector, Capability};
use mac_address::MacAddress;

use super::context::CheckCx;
use crate::connect::Target;

/// What the operator supplied for checks to use.
pub(crate) struct Inputs {
    /// MAC address of the interface boot order puts first.
    pub(crate) boot_mac: Option<MacAddress>,
    pub(crate) bios_profile: BiosSettings,
    pub(crate) ntp_servers: Vec<String>,
    pub(crate) uefi_password: Option<UefiPassword>,
    pub(crate) allow_ac_cycle: bool,
    /// Whether the storage checks may erase the boot controller.
    pub(crate) allow_storage_erase: bool,
    /// PEM of the platform key secure boot enrolls.
    pub(crate) platform_key: Option<String>,
    /// The host BMC that power-cycles a DPU's host.
    pub(crate) host_bmc: Option<Target>,
    /// The factory account, which a factory reset must restore.
    pub(crate) factory: Option<Target>,
    /// How many CX9 SuperNICs the host has.
    pub(crate) super_nics: Option<u8>,
}

pub(crate) struct UefiPassword {
    /// The password the firmware has now; empty when none is set.
    pub(crate) current: String,
    pub(crate) new: String,
}

/// An input a check cannot run without. The runner skips a check that
/// needs an input the operator did not supply, so the `required_*`
/// accessors only fail for a check that does not declare what it reads.
#[derive(Clone, Copy, Debug)]
pub(super) enum Needs {
    BootInterface,
    NtpServers,
    UefiPassword,
    AcCycle,
    StorageErase,
    HostBmc,
    SuperNics,
}

impl Needs {
    /// How a check ends without the input: skipped, naming its flag.
    pub(super) fn skipped(self) -> Outcome {
        let flag = match self {
            Self::BootInterface => "--boot-mac",
            Self::NtpServers => "--ntp-server",
            Self::UefiPassword => "--uefi-password",
            Self::AcCycle => "--allow-ac-cycle",
            Self::StorageErase => "--allow-storage-erase",
            Self::HostBmc => "--host-bmc",
            Self::SuperNics => "--super-nics",
        };
        Outcome::Skipped(format!("needs {flag}"))
    }
}

impl Inputs {
    pub(super) fn has(&self, needs: Needs) -> bool {
        match needs {
            Needs::BootInterface => self.boot_mac.is_some(),
            Needs::NtpServers => !self.ntp_servers.is_empty(),
            Needs::UefiPassword => self.uefi_password.is_some(),
            Needs::AcCycle => self.allow_ac_cycle,
            Needs::StorageErase => self.allow_storage_erase,
            Needs::HostBmc => self.host_bmc.is_some(),
            Needs::SuperNics => self.super_nics.is_some(),
        }
    }

    pub(super) fn boot_interface(&self) -> Option<BootInterfaceSelector> {
        self.boot_mac.map(BootInterfaceSelector::Mac)
    }

    pub(super) fn required_boot_interface(&self) -> Result<BootInterfaceSelector, Outcome> {
        self.boot_interface()
            .ok_or_else(|| Needs::BootInterface.skipped())
    }

    pub(super) fn required_uefi_password(&self) -> Result<&UefiPassword, Outcome> {
        self.uefi_password
            .as_ref()
            .ok_or_else(|| Needs::UefiPassword.skipped())
    }

    pub(super) fn required_super_nics(&self) -> Result<u8, Outcome> {
        self.super_nics.ok_or_else(|| Needs::SuperNics.skipped())
    }

    pub(super) fn required_host_bmc(&self) -> Result<&Target, Outcome> {
        self.host_bmc
            .as_ref()
            .ok_or_else(|| Needs::HostBmc.skipped())
    }
}

/// How a check ended.
#[derive(Debug)]
pub(crate) enum Outcome {
    Pass(Detail),
    Fail(String),
    /// The BMC did not answer or reported itself temporarily unavailable, as
    /// while a reset, a settings change, or a host POST occupies it. The
    /// runner runs the check again until it answers or the wait runs out.
    Unreachable(String),
    /// The selection or the driver reports the operation unsupported.
    Unsupported(String),
    /// The check could not run: an input is missing or the platform lacks
    /// what it needs.
    Skipped(String),
    /// A check this one follows failed or was blocked itself.
    Blocked(String),
}

/// What a passing check observed: a one-line summary and the name/value
/// fields shown beneath it.
#[derive(Debug, Default)]
pub(crate) struct Detail {
    pub(crate) summary: String,
    pub(crate) fields: Vec<(String, String)>,
}

impl From<String> for Detail {
    fn from(summary: String) -> Self {
        Self {
            summary,
            fields: Vec::new(),
        }
    }
}

/// A check's text result, or how else it ended.
pub(super) type Step = Result<String, Outcome>;

/// A check's result with any fields it observed.
pub(super) type Checked = Result<Detail, Outcome>;

pub(super) type CheckFn =
    for<'a> fn(&'a CheckCx<'a>) -> Pin<Box<dyn Future<Output = Checked> + 'a>>;

/// A check's id, as `--check` names it and dependencies refer to it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CheckId(&'static str);

impl CheckId {
    pub(super) const fn new(id: &'static str) -> Self {
        Self(id)
    }

    pub(crate) const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for CheckId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.pad(self.0)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    /// Only reads the BMC.
    Read,
    /// Changes the BMC or host.
    Apply,
}

pub(crate) struct Check {
    pub(crate) id: CheckId,
    pub(crate) capability: Capability,
    pub(crate) kind: Kind,
    pub(super) needs: &'static [Needs],
    /// Checks that must not have failed for this one to run.
    pub(super) after: Vec<CheckId>,
    /// Whether the check may run again when it finds the BMC unreachable;
    /// not for one whose repeat would redo a change it may already have made.
    pub(super) rerun: bool,
    pub(super) timeout: Duration,
    pub(super) run: CheckFn,
}

impl Check {
    pub(super) fn read(id: &'static str, capability: Capability, run: CheckFn) -> Self {
        Self {
            id: CheckId::new(id),
            capability,
            kind: Kind::Read,
            needs: &[],
            after: Vec::new(),
            rerun: true,
            timeout: Duration::from_secs(60),
            run,
        }
    }

    /// Apply checks bound their own waits so they can restore what they
    /// changed; the timeout only backstops a hung request.
    pub(super) fn apply(id: CheckId, capability: Capability, run: CheckFn) -> Self {
        Self {
            kind: Kind::Apply,
            timeout: Duration::from_secs(2 * 60 * 60),
            ..Self::read(id.as_str(), capability, run)
        }
    }

    pub(super) fn timeout(self, timeout: Duration) -> Self {
        Self { timeout, ..self }
    }

    pub(super) fn needs(self, needs: &'static [Needs]) -> Self {
        Self { needs, ..self }
    }

    pub(super) fn after(self, after: &[CheckId]) -> Self {
        Self {
            after: after.to_vec(),
            ..self
        }
    }

    /// Fails the check, rather than running it again, when it finds the BMC
    /// unreachable.
    pub(super) fn once(self) -> Self {
        Self {
            rerun: false,
            ..self
        }
    }
}
