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

use bmc_drivers::CatalogError;
use bmc_platform::{
    DriverOutcome, Lockdown, LockdownDesiredState, LockdownScope, LockdownState, LockdownStatus,
    PlatformError,
};
use bmc_runtime::BoundDriver;
use carbide_redfish::nv_redfish::RedfishBmc;
use libredfish::{EnabledDisabled, Redfish};

use crate::BackendBmc;
use crate::convert::platform_error;

impl BackendBmc {
    /// See [`bmc_runtime::ConnectedBmc::lockdown`].
    pub fn lockdown(&self) -> Result<BackendLockdown<'_>, CatalogError> {
        match self {
            Self::Libredfish(client) => Ok(BackendLockdown::Libredfish(client.as_ref())),
            Self::Drivers(bmc) => bmc.lockdown().map(BackendLockdown::Drivers),
        }
    }

    /// Reads the host's and BMC's lockdown, `None` on a platform without
    /// lockdown.
    pub async fn lockdown_status(&self) -> Result<Option<LockdownStatus>, PlatformError> {
        supported(async { self.lockdown()?.status().await }.await)
    }

    /// Moves `scope` of the platform's lockdown to `desired`, returning whether
    /// the platform has that control; without it nothing changes. Work the
    /// change starts is not waited on.
    pub async fn set_lockdown(
        &self,
        scope: LockdownScope,
        desired: LockdownDesiredState,
    ) -> Result<bool, PlatformError> {
        supported(async { self.lockdown()?.set(scope, desired).await }.await)
            .map(|outcome| outcome.is_some())
    }
}

/// `result` as `None` when the operation is unsupported.
fn supported<T>(result: Result<T, PlatformError>) -> Result<Option<T>, PlatformError> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(PlatformError::Unsupported) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The lockdown operations of a [`BackendBmc`].
pub enum BackendLockdown<'a> {
    Libredfish(&'a dyn Redfish),
    Drivers(BoundDriver<'a, dyn Lockdown<RedfishBmc>, RedfishBmc>),
}

impl BackendLockdown<'_> {
    /// See [`Lockdown::status`]. libredfish reports only the aggregate.
    pub async fn status(&self) -> Result<LockdownStatus, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let status = client.lockdown_status().await.map_err(platform_error)?;
                Ok(LockdownStatus {
                    aggregate: aggregate(status.is_fully_enabled(), status.is_fully_disabled()),
                    message: status.message().to_string(),
                    host: LockdownState::Unknown,
                    bmc: LockdownState::Unknown,
                })
            }
            Self::Drivers(lockdown) => lockdown.status().await,
        }
    }

    /// See [`Lockdown::set`]. libredfish's `lockdown` is the full lockdown,
    /// `lockdown_bmc` the BMC's, and `set_idrac_lockdown` Dell's system
    /// lockdown switch; it has no host-only lockdown.
    pub async fn set(
        &self,
        scope: LockdownScope,
        desired: LockdownDesiredState,
    ) -> Result<DriverOutcome, PlatformError> {
        match self {
            Self::Libredfish(client) => {
                let target = match desired {
                    LockdownDesiredState::Enabled => EnabledDisabled::Enabled,
                    LockdownDesiredState::Disabled => EnabledDisabled::Disabled,
                };
                match scope {
                    LockdownScope::All => client.lockdown(target).await,
                    LockdownScope::Bmc => client.lockdown_bmc(target).await,
                    LockdownScope::BmcSystemLockdown => client.set_idrac_lockdown(target).await,
                    LockdownScope::Host => return Err(PlatformError::Unsupported),
                }
                .map_err(platform_error)?;
                Ok(DriverOutcome::complete())
            }
            Self::Drivers(lockdown) => lockdown.set(scope, desired).await,
        }
    }
}

/// The aggregate lockdown of a libredfish status that is fully enabled,
/// fully disabled, or neither.
fn aggregate(fully_enabled: bool, fully_disabled: bool) -> LockdownState {
    match (fully_enabled, fully_disabled) {
        (true, _) => LockdownState::Enabled,
        (false, true) => LockdownState::Disabled,
        (false, false) => LockdownState::Partial,
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn libredfish_status_maps_to_its_aggregate() {
        value_scenarios!(run = |(enabled, disabled)| aggregate(enabled, disabled);
            "fully applied or lifted" {
                (true, false) => LockdownState::Enabled,
                (false, true) => LockdownState::Disabled,
            }
            "neither" {
                (false, false) => LockdownState::Partial,
            }
        );
    }
}
