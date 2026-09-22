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

use std::net::IpAddr;
use std::str::FromStr;

use carbide_secrets::credentials::Credentials;
use carbide_utils::none_if_empty::NoneIfEmpty;
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::SwitchId;
use component_manager::nv_switch_manager::{
    ScaleUpFabricResponseStatus, ScaleUpFabricServiceStatuses, ScaleUpFabricStatus, SwitchEndpoint,
};
use db::switch as db_switch;
use mac_address::MacAddress;
use model::rack::FirmwareUpgradeDeviceInfo;
use sqlx::PgConnection;

pub(super) fn validate_switch_inventory_for_nmx_cluster(
    switches: &[FirmwareUpgradeDeviceInfo],
) -> Result<(), String> {
    for switch in switches {
        if switch.os_ip.as_deref().unwrap_or_default().is_empty() {
            return Err(format!(
                "switch {} is missing an NVOS IP address for ConfigureNmxCluster",
                switch.node_id
            ));
        }
        if switch.os_username.as_deref().unwrap_or_default().is_empty()
            || switch.os_password.as_deref().unwrap_or_default().is_empty()
        {
            return Err(format!(
                "switch {} is missing NVOS credentials for ConfigureNmxCluster",
                switch.node_id
            ));
        }
    }

    Ok(())
}

pub(super) async fn persist_fabric_manager_statuses(
    txn: &mut PgConnection,
    rack_id: &RackId,
    switches: &[FirmwareUpgradeDeviceInfo],
    response: &ScaleUpFabricServiceStatuses,
) -> Result<(), String> {
    match response.status {
        ScaleUpFabricResponseStatus::Success => {}
        ScaleUpFabricResponseStatus::Failure => {
            return Err(
                "RMS BatchGetScaleUpFabricServiceStatus returned failure for ConfigureNmxCluster"
                    .to_string(),
            );
        }
        ScaleUpFabricResponseStatus::Unknown(code) => {
            return Err(format!(
                "RMS BatchGetScaleUpFabricServiceStatus returned unknown status code {code} for ConfigureNmxCluster"
            ));
        }
    }

    for switch in switches {
        let Some(entry) = response.service_statuses.get(switch.node_id.as_str()) else {
            return Err(format!(
                "RMS did not return fabric-manager status for switch {}",
                switch.node_id
            ));
        };
        let switch_id = switch.node_id.parse::<SwitchId>().map_err(|error| {
            format!(
                "invalid switch id {} while persisting fabric-manager status: {}",
                switch.node_id, error
            )
        })?;

        db_switch::update_fabric_manager_status(txn, switch_id, Some(entry))
            .await
            .map_err(|error| {
                format!(
                    "failed to persist fabric-manager status for switch {}: {}",
                    switch.node_id, error
                )
            })?;

        tracing::info!(
            rack_id = %rack_id,
            switch_id = %switch.node_id,
            fabric_manager_status = %entry.display_status(),
            raw_fabric_manager_state = ?entry.fabric_manager_state,
            error_message = %entry.error_message.as_deref().unwrap_or_default(),
            "Persisted FabricManager status for switch"
        );
    }

    Ok(())
}

/// Returns the primary switch observed in an RMS ScaleUpFabric status response.
///
/// A valid response must succeed and mark exactly one submitted rack switch as
/// enabled.
///
/// # Errors
///
/// Returns an error when RMS does not report one valid primary.
pub(super) fn observed_primary_switch(
    switches: &[FirmwareUpgradeDeviceInfo],
    response: &ScaleUpFabricStatus,
) -> Result<SwitchId, String> {
    match response.status {
        ScaleUpFabricResponseStatus::Success => {}
        ScaleUpFabricResponseStatus::Failure => {
            let details = if response.error_message.trim().is_empty() {
                "no error details provided"
            } else {
                response.error_message.as_str()
            };

            return Err(format!("RMS GetScaleUpFabricStatus failed: {details}"));
        }
        ScaleUpFabricResponseStatus::Unknown(status) => {
            return Err(format!(
                "RMS GetScaleUpFabricStatus returned invalid status {}",
                status
            ));
        }
    }

    let Some(switch_statuses) = response.switches.as_ref() else {
        return Err("RMS GetScaleUpFabricStatus returned no fabric status".to_string());
    };

    let mut enabled_switches = switch_statuses.iter().filter(|switch| switch.enabled);

    let Some(enabled_switch) = enabled_switches.next() else {
        return Err("RMS GetScaleUpFabricStatus reported no primary switch".to_string());
    };

    if enabled_switches.next().is_some() {
        return Err("RMS GetScaleUpFabricStatus reported multiple primary switches".to_string());
    }

    let observed_primary = enabled_switch
        .node_id
        .parse::<SwitchId>()
        .map_err(|error| {
            format!(
                "RMS returned invalid primary switch ID '{}': {error}",
                enabled_switch.node_id
            )
        })?;

    if !switches
        .iter()
        .any(|switch| switch.node_id == enabled_switch.node_id)
    {
        return Err(format!(
            "RMS returned primary switch {} outside the submitted rack",
            enabled_switch.node_id
        ));
    }

    if !enabled_switch.error_message.trim().is_empty() {
        return Err(format!(
            "RMS failed to inspect primary switch {}: {}",
            enabled_switch.node_id, enabled_switch.error_message
        ));
    }

    Ok(observed_primary)
}

pub(super) async fn persist_primary_switch(
    txn: &mut PgConnection,
    rack_id: &RackId,
    primary_switch_node_id: &str,
) -> Result<(), String> {
    let primary_switch_id = primary_switch_node_id
        .parse::<SwitchId>()
        .map_err(|error| {
            format!(
                "selected primary switch '{}' is not a valid SwitchId: {}",
                primary_switch_node_id, error
            )
        })?;

    db_switch::set_primary_switch_for_rack(txn, rack_id, &primary_switch_id)
        .await
        .map_err(|error| {
            format!(
                "failed to persist primary switch '{}' for rack {}: {}",
                primary_switch_node_id, rack_id, error
            )
        })?;

    Ok(())
}

/// Builds a Component Manager switch endpoint from RMS firmware inventory.
///
/// Falls back to BMC credentials for NVOS when `os_username` or
/// `os_password` is absent.
///
/// # Errors
///
/// Returns an error when the BMC or NVOS MAC/IP fields do not parse.
pub(super) fn switch_endpoint_from_firmware_device(
    device: &FirmwareUpgradeDeviceInfo,
) -> Result<SwitchEndpoint, String> {
    let bmc_mac = MacAddress::from_str(&device.mac)
        .map_err(|error| format!("switch {} has invalid BMC MAC: {error}", device.node_id))?;

    let bmc_ip = IpAddr::from_str(&device.bmc_ip)
        .map_err(|error| format!("switch {} has invalid BMC IP: {error}", device.node_id))?;

    let nvos_mac = MacAddress::from_str(device.os_mac.as_deref().unwrap_or_default())
        .map_err(|error| format!("switch {} has invalid NVOS MAC: {error}", device.node_id))?;

    let nvos_ip = IpAddr::from_str(device.os_ip.as_deref().unwrap_or_default())
        .map_err(|error| format!("switch {} has invalid NVOS IP: {error}", device.node_id))?;

    let bmc_credentials = Credentials::UsernamePassword {
        username: device.bmc_username.clone(),
        password: device.bmc_password.clone(),
    };

    let nvos_credentials = Credentials::UsernamePassword {
        username: device
            .os_username
            .clone()
            .unwrap_or_else(|| device.bmc_username.clone()),
        password: device
            .os_password
            .clone()
            .unwrap_or_else(|| device.bmc_password.clone()),
    };

    Ok(SwitchEndpoint {
        bmc_ip,
        bmc_mac,
        nvos_ip,
        nvos_mac,
        bmc_credentials,
        nvos_credentials,
        nvos_host_name: device.os_hostname.clone().none_if_empty(),
    })
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Case, Check, Outcome, check_cases, check_values};
    use carbide_uuid::switch::{SwitchIdSource, SwitchType};
    use component_manager::nv_switch_manager::ScaleUpFabricSwitchStatus;

    use super::*;

    fn switch(node_id: &str) -> FirmwareUpgradeDeviceInfo {
        FirmwareUpgradeDeviceInfo {
            node_id: node_id.to_string(),
            mac: "00:11:22:33:44:55".to_string(),
            bmc_ip: "192.0.2.10".to_string(),
            bmc_username: "admin".to_string(),
            bmc_password: "password".to_string(),
            os_mac: Some("aa:bb:cc:dd:ee:ff".to_string()),
            os_ip: Some("198.51.100.10".to_string()),
            os_username: Some("nvos".to_string()),
            os_password: Some("password".to_string()),
            os_hostname: None,
        }
    }

    #[test]
    fn switch_endpoint_from_firmware_device_validates_each_field() {
        let with = |mutate: fn(&mut FirmwareUpgradeDeviceInfo)| {
            let mut device = switch("switch-1");
            mutate(&mut device);
            device
        };

        let discriminating_phrases = [
            "invalid BMC MAC",
            "invalid BMC IP",
            "invalid NVOS MAC",
            "invalid NVOS IP",
        ];

        check_cases(
            [
                Case {
                    scenario: "valid device",
                    input: switch("switch-1"),
                    expect: Outcome::Yields(()),
                },
                Case {
                    scenario: "invalid BMC MAC",
                    input: with(|d| d.mac = "not-a-mac".to_string()),
                    expect: Outcome::FailsWith("invalid BMC MAC"),
                },
                Case {
                    scenario: "invalid BMC IP",
                    input: with(|d| d.bmc_ip = "999.0.0.1".to_string()),
                    expect: Outcome::FailsWith("invalid BMC IP"),
                },
                Case {
                    scenario: "missing NVOS MAC",
                    input: with(|d| d.os_mac = None),
                    expect: Outcome::FailsWith("invalid NVOS MAC"),
                },
                Case {
                    scenario: "missing NVOS IP",
                    input: with(|d| d.os_ip = None),
                    expect: Outcome::FailsWith("invalid NVOS IP"),
                },
            ],
            |device| {
                switch_endpoint_from_firmware_device(&device)
                    .map(|_| ())
                    .map_err(|error| {
                        discriminating_phrases
                            .into_iter()
                            .find(|phrase| error.contains(phrase))
                            .unwrap_or("unexpected error")
                    })
            },
        );
    }

    #[test]
    fn switch_endpoint_from_firmware_device_falls_back_to_bmc_credentials() {
        let mut device = switch("switch-1");
        device.os_username = None;
        device.os_password = None;

        let endpoint = switch_endpoint_from_firmware_device(&device).expect("valid device");

        assert_eq!(endpoint.nvos_credentials, endpoint.bmc_credentials);
    }

    #[test]
    fn observed_primary_switch_requires_one_submitted_rack_switch() {
        let first_id = SwitchId::new(SwitchIdSource::Tpm, [1; 32], SwitchType::NvLink).to_string();
        let second_id = SwitchId::new(SwitchIdSource::Tpm, [2; 32], SwitchType::NvLink).to_string();
        let expected_switches = vec![switch(&first_id), switch(&second_id)];

        let switch_status = |node_id: &str, enabled| ScaleUpFabricSwitchStatus {
            node_id: node_id.to_string(),
            enabled,
            error_message: String::new(),
        };

        let response = |status, switches| ScaleUpFabricStatus {
            status,
            switches,
            error_message: String::new(),
        };

        check_values(
            [
                Check {
                    scenario: "one enabled submitted switch",
                    input: response(
                        ScaleUpFabricResponseStatus::Success,
                        Some(vec![
                            switch_status(&first_id, false),
                            switch_status(&second_id, true),
                        ]),
                    ),
                    expect: Some(second_id.clone()),
                },
                Check {
                    scenario: "RMS failure",
                    input: response(ScaleUpFabricResponseStatus::Failure, Some(Vec::new())),
                    expect: None,
                },
                Check {
                    scenario: "unknown response status",
                    input: response(ScaleUpFabricResponseStatus::Unknown(17), Some(Vec::new())),
                    expect: None,
                },
                Check {
                    scenario: "missing fabric status",
                    input: response(ScaleUpFabricResponseStatus::Success, None),
                    expect: None,
                },
                Check {
                    scenario: "no enabled switch",
                    input: response(
                        ScaleUpFabricResponseStatus::Success,
                        Some(vec![switch_status(&first_id, false)]),
                    ),
                    expect: None,
                },
                Check {
                    scenario: "multiple enabled switches",
                    input: response(
                        ScaleUpFabricResponseStatus::Success,
                        Some(vec![
                            switch_status(&first_id, true),
                            switch_status(&second_id, true),
                        ]),
                    ),
                    expect: None,
                },
                Check {
                    scenario: "enabled switch outside submitted rack",
                    input: response(
                        ScaleUpFabricResponseStatus::Success,
                        Some(vec![switch_status(
                            &SwitchId::new(SwitchIdSource::Tpm, [3; 32], SwitchType::NvLink)
                                .to_string(),
                            true,
                        )]),
                    ),
                    expect: None,
                },
            ],
            |response| {
                observed_primary_switch(&expected_switches, &response)
                    .ok()
                    .map(|primary| primary.to_string())
            },
        );
    }
}
