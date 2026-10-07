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

use ::rpc::admin_cli::OutputFormat;
use ::rpc::forge::ComputeTrayComponent;
use prettytable::{Cell, Row, Table};

use super::args::Args;
use crate::component_manager::common;
use crate::errors::CarbideCliError;
use crate::rpc::ApiClient;

/// Formats a raw proto `ComputeTrayComponent` value for display.
///
/// Keep in sync with `map_compute_tray_component_names` and
/// `firmware_component_type_to_proto` in `api/src/handlers/component_manager.rs`.
fn format_compute_tray_component(value: i32) -> String {
    match ComputeTrayComponent::try_from(value) {
        Ok(ComputeTrayComponent::Bmc) => "BMC".to_string(),
        Ok(ComputeTrayComponent::Bios) => "BIOS".to_string(),
        Ok(ComputeTrayComponent::Cec) => "CEC".to_string(),
        Ok(ComputeTrayComponent::Nic) => "NIC".to_string(),
        Ok(ComputeTrayComponent::CpldMb) => "CPLD_MB".to_string(),
        Ok(ComputeTrayComponent::CpldPdb) => "CPLD_PDB".to_string(),
        Ok(ComputeTrayComponent::HgxBmc) => "HGX_BMC".to_string(),
        Ok(ComputeTrayComponent::CombinedBmcUefi) => "COMBINED_BMC_UEFI".to_string(),
        Ok(ComputeTrayComponent::Gpu) => "GPU".to_string(),
        Ok(ComputeTrayComponent::Cx7) => "CX7".to_string(),
        Ok(ComputeTrayComponent::Unknown) => format!("UNKNOWN({value})"),
        Err(e) => format!("UNRECOGNIZED({value}: {e})"),
    }
}

pub(super) async fn list_versions(
    opts: Args,
    format: OutputFormat,
    api_client: &ApiClient,
) -> Result<(), CarbideCliError> {
    let response = api_client
        .0
        .list_component_firmware_versions(opts)
        .await
        .map_err(CarbideCliError::from)?;

    if format == OutputFormat::Json {
        let devices = response
            .devices
            .iter()
            .map(|device| {
                let compute_fw_versions: serde_json::Map<String, serde_json::Value> = device
                    .compute_fw_versions
                    .iter()
                    .map(|cv| {
                        let name = format_compute_tray_component(cv.component);
                        (name, serde_json::json!(cv.versions))
                    })
                    .collect();

                let mut obj = serde_json::json!({
                    "result": common::component_result_json(device.result.as_ref()),
                    "versions": device.versions,
                });
                if !compute_fw_versions.is_empty() {
                    obj["compute_fw_versions"] = serde_json::Value::Object(compute_fw_versions);
                }
                obj
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&devices)?);
    } else {
        let mut table = Table::new();
        table.set_titles(Row::new(vec![
            Cell::new("Component ID"),
            Cell::new("Result"),
            Cell::new("Versions"),
            Cell::new("Error"),
        ]));

        for device in &response.devices {
            let (component_id, result_status, error) =
                common::component_result_fields(device.result.as_ref());
            if !device.compute_fw_versions.is_empty() {
                for cv in &device.compute_fw_versions {
                    let name = format_compute_tray_component(cv.component);
                    let versions = common::join_or_dash(&cv.versions);
                    table.add_row(Row::new(vec![
                        Cell::new(&component_id),
                        Cell::new(&result_status),
                        Cell::new(&format!("{name}: {versions}")),
                        Cell::new(&error),
                    ]));
                }
            } else {
                let versions = common::join_or_dash(&device.versions);
                table.add_row(Row::new(vec![
                    Cell::new(&component_id),
                    Cell::new(&result_status),
                    Cell::new(&versions),
                    Cell::new(&error),
                ]));
            }
        }

        table.printstd();
    }

    let (failures, failure_summary) = common::component_failure_count_and_summary(
        response.devices.iter().map(|device| device.result.as_ref()),
    );

    if failures > 0 {
        return Err(CarbideCliError::GenericError(format!(
            "{failures} component firmware version result(s) failed{failure_summary}"
        )));
    }

    Ok(())
}
