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

use color_eyre::Result;
use rpc::forge as rpc;

use super::args::Args;
use crate::cfg::runtime::RuntimeConfig;
use crate::rpc::ApiClient;

pub(super) async fn list_power_shelves(
    args: Args,
    api_client: &ApiClient,
    config: &RuntimeConfig,
) -> Result<()> {
    let filter = rpc::PowerShelfSearchFilter {
        rack_id: None,
        deleted: args.deleted as i32,
        controller_state: args.controller_state,
        bmc_mac: args.bmc_mac.map(|m| m.to_string()),
    };
    let response = api_client
        .get_all_power_shelves(filter, config.page_size)
        .await?;

    let power_shelves = response.power_shelves;

    if power_shelves.is_empty() {
        println!("No power shelves found.");
        return Ok(());
    }

    println!("Found {} power shelf(ves):", power_shelves.len());

    for (i, shelf) in power_shelves.iter().enumerate() {
        let name = shelf
            .config
            .as_ref()
            .map(|config| config.name.as_str())
            .unwrap_or_else(|| "Unnamed");

        let id = shelf
            .id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "N/A".to_string());

        let power_state = shelf
            .status
            .as_ref()
            .and_then(|status| status.power_state.as_deref())
            .unwrap_or("Unknown");

        let health = shelf
            .status
            .as_ref()
            .and_then(|status| status.health_status.as_deref())
            .unwrap_or("Unknown");

        let controller_state = shelf.controller_state.as_str();

        println!(
            "{}. {} (ID: {}) - Power: {}, Health: {}, State: {}",
            i + 1,
            name,
            id,
            power_state,
            health,
            controller_state
        );
    }

    Ok(())
}
