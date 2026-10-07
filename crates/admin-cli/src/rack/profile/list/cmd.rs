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
use prettytable::{Cell, Row, Table};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{
    ConfiguredRackProfile, RackCapabilityCompute, RackCapabilityPowerShelf, RackCapabilitySwitch,
    RackHardwareClass, RackHardwareTopology, RackProductFamily,
};

use super::Args;
use crate::cfg::runtime::RuntimeConfig;
use crate::rpc::ApiClient;

fn product_family_name(value: i32) -> &'static str {
    RackProductFamily::try_from(value)
        .unwrap_or_default()
        .as_str_name()
}

fn topology_name(value: i32) -> &'static str {
    RackHardwareTopology::try_from(value)
        .unwrap_or_default()
        .as_str_name()
}

fn hardware_class_name(value: i32) -> &'static str {
    RackHardwareClass::try_from(value)
        .unwrap_or_default()
        .as_str_name()
}

fn capability_summary(count: u32, name: Option<&str>, vendor: Option<&str>) -> String {
    [
        Some(count.to_string()),
        vendor.map(str::to_string),
        name.map(str::to_string),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join(" ")
}

fn compute_summary(capability: Option<&RackCapabilityCompute>) -> String {
    capability
        .map(|value| {
            capability_summary(value.count, value.name.as_deref(), value.vendor.as_deref())
        })
        .unwrap_or_else(|| "N/A".to_string())
}

fn switch_summary(capability: Option<&RackCapabilitySwitch>) -> String {
    capability
        .map(|value| {
            capability_summary(value.count, value.name.as_deref(), value.vendor.as_deref())
        })
        .unwrap_or_else(|| "N/A".to_string())
}

fn power_shelf_summary(capability: Option<&RackCapabilityPowerShelf>) -> String {
    capability
        .map(|value| {
            capability_summary(value.count, value.name.as_deref(), value.vendor.as_deref())
        })
        .unwrap_or_else(|| "N/A".to_string())
}

fn build_profile_table(rack_profiles: &[ConfiguredRackProfile]) -> Table {
    let mut table = Table::new();
    table.set_titles(Row::new(
        [
            "Profile ID",
            "Product Family",
            "Hardware Type",
            "Topology",
            "Class",
            "Compute",
            "Switch",
            "Power Shelf",
        ]
        .into_iter()
        .map(Cell::new)
        .collect(),
    ));

    for configured in rack_profiles {
        let profile = configured.profile.as_ref();
        let capabilities = profile.and_then(|value| value.capabilities.as_ref());
        table.add_row(prettytable::row![
            configured
                .rack_profile_id
                .as_ref()
                .map(ToString::to_string)
                .unwrap_or_default(),
            profile
                .map(|value| product_family_name(value.product_family))
                .unwrap_or("UNSPECIFIED"),
            profile
                .and_then(|value| value.rack_hardware_type.as_ref())
                .map(|value| value.value.as_str())
                .unwrap_or("N/A"),
            profile
                .map(|value| topology_name(value.rack_hardware_topology))
                .unwrap_or("UNSPECIFIED"),
            profile
                .map(|value| hardware_class_name(value.rack_hardware_class))
                .unwrap_or("UNSPECIFIED"),
            compute_summary(capabilities.and_then(|value| value.compute.as_ref())),
            switch_summary(capabilities.and_then(|value| value.switch.as_ref())),
            power_shelf_summary(capabilities.and_then(|value| value.power_shelf.as_ref())),
        ]);
    }

    table
}

pub(in crate::rack::profile) async fn list_profiles(
    api_client: &ApiClient,
    _args: Args,
    config: &RuntimeConfig,
) -> Result<()> {
    let response = api_client.list_rack_profiles().await?;

    match config.format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&response.rack_profiles)?);
        }
        OutputFormat::Yaml => println!("{}", serde_yaml::to_string(&response.rack_profiles)?),
        _ if response.rack_profiles.is_empty() => println!("No rack profiles configured"),
        _ => build_profile_table(&response.rack_profiles).printstd(),
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use carbide_uuid::rack::RackProfileId;
    use rpc::common::RackHardwareType;
    use rpc::forge::{RackCapabilitiesSet, RackProfile};

    use super::*;

    #[test]
    fn profile_table_displays_id_and_useful_profile_details() {
        let rack_profiles = vec![ConfiguredRackProfile {
            rack_profile_id: Some(RackProfileId::new("NVL72")),
            profile: Some(RackProfile {
                rack_hardware_type: Some(RackHardwareType {
                    value: "wiwynn_gb200_nvl72".to_string(),
                }),
                rack_hardware_topology: RackHardwareTopology::Gb200Nvl72r1C2g4 as i32,
                rack_hardware_class: RackHardwareClass::Prod as i32,
                capabilities: Some(RackCapabilitiesSet {
                    compute: Some(RackCapabilityCompute {
                        name: Some("GB200".to_string()),
                        count: 18,
                        vendor: Some("Wiwynn".to_string()),
                        slot_ids: vec![],
                    }),
                    switch: Some(RackCapabilitySwitch {
                        name: Some("ND5200".to_string()),
                        count: 9,
                        vendor: Some("NVIDIA".to_string()),
                        slot_ids: vec![],
                    }),
                    power_shelf: Some(RackCapabilityPowerShelf {
                        name: Some("PowerShelf".to_string()),
                        count: 8,
                        vendor: Some("LiteOn".to_string()),
                        slot_ids: vec![],
                    }),
                }),
                product_family: RackProductFamily::Gb200 as i32,
            }),
        }];

        let mut output = Vec::new();
        build_profile_table(&rack_profiles)
            .print(&mut output)
            .expect("unable to render profile table");
        let output = String::from_utf8(output).expect("table output was not UTF-8");

        for expected in [
            "NVL72",
            "GB200",
            "wiwynn_gb200_nvl72",
            "GB200_NVL72R1_C2G4",
            "PROD",
            "18 Wiwynn GB200",
            "9 NVIDIA ND5200",
            "8 LiteOn PowerShelf",
        ] {
            assert!(
                output.contains(expected),
                "missing {expected:?} in {output}"
            );
        }
    }
}
