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

use carbide_utils::cmd::{Cmd, CmdError};
use regex::Regex;
use rpc::machine_discovery::DpuData;

#[derive(thiserror::Error, Debug)]
pub enum DpuEnumerationError {
    #[error("failed reading basic DPU info: {0}")]
    BasicInfo(String),
    #[error("regex error {0}")]
    Regex(#[from] regex::Error),
    #[error("command error {0}")]
    Cmd(#[from] CmdError),
    #[error("DPU enumeration failed reading '{0}': {1}")]
    Read(&'static str, String),
    #[error("no BlueField device found under /dev/mst")]
    NoBlueField,
}

fn get_flint_query() -> Result<String, DpuEnumerationError> {
    if cfg!(test) {
        const TEST_DATA: &str = "test/flint_query.txt";
        return std::fs::read_to_string(TEST_DATA)
            .map_err(|x| DpuEnumerationError::Read(TEST_DATA, x.to_string()));
    }

    // Query each mst device on its own and keep only the BlueField ones,
    // stopping at the first match since the parser below takes the first
    // occurrence of each field regardless.
    //
    // Exiting non-zero when nothing could be queried is what keeps the two
    // failures apart. `flint` missing, the mst driver not started, and
    // /dev/mst unreadable all yield no output, and reporting those as "this
    // host has no BlueField" would send diagnosis after the wrong thing. Only
    // an empty result from devices that did answer means that.
    const FLINT_QUERY: &str = r#"
shopt -s nullglob
queried=0
for dev in /dev/mst/*_pciconf0; do
    if out=$(flint -d "$dev" q full 2>&1); then
        queried=1
        if echo "$out" | grep -q "Description:.*BlueField"; then
            echo "$out"
            exit 0
        fi
    else
        echo "$dev: $out" >&2
    fi
done
[ "$queried" -eq 1 ] || exit 1
"#;
    let output = Cmd::new("bash").args(vec!["-c", FLINT_QUERY]).output()?;

    // Devices answered and none of them is a BlueField. Reported here rather
    // than left to the field parsing below, which would blame the empty output
    // on a missing firmware version.
    if output.trim().is_empty() {
        return Err(DpuEnumerationError::NoBlueField);
    }
    Ok(output)
}

pub fn get_dpu_info() -> Result<DpuData, DpuEnumerationError> {
    let fw_ver_pattern = Regex::new("FW Version:\\s*(.*?)$")?;
    let fw_date_pattern = Regex::new("FW Release Date:\\s*(.*?)$")?;
    let part_num_pattern = Regex::new("Part Number:\\s*(.*?)$")?;
    let desc_pattern = Regex::new("Description:\\s*(.*?)$")?;
    let prod_ver_pattern = Regex::new("Product Version:\\s*(.*?)$")?;
    let base_mac_pattern = Regex::new("Base MAC:\\s+([[:alnum:]]+?)\\s+(.*?)$")?;

    let output = get_flint_query()?;
    let fw_ver = output
        .lines()
        .filter_map(|line| fw_ver_pattern.captures(line))
        .map(|x| x[1].trim().to_string())
        .take(1)
        .collect::<Vec<String>>();

    if fw_ver.is_empty() {
        return Err(DpuEnumerationError::BasicInfo(
            "Could not find firmware version.".to_string(),
        ));
    }
    let fw_date = output
        .lines()
        .filter_map(|line| fw_date_pattern.captures(line))
        .map(|x| x[1].trim().to_string())
        .take(1)
        .collect::<Vec<String>>();

    if fw_date.is_empty() {
        return Err(DpuEnumerationError::BasicInfo(
            "Could not find firmware date.".to_string(),
        ));
    }

    let part_number = output
        .lines()
        .filter_map(|line| part_num_pattern.captures(line))
        .map(|x| x[1].trim().to_string())
        .take(1)
        .collect::<Vec<String>>();

    if part_number.is_empty() {
        return Err(DpuEnumerationError::BasicInfo(
            "Could not find part number.".to_string(),
        ));
    }

    let device_description = output
        .lines()
        .filter_map(|line| desc_pattern.captures(line))
        .map(|x| x[1].trim().to_string())
        .take(1)
        .collect::<Vec<String>>();

    if device_description.is_empty() {
        return Err(DpuEnumerationError::BasicInfo(
            "Could not find device description.".to_string(),
        ));
    }

    let product_version = output
        .lines()
        .filter_map(|line| prod_ver_pattern.captures(line))
        .map(|x| x[1].trim().to_string())
        .take(1)
        .collect::<Vec<String>>();

    if product_version.is_empty() {
        return Err(DpuEnumerationError::BasicInfo(
            "Could not find product version.".to_string(),
        ));
    }

    let factory_mac_address = output
        .lines()
        .filter_map(|line| base_mac_pattern.captures(line))
        .map(|x| x[1].trim().to_string())
        .take(1)
        .collect::<Vec<String>>();

    if factory_mac_address.is_empty() {
        return Err(DpuEnumerationError::BasicInfo(
            "Could not find factory mac address.".to_string(),
        ));
    }
    // flint produces mac address without : separators
    let mut factory_mac = String::with_capacity(18);
    factory_mac.insert_str(0, &factory_mac_address[0]);
    if factory_mac.find(':').is_none() {
        factory_mac.insert(2, ':');
        factory_mac.insert(5, ':');
        factory_mac.insert(8, ':');
        factory_mac.insert(11, ':');
        factory_mac.insert(14, ':');
    }

    let dpu_info = DpuData {
        part_number: part_number[0].clone(),
        part_description: device_description[0].clone(),
        product_version: product_version[0].clone(),
        factory_mac_address: factory_mac,
        firmware_version: fw_ver[0].clone(),
        firmware_date: fw_date[0].clone(),
        // Left empty here; LLDP neighbors are collected and reported separately
        // by the lldp_collector (lldp_reporter will be done in next PRs).
        switches: vec![],
    };
    Ok(dpu_info)
}
