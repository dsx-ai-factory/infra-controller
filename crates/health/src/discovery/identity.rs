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

use nv_redfish::ServiceRoot;

use crate::HealthError;
use crate::endpoint::{BmcEndpoint, EndpointMetadata};

struct SystemIdentity {
    id: String,
    uuid: Option<uuid::Uuid>,
    bios_version: Option<String>,
}

fn select_primary_system(systems: &[SystemIdentity]) -> Option<&SystemIdentity> {
    systems
        .iter()
        .find(|system| {
            system
                .bios_version
                .as_deref()
                .is_some_and(|version| !version.trim().is_empty())
        })
        .or_else(|| systems.first())
}

/// Resolves the primary ComputerSystem UUID when it is not already known.
///
/// A system with a non-empty BIOS version is preferred because BMCs may expose
/// auxiliary systems alongside the host. When no system has BIOS metadata, the
/// first collection member is used.
pub(super) async fn ensure_primary_system_uuid(endpoint: &BmcEndpoint) -> Result<(), HealthError> {
    let Some(EndpointMetadata::Machine(machine)) = endpoint.metadata.as_ref() else {
        return Ok(());
    };

    machine
        .system_uuid
        .get_or_try_init(|| async {
            let root = ServiceRoot::new(endpoint.bmc().clone()).await?;
            let Some(systems) = root.systems().await? else {
                tracing::warn!(
                    bmc_address = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "BMC does not expose a ComputerSystem collection"
                );
                return Ok(None);
            };
            let systems = systems.members().await?;
            let identities: Vec<SystemIdentity> = systems
                .iter()
                .map(|system| {
                    let raw = system.raw();
                    SystemIdentity {
                        id: raw.id.clone(),
                        uuid: raw.uuid.flatten(),
                        bios_version: raw.bios_version.clone().flatten(),
                    }
                })
                .collect();
            let Some(primary) = select_primary_system(&identities) else {
                tracing::warn!(
                    bmc_address = ?endpoint.addr,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "BMC exposes an empty ComputerSystem collection"
                );
                return Ok(None);
            };
            if primary.uuid.is_none() {
                tracing::warn!(
                    bmc_address = ?endpoint.addr,
                    system_id = %primary.id,
                    rack_id = endpoint.rack_id.as_ref().map(tracing::field::display),
                    "Primary ComputerSystem does not expose a UUID"
                );
            }
            Ok::<Option<uuid::Uuid>, HealthError>(primary.uuid)
        })
        .await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIRST_UUID: uuid::Uuid = uuid::uuid!("11111111-1111-1111-1111-111111111111");
    const BIOS_UUID: uuid::Uuid = uuid::uuid!("22222222-2222-2222-2222-222222222222");

    #[test]
    fn primary_system_prefers_first_system_with_bios() {
        let systems = [
            SystemIdentity {
                id: "auxiliary".to_string(),
                uuid: Some(FIRST_UUID),
                bios_version: None,
            },
            SystemIdentity {
                id: "host".to_string(),
                uuid: Some(BIOS_UUID),
                bios_version: Some("1.2.3".to_string()),
            },
        ];

        let primary = select_primary_system(&systems).expect("primary system");

        assert_eq!(primary.id, "host");
        assert_eq!(primary.uuid, Some(BIOS_UUID));
    }

    #[test]
    fn primary_system_falls_back_to_first_member() {
        let systems = [
            SystemIdentity {
                id: "first".to_string(),
                uuid: Some(FIRST_UUID),
                bios_version: None,
            },
            SystemIdentity {
                id: "second".to_string(),
                uuid: Some(BIOS_UUID),
                bios_version: Some("  ".to_string()),
            },
        ];

        let primary = select_primary_system(&systems).expect("primary system");

        assert_eq!(primary.id, "first");
        assert_eq!(primary.uuid, Some(FIRST_UUID));
    }

    #[test]
    fn primary_system_prefers_host_bios_over_auxiliary_uuid() {
        let systems = [
            SystemIdentity {
                id: "HGX_Baseboard".to_string(),
                uuid: Some(FIRST_UUID),
                bios_version: None,
            },
            SystemIdentity {
                id: "host".to_string(),
                uuid: None,
                bios_version: Some("1.2.3".to_string()),
            },
        ];

        let primary = select_primary_system(&systems).expect("primary system");

        assert_eq!(primary.id, "host");
        assert_eq!(primary.uuid, None);
    }

    #[test]
    fn primary_system_is_absent_for_empty_collection() {
        assert!(select_primary_system(&[]).is_none());
    }
}
