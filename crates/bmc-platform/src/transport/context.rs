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

use std::collections::BTreeSet;

use nv_redfish::computer_system::ComputerSystem;
use nv_redfish::core::Action;
use nv_redfish::manager::Manager;
use nv_redfish::{Bmc, Error as RedfishError, ServiceRoot};
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;

use super::{ClassifyBmcError, IpmiOps, manager_index_for, primary_system_index};
use crate::{DriverOutcome, PlatformError, PlatformIdentity, Quirk};

/// Runtime-owned operation context supplied to a stateless driver.
///
/// `nv-redfish` is the only Redfish transport: drivers navigate through the
/// typed `ServiceRoot` wrappers and reach OEM resources through the same
/// authenticated `Bmc`. The ComputerSystem and Manager that exploration
/// selected are resolved on first use, because a factory BMC rejects every
/// request but the first password change until that change is made.
pub struct OpCx<'a, B: Bmc> {
    bmc: &'a B,
    service_root: &'a ServiceRoot<B>,
    identity: &'a PlatformIdentity,
    classify: fn(B::Error) -> PlatformError,
    system: OnceCell<Option<ComputerSystem<B>>>,
    manager: OnceCell<Option<Manager<B>>>,
    ipmi: Option<&'a dyn IpmiOps>,
    quirks: &'a BTreeSet<Quirk>,
}

static NO_QUIRKS: BTreeSet<Quirk> = BTreeSet::new();

impl<'a, B: Bmc> OpCx<'a, B> {
    /// Creates a context for the exploration-selected identity.
    pub fn new(bmc: &'a B, service_root: &'a ServiceRoot<B>, identity: &'a PlatformIdentity) -> Self
    where
        B::Error: ClassifyBmcError,
    {
        Self {
            bmc,
            service_root,
            identity,
            classify: <B::Error as ClassifyBmcError>::classify,
            system: OnceCell::new(),
            manager: OnceCell::new(),
            ipmi: None,
            quirks: &NO_QUIRKS,
        }
    }

    /// A context for the same BMC that holds nothing yet, so the next
    /// [`Self::system`] or [`Self::manager`] reads the BMC again: for a driver
    /// that waits, within one operation, for the BMC to report a change, or
    /// that writes a resource whose ETag the BMC changes on its own.
    pub fn fresh(&self) -> Self {
        Self {
            system: OnceCell::new(),
            manager: OnceCell::new(),
            ..*self
        }
    }

    /// Attaches IPMI operations for drivers that need them.
    pub fn with_ipmi(mut self, ipmi: &'a dyn IpmiOps) -> Self {
        self.ipmi = Some(ipmi);
        self
    }

    /// Attaches the firmware quirks selection found for this BMC.
    pub fn with_quirks(mut self, quirks: &'a BTreeSet<Quirk>) -> Self {
        self.quirks = quirks;
        self
    }

    /// Whether selection found `quirk` for this BMC.
    pub fn has_quirk(&self, quirk: Quirk) -> bool {
        self.quirks.contains(&quirk)
    }

    /// Returns the authenticated transport used to construct `service_root`.
    pub const fn bmc(&self) -> &'a B {
        self.bmc
    }

    /// Returns the service root created from `bmc`.
    pub const fn service_root(&self) -> &'a ServiceRoot<B> {
        self.service_root
    }

    /// Returns the exploration-selected identity.
    pub const fn identity(&self) -> &'a PlatformIdentity {
        self.identity
    }

    /// Returns the ComputerSystem exploration selected, or the primary one
    /// the BMC lists when the identity names none, as for a BMC without an
    /// exploration report. `Unsupported` when the BMC manages no system
    /// (power shelves, switches).
    ///
    /// Fails when the identity names a system the BMC no longer lists.
    pub async fn system(&self) -> Result<&ComputerSystem<B>, PlatformError> {
        self.system
            .get_or_try_init(|| async {
                let selected = self.identity.system.as_ref();
                if selected.is_none() && self.service_root.root.systems.is_none() {
                    return Ok(None);
                }
                let mut systems = self
                    .service_root
                    .systems()
                    .await
                    .map_err(|error| self.map_redfish_error(error))?
                    .ok_or(PlatformError::Unsupported)?
                    .members()
                    .await
                    .map_err(|error| self.map_redfish_error(error))?;
                match selected {
                    Some(system) => find_selected(
                        systems,
                        |member| member.raw().id.clone(),
                        &system.id,
                        "ComputerSystem",
                    )
                    .map(Some),
                    None => {
                        Ok(primary_system_index(&systems).map(|index| systems.swap_remove(index)))
                    }
                }
            })
            .await?
            .as_ref()
            .ok_or(PlatformError::Unsupported)
    }

    /// Returns the Manager exploration selected, or the one linked to
    /// [`Self::system`] when the identity names none.
    ///
    /// Fails when the identity names a manager the BMC no longer lists.
    pub async fn manager(&self) -> Result<&Manager<B>, PlatformError> {
        self.manager
            .get_or_try_init(|| async {
                let mut managers = self
                    .service_root
                    .managers()
                    .await
                    .map_err(|error| self.map_redfish_error(error))?
                    .ok_or(PlatformError::Unsupported)?
                    .members()
                    .await
                    .map_err(|error| self.map_redfish_error(error))?;
                match &self.identity.manager {
                    Some(manager) => find_selected(
                        managers,
                        |member| member.raw().id.clone(),
                        &manager.id,
                        "Manager",
                    )
                    .map(Some),
                    None => {
                        let system = match self.system().await {
                            Ok(system) => Some(system),
                            Err(PlatformError::Unsupported) => None,
                            Err(error) => return Err(error),
                        };
                        Ok(manager_index_for(&managers, system)
                            .map(|index| managers.swap_remove(index)))
                    }
                }
            })
            .await?
            .as_ref()
            .ok_or(PlatformError::Unsupported)
    }

    /// Returns IPMI operations when the runtime attached them.
    pub fn ipmi(&self) -> Option<&'a dyn IpmiOps> {
        self.ipmi
    }

    /// Maps a transport failure into the platform error vocabulary.
    pub fn map_bmc_error(&self, error: B::Error) -> PlatformError {
        (self.classify)(error)
    }

    /// Maps an `nv-redfish` wrapper failure into the platform error vocabulary.
    pub fn map_redfish_error(&self, error: RedfishError<B>) -> PlatformError {
        PlatformError::from_redfish_with(error, self.classify)
    }

    /// Runs an advertised Redfish action with `params`.
    ///
    /// The transport resolves the action target the same way `nv-redfish`
    /// does for its typed actions, so drivers never rebuild standard URIs.
    pub async fn action<T, R>(
        &self,
        action: &Action<T, R>,
        params: &T,
    ) -> Result<DriverOutcome, PlatformError>
    where
        T: Serialize + Send + Sync,
        R: for<'de> Deserialize<'de> + Send + Sync,
    {
        action
            .run(self.bmc, params)
            .await
            .map(DriverOutcome::from)
            .map_err(|error| self.map_bmc_error(error))
    }
}

fn find_selected<R>(
    members: Vec<R>,
    member_id: impl Fn(&R) -> String,
    id: &str,
    kind: &str,
) -> Result<R, PlatformError> {
    if id.is_empty() {
        return Err(PlatformError::InvalidResponse {
            message: format!("platform identity names an empty {kind} id"),
        });
    }
    members
        .into_iter()
        .find(|member| member_id(member) == id)
        .ok_or_else(|| PlatformError::InvalidResponse {
            message: format!("exploration-selected {kind} {id} is no longer available"),
        })
}
