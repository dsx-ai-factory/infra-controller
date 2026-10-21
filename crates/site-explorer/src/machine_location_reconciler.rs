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
use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use carbide_instrument::emit;
use carbide_secrets::credentials::{BmcCredentialType, CredentialKey, CredentialManager};
use carbide_uuid::machine::MachineId;
use component_manager::{MachineInfoProvider, MachineLocationTarget};
use db::Transaction;
use model::rack_type::RackProfileConfig;
use sqlx::PgPool;

use crate::errors::SiteExplorerResult;
use crate::metrics::{
    SiteExplorerMachineSlotTrayFetchFailed, SiteExplorerMachineSlotTrayPersistenceFailed,
    SiteExplorerMachineSlotTrayResponseMissing, SiteExplorerMachineSlotTrayValueInvalid,
};

// Keep RMS location lookups from holding the iteration lock indefinitely.
const RMS_MACHINE_LOCATION_TIMEOUT: Duration = Duration::from_secs(10);

/// Fills missing machine slot and tray data from RMS.
pub struct MachineLocationReconciler {
    database_connection: PgPool,
    rack_profiles: Arc<RackProfileConfig>,
    machine_info_provider: Option<Arc<dyn MachineInfoProvider>>,
    credential_manager: Arc<dyn CredentialManager>,
}

impl MachineLocationReconciler {
    /// Creates a reconciler with an optional machine-information provider.
    pub fn new(
        database_connection: PgPool,
        rack_profiles: Arc<RackProfileConfig>,
        machine_info_provider: Option<Arc<dyn MachineInfoProvider>>,
        credential_manager: Arc<dyn CredentialManager>,
    ) -> Self {
        Self {
            database_connection,
            rack_profiles,
            machine_info_provider,
            credential_manager,
        }
    }

    /// Best-effort fills missing machine location data in one RMS batch.
    ///
    /// Site Explorer calls this after its ingestion and audit phases so RMS
    /// latency cannot serialize machine creation. Machines whose response is
    /// absent or whose request fails remain eligible on the next iteration.
    /// The RPC deadline includes lazy connection setup; expiry cancels this
    /// attempt, emits a failure event, and leaves stored location data unchanged.
    pub async fn reconcile_machine_locations(&self, bmc_ips: &[IpAddr]) -> SiteExplorerResult<()> {
        let Some(machine_info_provider) = &self.machine_info_provider else {
            return Ok(());
        };
        if bmc_ips.is_empty() {
            return Ok(());
        }

        let identities =
            db::machine::find_rms_identities_by_bmc_ips(&self.database_connection, bmc_ips).await?;

        let mut machine_ids_by_node_id = HashMap::new();
        let mut targets = Vec::new();

        for identity in identities {
            if identity.slot_number.is_some() && identity.tray_index.is_some() {
                continue;
            }
            let Some(rack_id) = identity.rack_id else {
                continue;
            };
            let Some(rack_profile_id) = identity.rack_profile_id else {
                tracing::warn!(
                    %rack_id,
                    host_machine_id = %identity.id,
                    "Rack has no rack_profile_id for RMS slot and tray reconciliation"
                );
                continue;
            };
            let Some(rack_profile) = self.rack_profiles.get(rack_profile_id.as_str()) else {
                tracing::warn!(
                    %rack_id,
                    %rack_profile_id,
                    host_machine_id = %identity.id,
                    "Rack profile is not configured for RMS slot and tray reconciliation"
                );
                continue;
            };

            if let Err(error) = machine_info_provider.validate_profile(rack_profile) {
                tracing::warn!(
                    %error,
                    %rack_id,
                    %rack_profile_id,
                    host_machine_id = %identity.id,
                    "Rack profile cannot identify a compute node for RMS slot and tray reconciliation"
                );

                continue;
            }

            let host_machine_id = match identity.id.parse::<MachineId>() {
                Ok(host_machine_id) => host_machine_id,
                Err(error) => {
                    tracing::warn!(
                        %error,
                        host_machine_id = %identity.id,
                        "Invalid machine ID during RMS slot and tray reconciliation"
                    );
                    continue;
                }
            };
            let bmc_credentials = self
                .credential_manager
                .get_credentials(&CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot {
                        bmc_mac_address: identity.bmc_mac_address,
                    },
                })
                .await
                .ok()
                .flatten();

            let node_id = identity.id;

            machine_ids_by_node_id.insert(node_id.clone(), host_machine_id);

            targets.push(MachineLocationTarget {
                node_id,
                rack_id,
                profile: rack_profile,
                bmc_ip: identity.bmc_ip,
                bmc_mac: identity.bmc_mac_address,
                credentials: bmc_credentials,
            });
        }

        if targets.is_empty() {
            return Ok(());
        }

        let response = match tokio::time::timeout(
            RMS_MACHINE_LOCATION_TIMEOUT,
            machine_info_provider.get_machine_locations(targets),
        )
        .await
        {
            Ok(Ok(response)) => response,
            Ok(Err(error)) => {
                emit(SiteExplorerMachineSlotTrayFetchFailed::new(
                    error.to_string(),
                ));
                return Ok(());
            }
            Err(_) => {
                emit(SiteExplorerMachineSlotTrayFetchFailed::new(format!(
                    "RMS slot and tray lookup timed out after {RMS_MACHINE_LOCATION_TIMEOUT:?}"
                )));
                return Ok(());
            }
        };

        for details in response {
            let Some(host_machine_id) = machine_ids_by_node_id.remove(&details.node_id) else {
                tracing::warn!(
                    rms_node_id = %details.node_id,
                    "RMS returned unrequested machine slot and tray data"
                );
                continue;
            };

            let (slot_number, tray_index) =
                machine_location_values(details.slot_number, details.tray_index);

            persist_machine_slot_and_tray(
                &self.database_connection,
                host_machine_id,
                slot_number,
                tray_index,
            )
            .await;
        }

        for _ in machine_ids_by_node_id {
            emit(SiteExplorerMachineSlotTrayResponseMissing::new());
        }

        Ok(())
    }
}

fn machine_location_values(
    slot_number: Option<u32>,
    tray_index: Option<u32>,
) -> (Option<i32>, Option<i32>) {
    let slot_number = crate::rms_location_value(slot_number).unwrap_or_else(|value| {
        emit(SiteExplorerMachineSlotTrayValueInvalid::SlotNumber { value });
        None
    });

    let tray_index = crate::rms_location_value(tray_index).unwrap_or_else(|value| {
        emit(SiteExplorerMachineSlotTrayValueInvalid::TrayIndex { value });
        None
    });

    (slot_number, tray_index)
}

/// Persists best-effort RMS location data without changing machine-creation success.
async fn persist_machine_slot_and_tray(
    pool: &PgPool,
    host_machine_id: MachineId,
    slot_number: Option<i32>,
    tray_index: Option<i32>,
) {
    let mut txn = match Transaction::begin(pool).await {
        Ok(txn) => txn,
        Err(error) => {
            emit(SiteExplorerMachineSlotTrayPersistenceFailed::new(
                error.to_string(),
                host_machine_id.to_string(),
            ));
            return;
        }
    };

    if let Err(error) =
        db::machine::update_slot_and_tray(&mut txn, &host_machine_id, slot_number, tray_index).await
    {
        emit(SiteExplorerMachineSlotTrayPersistenceFailed::new(
            error.to_string(),
            host_machine_id.to_string(),
        ));
        txn.rollback_or_log("site-explorer slot and tray update after operation failure")
            .await;
    } else if let Err(error) = txn.commit().await {
        emit(SiteExplorerMachineSlotTrayPersistenceFailed::new(
            error.to_string(),
            host_machine_id.to_string(),
        ));
    }
}
