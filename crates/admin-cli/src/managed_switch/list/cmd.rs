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

use std::borrow::Cow;

use color_eyre::Result;

use crate::rpc::ApiClient;

pub(super) async fn list_managed_switches(api_client: &ApiClient) -> Result<()> {
    let linked = api_client
        .0
        .get_all_expected_switches_linked()
        .await?
        .expected_switches;

    if linked.is_empty() {
        println!("No managed switches found.");
        return Ok(());
    }

    println!("Found {} managed switch(es):", linked.len());
    println!(
        "{:<36} {:<20} {:<18} {:<18} {:<36}",
        "Switch ID", "Serial Number", "BMC MAC", "Explored Endpoint", "Rack ID"
    );
    println!("{:-<140}", "");

    for (i, entry) in linked.iter().enumerate() {
        let switch_id = entry
            .switch_id
            .as_ref()
            .map(|id| Cow::Owned(id.to_string()))
            .unwrap_or(Cow::Borrowed("NotCreated"));

        let serial = if entry.switch_serial_number.is_empty() {
            "N/A"
        } else {
            &entry.switch_serial_number
        };

        let bmc_mac = if entry.bmc_mac_address.is_empty() {
            "N/A"
        } else {
            &entry.bmc_mac_address
        };

        let endpoint = entry.explored_endpoint_address.as_deref().unwrap_or("N/A");

        let rack_id = entry
            .rack_id
            .as_ref()
            .map(|id| Cow::Owned(id.to_string()))
            .unwrap_or(Cow::Borrowed("N/A"));

        println!(
            "{}. {:<36} {:<20} {:<18} {:<18} {:<36}",
            i + 1,
            switch_id,
            serial,
            bmc_mac,
            endpoint,
            rack_id,
        );
    }

    Ok(())
}
