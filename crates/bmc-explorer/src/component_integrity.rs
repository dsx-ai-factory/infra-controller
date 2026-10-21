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

use model::site_explorer::ComponentIntegrityEntry;
use nv_redfish::component_integrity::{ComponentIntegrity, ComponentIntegrityType};
use nv_redfish::{Bmc, ServiceRoot};

/// Collected resources or the failure that prevented a complete inventory.
pub(crate) struct ExploredComponentIntegrity<B: Bmc> {
    members: Result<Option<Vec<ComponentIntegrity<B>>>, nv_redfish::Error<B>>,
}

/// Normalized inventory and whether an advertised collection was unreadable.
#[derive(Default)]
pub(crate) struct Observation {
    pub(crate) entries: Option<Vec<ComponentIntegrityEntry>>,
    pub(crate) unavailable: bool,
}

impl<B: Bmc> ExploredComponentIntegrity<B> {
    /// Collect every member without converting resources to the report model.
    /// Collection failures must not fail otherwise successful exploration.
    pub(crate) async fn explore(root: &ServiceRoot<B>) -> Self {
        let members = match root.component_integrity().await {
            Ok(Some(collection)) => collection.members().await.map(Some),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        if let Err(error) = &members {
            tracing::warn!(%error, "Failed to fetch the ComponentIntegrity collection.");
        }
        Self { members }
    }

    /// Convert collected resources without BMC requests. An incomplete member
    /// makes the whole inventory unavailable rather than reporting a partial set.
    pub(crate) fn to_model(&self) -> Observation {
        match &self.members {
            Ok(Some(members)) => {
                let entries: Option<Vec<_>> = members.iter().map(|member| {
                    let raw = member.raw();
                    let integrity_type = match raw.component_integrity_type {
                        ComponentIntegrityType::Spdm => Some("SPDM"),
                        ComponentIntegrityType::Tpm => Some("TPM"),
                        ComponentIntegrityType::Tcm => Some("TCM"),
                        ComponentIntegrityType::Tpcm => Some("TPCM"),
                        ComponentIntegrityType::Oem => Some("OEM"),
                        ComponentIntegrityType::UnsupportedValue => None,
                    };
                    match (integrity_type, raw.component_integrity_enabled) {
                        (Some(integrity_type), Some(enabled)) => Some(ComponentIntegrityEntry {
                            id: raw.id.clone(),
                            component_integrity_type: integrity_type.into(),
                            component_integrity_enabled: enabled,
                        }),
                        _ => {
                            tracing::warn!(component_id = %raw.id,
                                "Incomplete or unsupported ComponentIntegrity data; inventory unavailable.");
                            None
                        }
                    }
                }).collect();
                Observation {
                    unavailable: entries.is_none(),
                    entries,
                }
            }
            Ok(None) => Observation::default(),
            Err(_) => Observation {
                entries: None,
                unavailable: true,
            },
        }
    }
}
