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

use askama::Template;
use carbide_api_core::Api;
use rpc::forge::forge_server::Forge;
use rpc::site_explorer::{ExploredEndpointsByIdsRequest, Inventory};

use super::filters;

#[derive(Default, Template)]
#[template(path = "firmware_inventory.html")]
pub(super) struct FirmwareInventory {
    inventories: Vec<Inventory>,
    load_failed: bool,
}

impl FirmwareInventory {
    pub(super) async fn load(api: &Api, bmc_ip: Option<&str>) -> Self {
        let Some(bmc_ip) = bmc_ip.filter(|ip| !ip.is_empty()) else {
            return Self::default();
        };
        let endpoints = match api
            .find_explored_endpoints_by_ids(tonic::Request::new(ExploredEndpointsByIdsRequest {
                endpoint_ids: vec![bmc_ip.to_string()],
            }))
            .await
        {
            Ok(response) => response.into_inner().endpoints,
            Err(error) => {
                tracing::warn!(%error, %bmc_ip, "Failed to load firmware inventory");
                return Self {
                    load_failed: true,
                    ..Self::default()
                };
            }
        };
        let mut inventories: Vec<_> = endpoints
            .into_iter()
            .filter_map(|endpoint| endpoint.report)
            .flat_map(|report| report.service)
            .filter(|service| service.id == "FirmwareInventory")
            .flat_map(|service| service.inventories)
            .collect();
        inventories.sort_by(|left, right| left.id.cmp(&right.id));
        Self {
            inventories,
            load_failed: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use askama::Template;

    use super::FirmwareInventory;
    use crate::tests::env::TestEnv;

    #[crate::sqlx_test]
    async fn firmware_inventory_unavailable_states(pool: sqlx::PgPool) {
        let env = TestEnv::new(pool).await;
        let mut txn = env.api().database_connection.begin().await.unwrap();
        db::explored_endpoints::insert(
            "192.0.2.10".parse().unwrap(),
            &Default::default(),
            false,
            &mut txn,
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();

        for (scenario, bmc_ip) in [
            ("no BMC address", None),
            ("no exploration report", Some("192.0.2.11")),
            ("no firmware service", Some("192.0.2.10")),
        ] {
            let html = FirmwareInventory::load(env.api(), bmc_ip)
                .await
                .render()
                .unwrap();
            assert!(
                html.contains("No firmware inventory collected for this BMC."),
                "{scenario}"
            );
            assert!(!html.contains("<table"), "{scenario}");
            assert!(
                html.contains("Firmware Inventory · 0 components</summary>"),
                "{scenario}"
            );
        }

        sqlx::query(
            "ALTER TABLE explored_endpoints RENAME COLUMN exploration_report TO unavailable_report",
        )
        .execute(&env.api().database_connection)
        .await
        .unwrap();
        let html = FirmwareInventory::load(env.api(), Some("192.0.2.10"))
            .await
            .render()
            .unwrap();
        assert!(html.contains("Unable to load firmware inventory."));
        assert!(html.contains("Firmware Inventory · unavailable</summary>"));
        assert!(!html.contains("No firmware inventory collected"));
    }
}
