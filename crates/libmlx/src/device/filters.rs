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

use std::fmt::Display;
use std::str::FromStr;

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use clap::ValueEnum;
use regex;
use serde::{Deserialize, Serialize};

// MatchMode defines how filter values should be matched against device fields.
// All matching is case-insensitive.
#[derive(Debug, Default, Clone, Serialize, Deserialize, PartialEq, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum MatchMode {
    /// Regex matches using regular expressions (default).
    #[value(name = "regex")]
    #[default]
    Regex,
    /// Exact performs case-insensitive exact matching.
    #[value(name = "exact")]
    Exact,
    /// Prefix matches if the device field starts with the filter value.
    #[value(name = "prefix")]
    Prefix,
}

impl FromStr for MatchMode {
    type Err = String;

    // from_str converts a string to a MatchMode enum.
    fn from_str(mode_str: &str) -> Result<Self, Self::Err> {
        match mode_str.to_lowercase().as_str() {
            "regex" => Ok(MatchMode::Regex),
            "exact" => Ok(MatchMode::Exact),
            "prefix" => Ok(MatchMode::Prefix),
            _ => Err(format!(
                "Unknown match mode '{mode_str}'. Valid modes: regex, exact, prefix"
            )),
        }
    }
}

impl Display for MatchMode {
    // fmt formats the match mode as its canonical name.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            MatchMode::Regex => "regex",
            MatchMode::Exact => "exact",
            MatchMode::Prefix => "prefix",
        };
        write!(f, "{name}")
    }
}

// DeviceFilter represents a single filter criterion for device matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceFilter {
    // field specifies which device field to match against.
    pub field: DeviceField,
    // values contains the list of acceptable values for this field.
    pub values: Vec<String>,
    // match_mode defines how values should be matched against the device field.
    #[serde(default)]
    pub match_mode: MatchMode,
}

impl FromStr for DeviceFilter {
    type Err = String;

    // from_str parses a filter expression in the format field:value:match_mode.
    // Values can be comma-separated for OR logic: field:value1,value2,value3:match_mode
    fn from_str(expression: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = expression.split(':').collect();

        if parts.len() < 2 || parts.len() > 3 {
            return Err(format!(
                "Invalid filter expression '{expression}'. Expected format: field:value[,value2,value3] or field:value[,value2,value3]:match_mode"
            ));
        }

        let field = DeviceField::from_str(parts[0])?;

        // Parse comma-separated values for OR logic.
        let values: Vec<String> = parts[1]
            .split(',')
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .collect();

        if values.is_empty() {
            return Err(format!(
                "No valid values found in filter expression '{expression}'"
            ));
        }

        let match_mode = if parts.len() == 3 {
            <MatchMode as FromStr>::from_str(parts[2])?
        } else {
            // Use regex as default for all fields.
            MatchMode::Regex
        };

        Ok(DeviceFilter {
            field,
            values,
            match_mode,
        })
    }
}

impl Display for DeviceFilter {
    // fmt formats the filter as field:value1,value2:match_mode.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let field_name = self.field.to_string();
        let values_str = self.values.join(",");
        let mode_name = self.match_mode.to_string();
        write!(f, "{field_name}:{values_str}:{mode_name}")
    }
}

impl Display for DeviceFilterSet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.filters.is_empty() {
            write!(f, "No filters")
        } else {
            let filter_strings: Vec<String> = self
                .filters
                .iter()
                .map(|filter| filter.to_string())
                .collect();
            write!(f, "{}", filter_strings.join(", "))
        }
    }
}

// DeviceField represents the available device fields for filtering.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceField {
    // DeviceType filters on the device type field.
    DeviceType,
    // PartNumber filters on the part number field.
    PartNumber,
    // FirmwareVersion filters on the current firmware version.
    FirmwareVersion,
    // MacAddress filters on the base MAC address.
    MacAddress,
    // Description filters on the device description.
    Description,
    // PciName filters on the PCI name/address.
    PciName,
    // Status filters on the device status from mlxfwmanager.
    Status,
}

impl FromStr for DeviceField {
    type Err = String;

    // from_str converts a string to a DeviceField enum.
    fn from_str(field_str: &str) -> Result<Self, Self::Err> {
        match field_str.to_lowercase().as_str() {
            "device_type" | "type" => Ok(DeviceField::DeviceType),
            "part_number" | "part" => Ok(DeviceField::PartNumber),
            "firmware_version" | "firmware" | "fw" => Ok(DeviceField::FirmwareVersion),
            "mac_address" | "mac" => Ok(DeviceField::MacAddress),
            "description" | "desc" => Ok(DeviceField::Description),
            "pci_name" | "pci" => Ok(DeviceField::PciName),
            "status" => Ok(DeviceField::Status),
            _ => Err(format!(
                "Unknown field '{field_str}'. Valid fields: device_type, part_number, firmware_version, mac_address, description, pci_name, status"
            )),
        }
    }
}

impl Display for DeviceField {
    // fmt formats the device field as its canonical name.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            DeviceField::DeviceType => "device_type",
            DeviceField::PartNumber => "part_number",
            DeviceField::FirmwareVersion => "firmware_version",
            DeviceField::MacAddress => "mac_address",
            DeviceField::Description => "description",
            DeviceField::PciName => "pci_name",
            DeviceField::Status => "status",
        };
        write!(f, "{name}")
    }
}

// DeviceFilterSet represents a collection of filters that must all match.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct DeviceFilterSet {
    // filters contains the list of individual filters to apply.
    pub filters: Vec<DeviceFilter>,
}

impl DeviceFilter {
    // device_type creates a filter for device type matching.
    pub fn device_type(values: Vec<String>, match_mode: MatchMode) -> Self {
        Self {
            field: DeviceField::DeviceType,
            values,
            match_mode,
        }
    }

    // part_number creates a filter for part number matching.
    pub fn part_number(values: Vec<String>, match_mode: MatchMode) -> Self {
        Self {
            field: DeviceField::PartNumber,
            values,
            match_mode,
        }
    }

    // firmware_version creates a filter for firmware version matching.
    pub fn firmware_version(values: Vec<String>, match_mode: MatchMode) -> Self {
        Self {
            field: DeviceField::FirmwareVersion,
            values,
            match_mode,
        }
    }

    // mac_address creates a filter for MAC address matching.
    pub fn mac_address(values: Vec<String>, match_mode: MatchMode) -> Self {
        Self {
            field: DeviceField::MacAddress,
            values,
            match_mode,
        }
    }

    // description creates a filter for description matching.
    pub fn description(values: Vec<String>, match_mode: MatchMode) -> Self {
        Self {
            field: DeviceField::Description,
            values,
            match_mode,
        }
    }

    // pci_name creates a filter for PCI name matching.
    pub fn pci_name(values: Vec<String>, match_mode: MatchMode) -> Self {
        Self {
            field: DeviceField::PciName,
            values,
            match_mode,
        }
    }

    // status creates a filter for device status matching.
    pub fn status(values: Vec<String>, match_mode: MatchMode) -> Self {
        Self {
            field: DeviceField::Status,
            values,
            match_mode,
        }
    }

    // matches checks if a device satisfies this filter.
    pub fn matches(&self, device: &MlxDeviceInfo) -> bool {
        let device_value = self.get_device_field_value(device);

        self.values
            .iter()
            .any(|filter_value| self.matches_value(&device_value, filter_value))
    }

    // get_device_field_value extracts the specified field value
    // from a device as a formatted string. Used for display and
    // filter matching purposes (since filter matching is all
    // based on strings).
    fn get_device_field_value(&self, device: &MlxDeviceInfo) -> String {
        match self.field {
            DeviceField::DeviceType => device.device_type_pretty(),
            DeviceField::PartNumber => device.part_number_pretty(),
            DeviceField::FirmwareVersion => device.fw_version_current_pretty(),
            DeviceField::MacAddress => device.base_mac_pretty(),
            DeviceField::Description => device.device_description_pretty(),
            DeviceField::PciName => device.pci_name_pretty(),
            DeviceField::Status => device.status_pretty(),
        }
    }

    // matches_value checks if a device field value matches a filter value.
    // All matching is case-insensitive.
    fn matches_value(&self, device_value: &str, filter_value: &str) -> bool {
        let device_lower = device_value.to_lowercase();
        let filter_lower = filter_value.to_lowercase();

        match self.match_mode {
            MatchMode::Regex => {
                // Use regex matching with case-insensitive flag.
                match regex::RegexBuilder::new(&filter_lower)
                    .case_insensitive(true)
                    .build()
                {
                    Ok(re) => re.is_match(&device_lower),
                    Err(_) => {
                        // If regex is invalid, fall back to exact matching.
                        device_lower == filter_lower
                    }
                }
            }
            MatchMode::Exact => device_lower == filter_lower,
            MatchMode::Prefix => device_lower.starts_with(&filter_lower),
        }
    }
}

impl DeviceFilterSet {
    // new creates a new empty filter set.
    pub fn new() -> Self {
        Self::default()
    }

    // with_filter adds a filter to this filter set.
    pub fn with_filter(mut self, filter: DeviceFilter) -> Self {
        self.filters.push(filter);
        self
    }

    // add_filter adds a filter to this filter set.
    pub fn add_filter(&mut self, filter: DeviceFilter) {
        self.filters.push(filter);
    }

    // matches checks if a device satisfies all filters in this set.
    pub fn matches(&self, device: &MlxDeviceInfo) -> bool {
        self.filters.iter().all(|filter| filter.matches(device))
    }

    // has_filters checks if any filters are specified.
    pub fn has_filters(&self) -> bool {
        !self.filters.is_empty()
    }

    // summary gets a vector of active filters for display purposes.
    pub fn summary(&self) -> Vec<String> {
        self.filters
            .iter()
            .map(|filter| filter.to_string())
            .collect()
    }
}
