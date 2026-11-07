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

use serde::{Deserialize, Serialize};

/// The most recent tenant related status
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstanceTenantStatus {
    /// The current state of the instance from the point of view of the assigned tenant
    pub state: TenantState,
    /// An optional message which can contain details about the state
    pub state_details: String,
}

/// Enumerates possible instance states from the view of a tenant
/// This is only a subset of total states that the instance might be in, and
/// excludes states that are used while the instance is not being allocated to
/// a tenant.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TenantState {
    /// The instance is currently getting provisioned for a tenant
    Provisioning,
    /// DPU is being reprovisioned.
    DpuReprovisioning,
    /// Host is being reprovisioned.
    HostReprovisioning,
    /// Firmware or other updates are being peformed, of which
    /// the tenant should not have to be concerned with the
    /// specific details.
    Updating,
    /// The instance is ready and can be used by the tenant
    Ready,
    /// The instance has been ready, but the newest configuration that the tenant
    /// desired has not been applied yet
    Configuring,
    /// The instance is shutting down. Shutdown has not completed yet
    Terminating,
    /// The instance has fully shut down, and is no longer available for the user
    Terminated,
    /// The instance is in a terminal failed state. This state is equivalent to
    /// DEACTIVATED - no user software is running anymore during the state. However
    /// an instance might enter a FAILED state before even fully activating, in case
    /// activation failed.
    Failed,
    /// Not sure what happened. Check log for more info
    Invalid,
    /// Instance is undergoing online repair while otherwise tenant-ready. Set by
    /// `instance_status_tenant_state` in the RPC model layer when a repair health merge
    /// is active and the instance would otherwise be [`Ready`].
    Repairing,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_tenant_status() {
        let status = InstanceTenantStatus {
            state: TenantState::Configuring,
            state_details: "Details".to_string(),
        };
        let serialized = serde_json::to_string(&status).unwrap();
        assert_eq!(
            serialized,
            "{\"state\":\"configuring\",\"state_details\":\"Details\"}"
        );
        assert_eq!(
            serde_json::from_str::<InstanceTenantStatus>(&serialized).unwrap(),
            status
        );
    }
}
