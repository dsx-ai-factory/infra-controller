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

//! Shared switch fixtures for rack-controller and API integration tests.

use carbide_secrets::credentials::{
    BmcCredentialType, CredentialKey, CredentialWriter, Credentials,
};
use carbide_uuid::rack::RackId;
use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
use model::address_selection_strategy::AddressSelectionStrategy;
use model::expected_switch::ExpectedSwitch;
use model::switch::{NewSwitch, SwitchConfig, switch_id};

use crate::api::Api;

/// Attaches the first `count` persisted expected-switch models to a rack.
///
/// Creates NVOS and BMC interfaces for every supplied model in the existing
/// ADMIN and UNDERLAY segments. Interfaces and actual switches commit together;
/// BMC root (`root`/`notforprod`) and NVOS (`nvos-admin`/`nvos-pass`) credentials
/// are stored afterwards for the selected switches. Slot numbers follow input
/// order and tray indices are zero. Returns an error if fewer models are supplied.
pub async fn attach_switches_with_nvos_credentials(
    api: &Api,
    rack_id: &RackId,
    expected_switches: Vec<ExpectedSwitch>,
    count: usize,
) -> Result<Vec<SwitchId>, Box<dyn std::error::Error>> {
    // Expected records must already be committed before this transaction.
    // ADMIN and UNDERLAY must exist before provisioning interfaces and switches.
    let mut txn = api.database_connection.begin().await?;
    for record in &expected_switches {
        let admin_segments = db::network_segment::admin(txn.as_mut())
            .await
            .expect("failed to get admin network segment");
        for nvos_mac in record.nvos_mac_addresses() {
            db::machine_interface::create(
                txn.as_mut(),
                &admin_segments,
                &nvos_mac,
                false,
                AddressSelectionStrategy::NextAvailableIp,
                None,
            )
            .await
            .expect("failed to create NVOS machine interface");
        }
        let underlay_segment = db::network_segment::find_by_name(txn.as_mut(), "UNDERLAY")
            .await
            .expect("failed to get overlay network segment");
        db::machine_interface::create(
            txn.as_mut(),
            std::slice::from_ref(&underlay_segment),
            &record.bmc_mac_address,
            false,
            AddressSelectionStrategy::NextAvailableIp,
            None,
        )
        .await
        .expect("failed to create BMC machine interface");
    }
    let selected_switches = expected_switches
        .into_iter()
        .take(count)
        .collect::<Vec<_>>();
    if selected_switches.len() != count {
        txn.rollback().await?;
        return Err(eyre::eyre!("expected at least {} switch fixtures", count).into());
    }

    let mut switch_ids = Vec::with_capacity(selected_switches.len());
    for (index, expected_switch) in selected_switches.iter().enumerate() {
        let switch_id = switch_id::from_hardware_info(
            &expected_switch.serial_number,
            "NVIDIA",
            "Switch",
            SwitchIdSource::ProductBoardChassisSerial,
            SwitchType::NvLink,
        )?;

        let new_switch = NewSwitch {
            id: switch_id,
            config: SwitchConfig {
                name: expected_switch.metadata.name.clone(),
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some(expected_switch.bmc_mac_address),
            metadata: None,
            rack_id: Some(rack_id.clone()),
            slot_number: Some(index as i32),
            tray_index: Some(0),
        };
        db::switch::create(txn.as_mut(), &new_switch).await?;
        switch_ids.push(switch_id);
    }
    txn.commit().await?;

    for expected_switch in selected_switches {
        api.credential_manager()
            .set_credentials(
                &CredentialKey::BmcCredentials {
                    credential_type: BmcCredentialType::BmcRoot {
                        bmc_mac_address: expected_switch.bmc_mac_address,
                    },
                },
                &Credentials::UsernamePassword {
                    username: "root".to_string(),
                    password: "notforprod".to_string(),
                },
            )
            .await
            .map_err(|error| eyre::eyre!("failed to set switch BMC credentials: {}", error))?;
        api.credential_manager()
            .set_credentials(
                &CredentialKey::SwitchNvosAdmin {
                    bmc_mac_address: expected_switch.bmc_mac_address,
                },
                &Credentials::UsernamePassword {
                    username: "nvos-admin".to_string(),
                    password: "nvos-pass".to_string(),
                },
            )
            .await
            .map_err(|error| eyre::eyre!("failed to set switch NVOS credentials: {}", error))?;
    }

    Ok(switch_ids)
}
