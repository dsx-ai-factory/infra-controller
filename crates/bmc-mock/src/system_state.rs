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

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use crate::{
    BootConfigPatch, BootState, Callbacks, MockPowerState, SetSystemPowerError, SystemPowerControl,
    VirtualMediaState,
};

/// Failure to read or update backend-owned system state.
#[derive(Debug, thiserror::Error)]
pub enum CallbackError {
    /// The requested operation is unsupported by the backend.
    #[error("invalid callback request: {0}")]
    BadRequest(String),
    /// The system with the given ID has not been initialized.
    #[error("system {0} is not initialized")]
    SystemNotInitialized(String),
    /// The virtual media device with the given ID is not configured.
    #[error("virtual media device {0} is not configured")]
    VirtualMediaNotConfigured(String),
    /// An internal backend failure, including actor communication errors.
    #[error(transparent)]
    InternalError(#[from] eyre::Error),
}

/// Typed system data owned by a backend and returned as a consistent snapshot.
#[derive(Clone, Debug)]
pub struct SystemStateData {
    /// Boot configuration and the profile's available boot options.
    pub boot: BootState,
    /// Media contents keyed by configured Redfish device ID.
    pub virtual_media: BTreeMap<String, VirtualMediaState>,
}

/// Reusable boot/media storage for in-memory callback implementations.
/// Clones share the same storage; each BMC should construct its own instance.
/// Used directly as callbacks, it reports power as on and ignores power commands
/// and refresh indications.
#[derive(Clone, Debug, Default)]
pub struct InMemorySystemState(Arc<Mutex<BTreeMap<String, SystemStateData>>>);

impl InMemorySystemState {
    /// Initializes a system when constructing or rebuilding its BMC.
    pub fn initialize_system(&self, system_id: &str, initial: SystemStateData) {
        self.0
            .lock()
            .expect("system state lock poisoned")
            .insert(system_id.to_string(), initial);
    }

    /// Returns a consistent snapshot, failing when the system was not initialized.
    pub fn get_system_state(&self, system_id: &str) -> Result<SystemStateData, CallbackError> {
        self.0
            .lock()
            .expect("system state lock poisoned")
            .get(system_id)
            .cloned()
            .ok_or_else(|| CallbackError::SystemNotInitialized(system_id.to_string()))
    }

    /// Applies a boot patch, failing when the system was not initialized.
    pub fn set_boot_config(
        &self,
        system_id: &str,
        patch: BootConfigPatch,
    ) -> Result<(), CallbackError> {
        self.0
            .lock()
            .expect("system state lock poisoned")
            .get_mut(system_id)
            .ok_or_else(|| CallbackError::SystemNotInitialized(system_id.to_string()))?
            .boot
            .apply(patch);
        Ok(())
    }

    /// Replaces media contents, failing when the system or device is not configured.
    pub fn set_virtual_media(
        &self,
        system_id: &str,
        desired: VirtualMediaState,
    ) -> Result<(), CallbackError> {
        self.0
            .lock()
            .expect("system state lock poisoned")
            .get_mut(system_id)
            .ok_or_else(|| CallbackError::SystemNotInitialized(system_id.to_string()))?
            .set_media(desired)
    }

    /// Consumes one-time overrides for this BMC when its owner completes boot.
    pub fn on_boot_completed(&self) {
        for state in self
            .0
            .lock()
            .expect("system state lock poisoned")
            .values_mut()
        {
            state.boot.on_boot_completed();
        }
    }
}

impl Callbacks for InMemorySystemState {
    fn initialize_system(&self, system_id: &str, initial: SystemStateData) {
        Self::initialize_system(self, system_id, initial);
    }

    async fn get_system_state(&self, system_id: &str) -> Result<SystemStateData, CallbackError> {
        Self::get_system_state(self, system_id)
    }

    async fn set_boot_config(
        &self,
        system_id: &str,
        patch: BootConfigPatch,
    ) -> Result<(), CallbackError> {
        Self::set_boot_config(self, system_id, patch)
    }

    async fn set_virtual_media(
        &self,
        system_id: &str,
        desired: VirtualMediaState,
    ) -> Result<(), CallbackError> {
        Self::set_virtual_media(self, system_id, desired)
    }

    fn get_power_state(&self) -> MockPowerState {
        MockPowerState::On
    }

    fn send_power_command(&self, _: SystemPowerControl) -> Result<(), SetSystemPowerError> {
        Ok(())
    }

    fn state_refresh_indication(&self) {}
}

impl SystemStateData {
    pub(crate) fn set_media(&mut self, desired: VirtualMediaState) -> Result<(), CallbackError> {
        let media = self
            .virtual_media
            .get_mut(&desired.device_id)
            .ok_or_else(|| CallbackError::VirtualMediaNotConfigured(desired.device_id.clone()))?;
        *media = desired;
        Ok(())
    }
}
