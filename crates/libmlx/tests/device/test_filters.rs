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
use std::str::FromStr;

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use carbide_test_support::Outcome::*;
use carbide_test_support::{scenarios, value_scenarios};
use libmlx::device::filters::{DeviceField, DeviceFilter, DeviceFilterSet, MatchMode};

// A single filter against the fully-populated test device should match across
// every field and match mode -- device type (exact/prefix/regex/complex regex),
// part number, firmware, MAC, and the case-insensitive/substring description
// paths -- plus the OR-logic case where any one of several values matches.
#[test]
fn filter_matches_complete_device() {
    let device = MlxDeviceInfo::create_test_device();

    value_scenarios!(
        run = |filter| filter.matches(&device);
        "device_type exact \"ConnectX-6 Dx\"" {
            DeviceFilter::device_type(
                vec!["ConnectX-6 Dx".to_string()],
                MatchMode::Exact,
            ) => true,
        }

        "device_type prefix \"ConnectX\"" {
            DeviceFilter::device_type(vec!["ConnectX".to_string()], MatchMode::Prefix) => true,
        }

        "device_type regex \"Connect.*\"" {
            DeviceFilter::device_type(vec!["Connect.*".to_string()], MatchMode::Regex) => true,
        }

        "device_type complex regex \".*X-6.*\"" {
            DeviceFilter::device_type(vec![".*X-6.*".to_string()], MatchMode::Regex) => true,
        }

        "part_number prefix \"MCX623\"" {
            DeviceFilter::part_number(vec!["MCX623".to_string()], MatchMode::Prefix) => true,
        }

        "firmware_version prefix \"22.32\"" {
            DeviceFilter::firmware_version(vec!["22.32".to_string()], MatchMode::Prefix) => true,
        }

        "mac_address prefix \"b8:3f:d2\"" {
            DeviceFilter::mac_address(vec!["b8:3f:d2".to_string()], MatchMode::Prefix) => true,
        }

        "description regex substring \".*100GbE.*\"" {
            DeviceFilter::description(vec![".*100GbE.*".to_string()], MatchMode::Regex) => true,
        }

        "description case-insensitive prefix \"mellanox\"" {
            DeviceFilter::description(vec!["mellanox".to_string()], MatchMode::Prefix) => true,
        }

        "multiple values, OR logic (one value matches)" {
            DeviceFilter::device_type(
                vec!["ConnectX-7".to_string(), "ConnectX-6 Dx".to_string()],
                MatchMode::Exact,
            ) => true,
        }
    );
}

// The device with missing data has only its device type and status populated.
// Filters on present fields match (status, device type); filters on absent
// fields (part number, firmware, MAC) do not.
#[test]
fn filter_matches_device_with_missing_data() {
    let device = MlxDeviceInfo::create_test_device_with_missing_data();

    value_scenarios!(
        run = |filter| filter.matches(&device);
        "status exact \"Failed to open device\" present" {
            DeviceFilter::status(
                vec!["Failed to open device".to_string()],
                MatchMode::Exact,
            ) => true,
        }

        "status prefix \"Failed\" present" {
            DeviceFilter::status(vec!["Failed".to_string()], MatchMode::Prefix) => true,
        }

        "device_type prefix \"BlueField\" present" {
            DeviceFilter::device_type(vec!["BlueField".to_string()], MatchMode::Prefix) => true,
        }

        "part_number prefix \"MCX\" absent" {
            DeviceFilter::part_number(vec!["MCX".to_string()], MatchMode::Prefix) => false,
        }

        "firmware_version prefix \"22.32\" absent" {
            DeviceFilter::firmware_version(vec!["22.32".to_string()], MatchMode::Prefix) => false,
        }

        "mac_address prefix \"b8:3f\" absent" {
            DeviceFilter::mac_address(vec!["b8:3f".to_string()], MatchMode::Prefix) => false,
        }
    );
}

#[test]
fn test_device_filter_set_no_filters_matches_all() {
    let device = MlxDeviceInfo::create_test_device();
    let filter_set = DeviceFilterSet::new();

    assert!(filter_set.matches(&device));
    assert!(!filter_set.has_filters());
}

#[test]
fn test_device_filter_set_multiple_criteria_all_match() {
    let device = MlxDeviceInfo::create_test_device();
    let mut filter_set = DeviceFilterSet::new();

    filter_set.add_filter(DeviceFilter::device_type(
        vec!["ConnectX".to_string()],
        MatchMode::Prefix,
    ));
    filter_set.add_filter(DeviceFilter::part_number(
        vec!["MCX".to_string()],
        MatchMode::Prefix,
    ));
    filter_set.add_filter(DeviceFilter::firmware_version(
        vec!["22".to_string()],
        MatchMode::Prefix,
    ));

    assert!(filter_set.matches(&device));
    assert!(filter_set.has_filters());
}

#[test]
fn test_device_filter_set_multiple_criteria_one_fails() {
    let device = MlxDeviceInfo::create_test_device();
    let mut filter_set = DeviceFilterSet::new();

    filter_set.add_filter(DeviceFilter::device_type(
        vec!["ConnectX".to_string()],
        MatchMode::Prefix,
    ));
    filter_set.add_filter(DeviceFilter::part_number(
        vec!["WRONG".to_string()],
        MatchMode::Prefix,
    ));

    assert!(!filter_set.matches(&device));
}

#[test]
fn test_device_filter_set_summary_empty() {
    let filter_set = DeviceFilterSet::new();
    let summary = filter_set.to_string();

    assert_eq!(summary, "No filters");
}

#[test]
fn test_device_filter_set_summary_with_filters() {
    let mut filter_set = DeviceFilterSet::new();

    filter_set.add_filter(DeviceFilter::device_type(
        vec!["ConnectX".to_string()],
        MatchMode::Prefix,
    ));
    filter_set.add_filter(DeviceFilter::part_number(
        vec!["MCX".to_string()],
        MatchMode::Prefix,
    ));

    let summary_vec = filter_set.summary();

    assert_eq!(summary_vec.len(), 2);
    assert!(summary_vec.iter().any(|s| s.contains("device_type")));
    assert!(summary_vec.iter().any(|s| s.contains("part_number")));
}

// DeviceFilter::from_str parses "field:values[:match_mode]". The field and the
// (comma-split) values are required; an omitted match mode defaults to Regex.
// Each row pins the full parsed triple (field, values, match_mode).
#[test]
fn device_filter_from_str_parses_field_values_and_mode() {
    scenarios!(
        run = |s| {
            DeviceFilter::from_str(s)
                .map(|f| (f.field, f.values, f.match_mode))
                .map_err(drop)
        };
        "\"device_type:ConnectX\" defaults to regex" {
            "device_type:ConnectX" => Yields((
                DeviceField::DeviceType,
                vec!["ConnectX".to_string()],
                MatchMode::Regex,
            )),
        }

        "\"part_number:MCX623:exact\" with explicit mode" {
            "part_number:MCX623:exact" => Yields((
                DeviceField::PartNumber,
                vec!["MCX623".to_string()],
                MatchMode::Exact,
            )),
        }

        "\"device_type:ConnectX-6,ConnectX-7:prefix\" splits values" {
            "device_type:ConnectX-6,ConnectX-7:prefix" => Yields((
                DeviceField::DeviceType,
                vec!["ConnectX-6".to_string(), "ConnectX-7".to_string()],
                MatchMode::Prefix,
            )),
        }
    );
}

// MatchMode::from_str accepts the three mode names case-insensitively and
// rejects anything else.
#[test]
fn match_mode_from_str_parses_known_modes() {
    scenarios!(
        run = |s| MatchMode::from_str(s).map_err(drop);
        "\"regex\"" {
            "regex" => Yields(MatchMode::Regex),
        }

        "\"exact\"" {
            "exact" => Yields(MatchMode::Exact),
        }

        "\"prefix\"" {
            "prefix" => Yields(MatchMode::Prefix),
        }

        "\"REGEX\" is case-insensitive" {
            "REGEX" => Yields(MatchMode::Regex),
        }

        "\"invalid\" is rejected" {
            "invalid" => Fails,
        }
    );
}

// DeviceField::from_str accepts each field's full name and its short alias, and
// rejects anything else.
#[test]
fn device_field_from_str_parses_names_and_aliases() {
    scenarios!(
        run = |s| DeviceField::from_str(s).map_err(drop);
        "\"device_type\"" {
            "device_type" => Yields(DeviceField::DeviceType),
        }

        "\"type\" alias" {
            "type" => Yields(DeviceField::DeviceType),
        }

        "\"part_number\"" {
            "part_number" => Yields(DeviceField::PartNumber),
        }

        "\"part\" alias" {
            "part" => Yields(DeviceField::PartNumber),
        }

        "\"firmware_version\"" {
            "firmware_version" => Yields(DeviceField::FirmwareVersion),
        }

        "\"fw\" alias" {
            "fw" => Yields(DeviceField::FirmwareVersion),
        }

        "\"status\"" {
            "status" => Yields(DeviceField::Status),
        }

        "\"invalid\" is rejected" {
            "invalid" => Fails,
        }
    );
}

#[test]
fn test_mixed_device_filtering() {
    let complete_device = MlxDeviceInfo::create_test_device();
    let partial_device = MlxDeviceInfo::create_test_device_with_missing_data();

    // Filter that should match only complete devices
    let part_filter = DeviceFilter::part_number(vec!["MCX".to_string()], MatchMode::Prefix);
    assert!(part_filter.matches(&complete_device));
    assert!(!part_filter.matches(&partial_device));

    // A ".*" regex on device type matches both, since device type is always present
    let type_filter = DeviceFilter::device_type(vec![".*".to_string()], MatchMode::Regex);
    assert!(type_filter.matches(&complete_device)); // ConnectX-6 Dx
    assert!(type_filter.matches(&partial_device)); // BlueField3

    // An explicit alternation matches both device types too
    let broad_filter =
        DeviceFilter::device_type(vec!["Connect.*|Blue.*".to_string()], MatchMode::Regex);
    assert!(broad_filter.matches(&complete_device));
    assert!(broad_filter.matches(&partial_device));
}
