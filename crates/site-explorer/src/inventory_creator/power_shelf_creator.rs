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

use std::collections::HashSet;

use carbide_uuid::power_shelf::{PowerShelfIdSource, PowerShelfType};
use db::{DatabaseError, ObjectColumnFilter, power_shelf as db_power_shelf};
use mac_address::MacAddress;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::power_shelf::{NewPowerShelf, PowerShelfConfig};
use model::site_explorer::{EndpointExplorationReport, EndpointType, ExploredEndpoint};
use sqlx::PgPool;

use super::Config;
use crate::errors::{SiteExplorerError, SiteExplorerResult};
use crate::explored_endpoint_index::ExploredEndpointIndex;
use crate::metrics::SiteExplorationMetrics;

/// Creates power shelves from site-explorer endpoint reports.
pub struct PowerShelfCreator {
    database_connection: PgPool,
    config: Config,
}

impl PowerShelfCreator {
    pub(super) async fn identify_power_shelves_to_ingest(
        &self,
        expected_endpoint_index: &ExploredEndpointIndex,
        suppressed_bmc_macs: &HashSet<MacAddress>,
    ) -> SiteExplorerResult<Vec<(ExploredEndpoint, EndpointExplorationReport)>> {
        let mut txn = self
            .database_connection
            .begin()
            .await
            .map_err(|e| DatabaseError::new("load find_all_preingestion_complete data", e))?;

        let explored_endpoints =
            db::explored_endpoints::find_all_preingestion_complete(&mut txn).await?;

        txn.commit()
            .await
            .map_err(|e| DatabaseError::new("end find_all_preingestion_complete data", e))?;

        let mut explored_power_shelves = Vec::new();
        for ep in explored_endpoints.into_iter() {
            if let Some(expected) =
                expected_endpoint_index.matched_expected_power_shelf(&ep.address)
                && suppressed_bmc_macs.contains(&expected.bmc_mac_address)
            {
                tracing::info!(
                    bmc_ip_address = %ep.address,
                    bmc_mac_address = %expected.bmc_mac_address,
                    "Skipping ingestion of suppressed power shelf",
                );
                continue;
            }

            if ep.report.endpoint_type != EndpointType::Bmc {
                continue;
            }
            if ep.report.is_power_shelf() {
                explored_power_shelves.push((ep.clone(), ep.report.clone()));
            }
            //ignore other endpoints
        }

        Ok(explored_power_shelves)
    }

    /// Creates a power-shelf creator with the supplied database and configuration.
    pub fn new(database_connection: PgPool, config: Config) -> Self {
        Self {
            database_connection,
            config,
        }
    }

    pub(super) async fn create_power_shelves(
        &self,
        metrics: &mut SiteExplorationMetrics,
        explored_power_shelves: Vec<(ExploredEndpoint, EndpointExplorationReport)>,
        expected_endpoint_index: &ExploredEndpointIndex,
    ) -> SiteExplorerResult<()> {
        // One query replaces a per-shelf transaction for shelves that already
        // exist; `create_power_shelf` keeps its own checks for the rest.
        let mut existing_bmc_macs = HashSet::new();
        let mut existing_names = HashSet::new();
        for (bmc_mac_address, name) in
            db_power_shelf::find_all_bmc_mac_addresses_and_names(&self.database_connection).await?
        {
            if let Some(bmc_mac_address) = bmc_mac_address {
                existing_bmc_macs.insert(bmc_mac_address);
            }
            existing_names.insert(name);
        }

        let mut skipped_existing_power_shelves = 0usize;
        for (endpoint, _report) in explored_power_shelves {
            let address = endpoint.address;
            let Some(expected_power_shelf) =
                expected_endpoint_index.matched_expected_power_shelf(&endpoint.address)
            else {
                tracing::info!(
                    bmc_ip_address = ?endpoint.address,
                    "No expected power shelf found"
                );
                continue;
            };

            if power_shelf_already_exists(expected_power_shelf, &existing_bmc_macs, &existing_names)
            {
                skipped_existing_power_shelves += 1;
                continue;
            }

            match self
                .create_power_shelf(endpoint, expected_power_shelf, &self.database_connection)
                .await
            {
                Ok(true) => {
                    metrics.created_power_shelves_count += 1;
                    if metrics.created_power_shelves_count as u64
                        == self.config.power_shelves_created_per_run
                    {
                        break;
                    }
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::error!(
                        %error,
                        bmc_ip_address = ?address,
                        "Failed to create power shelf"
                    )
                }
            }
        }

        if skipped_existing_power_shelves > 0 {
            tracing::info!(
                skipped_existing_power_shelves,
                "Skipped power shelves that already exist"
            );
        }

        Ok(())
    }

    /// Creates a power shelf and associates its BMC interface and expected rack.
    /// Returns `false` if its BMC MAC or non-empty name already belongs to a power shelf.
    pub async fn create_power_shelf(
        &self,
        explored_endpoint: ExploredEndpoint,
        expected_shelf: &ExpectedPowerShelf,
        pool: &PgPool,
    ) -> SiteExplorerResult<bool> {
        let mut txn = pool
            .begin()
            .await
            .map_err(|e| DatabaseError::new("begin load create_power_shelf", e))?;

        // Defense against the duplicate-power-shelves bug: if a power shelf
        // already exists in the database for this BMC MAC, don't make another
        // one. This mirrors the dedup check on the switch creation path and
        // catches the case where the input we hash to mint the
        // `PowerShelfId` drifts between exploration cycles.
        if let Some(existing) =
            db_power_shelf::find_by_bmc_mac_address(&mut txn, expected_shelf.bmc_mac_address)
                .await?
        {
            tracing::warn!(
                bmc_mac_address = %expected_shelf.bmc_mac_address,
                existing_power_shelf_id = %existing.id,
                bmc_ip_address = %explored_endpoint.address,
                "Power shelf already exists; skipping discovery",
            );
            txn.rollback()
                .await
                .map_err(|e| DatabaseError::new("rollback create_power_shelf", e))?;
            return Ok(false);
        }

        // Check if a power shelf with the same name already exists
        if !expected_shelf.metadata.name.is_empty() {
            let existing_power_shelves = db_power_shelf::find_by(
                &mut txn,
                ObjectColumnFilter::All::<db::power_shelf::NameColumn>,
            )
            .await?;

            // Check if any existing power shelf has the same name
            for existing_ps in &existing_power_shelves {
                if existing_ps.config.name == expected_shelf.metadata.name {
                    tracing::info!(
                        power_shelf_name = %expected_shelf.metadata.name,
                        bmc_ip_address = %explored_endpoint.address,
                        "Power shelf already exists; skipping creation"
                    );
                    txn.rollback()
                        .await
                        .map_err(|e| DatabaseError::new("rollback create_power_shelf", e))?;
                    return Ok(false);
                }
            }
        }

        tracing::info!(
            bmc_ip_address = %explored_endpoint.address,
            "Creating power shelf"
        );

        // Create a new power shelf
        // Generate power_shelf_id similar to machine_id using deterministic hashing.
        // Extract serial / vendor / model from the chassis reported by the
        // explored endpoint. Prefer a chassis whose id identifies it as a
        // power shelf, falling back to the first chassis if none match.
        // Fall back to sensible defaults if the chassis is missing fields so
        // that we can still mint a stable id during exploration.
        let chassis_list = &explored_endpoint.report.chassis;
        let power_shelf_chassis = chassis_list
            .iter()
            .find(|c| c.id.to_lowercase().contains("powershelf"))
            .or_else(|| chassis_list.first());

        if power_shelf_chassis.is_none() {
            tracing::warn!(
                bmc_ip_address = %explored_endpoint.address,
                "No chassis reported for power shelf endpoint; falling back to defaults for id generation",
            );
        }

        let power_shelf_serial = power_shelf_chassis
            .and_then(|c| c.serial_number.as_deref())
            .unwrap_or(expected_shelf.metadata.name.as_str());
        let power_shelf_vendor = power_shelf_chassis
            .and_then(|c| c.manufacturer.as_deref())
            .unwrap_or("NVIDIA");
        let power_shelf_model = power_shelf_chassis
            .and_then(|c| c.model.as_deref())
            .unwrap_or("PowerShelf");
        let power_shelf_id = match model::power_shelf::power_shelf_id::from_hardware_info(
            power_shelf_serial,
            power_shelf_vendor,
            power_shelf_model,
            PowerShelfIdSource::ProductBoardChassisSerial,
            PowerShelfType::Rack,
        ) {
            Ok(id) => id,
            Err(e) => {
                tracing::error!(error = %e, "Failed to create power shelf ID");
                return Err(SiteExplorerError::InvalidArgument(format!(
                    "Failed to create power shelf ID: {e}"
                )));
            }
        };

        let config = PowerShelfConfig {
            name: expected_shelf.metadata.name.clone(),
            capacity: Some(100),
            voltage: Some(240),
        };

        let new_power_shelf = NewPowerShelf {
            id: power_shelf_id,
            config,
            bmc_mac_address: Some(expected_shelf.bmc_mac_address),
            metadata: Some(expected_shelf.metadata.clone()),
            rack_id: expected_shelf.rack_id.clone(),
        };

        db_power_shelf::create(&mut txn, &new_power_shelf).await?;

        let mi =
            db::machine_interface::find_by_mac_address(&mut *txn, expected_shelf.bmc_mac_address)
                .await?;
        if let Some(interface) = mi.first() {
            // A power shelf's BMC/PMC interface is its management endpoint, so
            // associate it with the shelf and annotate it as `Bmc` (like
            // host/switch BMC interfaces), demoting it from primary in one
            // statement. This `Bmc` link is what lets the power-shelf load query
            // resolve the shelf's MAC/IP into the `PowerShelf.bmc_info` field.
            db::machine_interface::associate_bmc_interface(
                &interface.id,
                model::machine_interface_address::MachineInterfaceAssociation::PowerShelf(
                    power_shelf_id,
                ),
                &mut txn,
            )
            .await?;
        }

        if let Some(ref rack_id) = expected_shelf.rack_id {
            let _ = crate::ensure_rack_exists(txn.as_mut(), rack_id).await?;
        }
        // No need to update the power shelf name again; it was already set in config above.
        txn.commit()
            .await
            .map_err(|e| DatabaseError::new("end create_power_shelf", e))?;

        tracing::info!(
            %power_shelf_id,
            bmc_ip_address = %explored_endpoint.address,
            "Created power shelf"
        );

        Ok(true)
    }
}

/// Whether `create_power_shelf` would refuse this shelf: its BMC MAC or its
/// non-empty name matches an existing power shelf record.
fn power_shelf_already_exists(
    expected_power_shelf: &ExpectedPowerShelf,
    existing_bmc_macs: &HashSet<MacAddress>,
    existing_names: &HashSet<String>,
) -> bool {
    let name = &expected_power_shelf.metadata.name;
    existing_bmc_macs.contains(&expected_power_shelf.bmc_mac_address)
        || (!name.is_empty() && existing_names.contains(name))
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn power_shelf_already_exists_cases() {
        let existing_mac: MacAddress = "02:00:00:00:00:01".parse().unwrap();
        let new_mac: MacAddress = "02:00:00:00:00:02".parse().unwrap();
        let existing_bmc_macs = HashSet::from([existing_mac]);
        let existing_names = HashSet::from(["shelf-1".to_string(), String::new()]);

        check_values(
            [
                Check {
                    scenario: "BMC MAC matches an existing shelf: exists",
                    input: (existing_mac, "shelf-2"),
                    expect: true,
                },
                Check {
                    scenario: "name matches an existing shelf: exists",
                    input: (new_mac, "shelf-1"),
                    expect: true,
                },
                Check {
                    scenario: "empty name does not match an existing empty name: new",
                    input: (new_mac, ""),
                    expect: false,
                },
                Check {
                    scenario: "neither matches: new",
                    input: (new_mac, "shelf-2"),
                    expect: false,
                },
            ],
            |(bmc_mac_address, name): (MacAddress, &str)| {
                let expected_power_shelf = ExpectedPowerShelf {
                    bmc_mac_address,
                    metadata: model::metadata::Metadata {
                        name: name.to_string(),
                        ..Default::default()
                    },
                    ..Default::default()
                };
                power_shelf_already_exists(
                    &expected_power_shelf,
                    &existing_bmc_macs,
                    &existing_names,
                )
            },
        );
    }
}
