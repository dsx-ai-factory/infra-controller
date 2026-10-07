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
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::GetRackProfileResponse;
use serde::Serialize;

use super::args::Args;
use crate::cfg::runtime::RuntimeConfig;
use crate::rpc::ApiClient;

#[derive(Serialize)]
struct ProfileOutput {
    rack_id: String,
    rack_profile_id: String,
    product_family: String,
    rack_hardware_type: String,
    rack_hardware_topology: String,
    rack_hardware_class: String,
    compute_name: String,
    compute_count: u32,
    compute_vendor: String,
    compute_slot_ids: Vec<u32>,
    switch_name: String,
    switch_count: u32,
    switch_vendor: String,
    switch_slot_ids: Vec<u32>,
    power_shelf_name: String,
    power_shelf_count: u32,
    power_shelf_vendor: String,
    power_shelf_slot_ids: Vec<u32>,
}

impl From<&GetRackProfileResponse> for ProfileOutput {
    fn from(r: &GetRackProfileResponse) -> Self {
        let profile = r.profile.as_ref();
        let capabilities = profile.and_then(|p| p.capabilities.as_ref());
        let compute = capabilities.and_then(|c| c.compute.as_ref());
        let switch = capabilities.and_then(|c| c.switch.as_ref());
        let power_shelf = capabilities.and_then(|c| c.power_shelf.as_ref());

        Self {
            rack_id: r
                .rack_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default(),
            rack_profile_id: r
                .rack_profile_id
                .as_ref()
                .map(|id| id.to_string())
                .unwrap_or_default(),
            product_family: profile
                .map(|p| product_family_display(p.product_family))
                .unwrap_or_else(|| "N/A".to_string()),
            rack_hardware_type: profile
                .and_then(|p| p.rack_hardware_type.as_ref())
                .map(|t| t.value.clone())
                .unwrap_or_else(|| "N/A".to_string()),
            rack_hardware_topology: profile
                .map(|p| {
                    rpc::forge::RackHardwareTopology::try_from(p.rack_hardware_topology)
                        .unwrap_or_default()
                        .as_str_name()
                        .to_string()
                })
                .unwrap_or_else(|| "N/A".to_string()),
            rack_hardware_class: profile
                .map(|p| {
                    rpc::forge::RackHardwareClass::try_from(p.rack_hardware_class)
                        .unwrap_or_default()
                        .as_str_name()
                        .to_string()
                })
                .unwrap_or_else(|| "N/A".to_string()),
            compute_name: compute
                .and_then(|c| c.name.clone())
                .unwrap_or_else(|| "N/A".to_string()),
            compute_count: compute.map(|c| c.count).unwrap_or(0),
            compute_vendor: compute
                .and_then(|c| c.vendor.clone())
                .unwrap_or_else(|| "N/A".to_string()),
            compute_slot_ids: compute.map(|c| c.slot_ids.clone()).unwrap_or_default(),
            switch_name: switch
                .and_then(|s| s.name.clone())
                .unwrap_or_else(|| "N/A".to_string()),
            switch_count: switch.map(|s| s.count).unwrap_or(0),
            switch_vendor: switch
                .and_then(|s| s.vendor.clone())
                .unwrap_or_else(|| "N/A".to_string()),
            switch_slot_ids: switch.map(|s| s.slot_ids.clone()).unwrap_or_default(),
            power_shelf_name: power_shelf
                .and_then(|p| p.name.clone())
                .unwrap_or_else(|| "N/A".to_string()),
            power_shelf_count: power_shelf.map(|p| p.count).unwrap_or(0),
            power_shelf_vendor: power_shelf
                .and_then(|p| p.vendor.clone())
                .unwrap_or_else(|| "N/A".to_string()),
            power_shelf_slot_ids: power_shelf.map(|p| p.slot_ids.clone()).unwrap_or_default(),
        }
    }
}

pub(in crate::rack::profile) async fn show_profile(
    api_client: &ApiClient,
    args: Args,
    config: &RuntimeConfig,
) -> Result<()> {
    let response = api_client.get_rack_profile(args.rack_id).await?;

    let output = ProfileOutput::from(&response);
    match config.format {
        OutputFormat::Json => println!("{}", serde_json::to_string_pretty(&output)?),
        OutputFormat::Yaml => println!("{}", serde_yaml::to_string(&output)?),
        _ => show_detail(&response),
    }

    Ok(())
}

fn slot_ids_display(ids: &[u32]) -> String {
    if ids.is_empty() {
        "N/A".to_string()
    } else {
        ids.iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn product_family_display(value: i32) -> String {
    match rpc::forge::RackProductFamily::try_from(value) {
        Ok(
            family @ (rpc::forge::RackProductFamily::Gb200 | rpc::forge::RackProductFamily::Gb300),
        ) => family.as_str_name().to_string(),
        Ok(rpc::forge::RackProductFamily::Unspecified) | Err(_) => "N/A".to_string(),
    }
}

fn show_detail(r: &GetRackProfileResponse) {
    let profile = r.profile.as_ref();
    let capabilities = profile.and_then(|p| p.capabilities.as_ref());
    let compute = capabilities.and_then(|c| c.compute.as_ref());
    let switch = capabilities.and_then(|c| c.switch.as_ref());
    let power_shelf = capabilities.and_then(|c| c.power_shelf.as_ref());

    let mut table = Table::new();
    table.add_row(row![
        "Rack ID",
        r.rack_id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default()
    ]);
    table.add_row(row![
        "Rack Profile",
        r.rack_profile_id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default()
    ]);
    table.add_row(row![
        "Hardware Type",
        profile
            .and_then(|p| p.rack_hardware_type.as_ref())
            .map(|t| t.value.as_str())
            .unwrap_or("N/A")
    ]);
    table.add_row(row![
        "Product Family",
        profile
            .map(|p| product_family_display(p.product_family))
            .unwrap_or_else(|| "N/A".to_string())
    ]);
    table.add_row(row![
        "Hardware Topology",
        profile
            .map(
                |p| rpc::forge::RackHardwareTopology::try_from(p.rack_hardware_topology)
                    .unwrap_or_default()
                    .as_str_name()
                    .to_string()
            )
            .unwrap_or_else(|| "N/A".to_string())
    ]);
    table.add_row(row![
        "Hardware Class",
        profile
            .map(
                |p| rpc::forge::RackHardwareClass::try_from(p.rack_hardware_class)
                    .unwrap_or_default()
                    .as_str_name()
                    .to_string()
            )
            .unwrap_or_else(|| "N/A".to_string())
    ]);
    table.add_row(row!["", ""]);
    table.add_row(row![
        "Compute Name",
        compute.and_then(|c| c.name.as_deref()).unwrap_or("N/A")
    ]);
    table.add_row(row!["Compute Count", compute.map(|c| c.count).unwrap_or(0)]);
    table.add_row(row![
        "Compute Vendor",
        compute.and_then(|c| c.vendor.as_deref()).unwrap_or("N/A")
    ]);
    table.add_row(row![
        "Compute Slot IDs",
        compute
            .map(|c| slot_ids_display(&c.slot_ids))
            .unwrap_or_else(|| "N/A".to_string())
    ]);
    table.add_row(row!["", ""]);
    table.add_row(row![
        "Switch Name",
        switch.and_then(|s| s.name.as_deref()).unwrap_or("N/A")
    ]);
    table.add_row(row!["Switch Count", switch.map(|s| s.count).unwrap_or(0)]);
    table.add_row(row![
        "Switch Vendor",
        switch.and_then(|s| s.vendor.as_deref()).unwrap_or("N/A")
    ]);
    table.add_row(row![
        "Switch Slot IDs",
        switch
            .map(|s| slot_ids_display(&s.slot_ids))
            .unwrap_or_else(|| "N/A".to_string())
    ]);
    table.add_row(row!["", ""]);
    table.add_row(row![
        "Power Shelf Name",
        power_shelf.and_then(|p| p.name.as_deref()).unwrap_or("N/A")
    ]);
    table.add_row(row![
        "Power Shelf Count",
        power_shelf.map(|p| p.count).unwrap_or(0)
    ]);
    table.add_row(row![
        "Power Shelf Vendor",
        power_shelf
            .and_then(|p| p.vendor.as_deref())
            .unwrap_or("N/A")
    ]);
    table.add_row(row![
        "Power Shelf Slot IDs",
        power_shelf
            .map(|p| slot_ids_display(&p.slot_ids))
            .unwrap_or_else(|| "N/A".to_string())
    ]);
    table.printstd();
}
