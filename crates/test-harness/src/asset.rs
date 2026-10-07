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

use std::sync::Arc;

use carbide_uuid::power_shelf::{PowerShelfId, PowerShelfIdSource, PowerShelfType};
use carbide_uuid::rack::{RackId, RackProfileId};
use carbide_uuid::switch::{SwitchId, SwitchIdSource, SwitchType};
use model::power_shelf::{NewPowerShelf, PowerShelfConfig, power_shelf_id};
use model::rack::RackConfig;
use model::switch::{NewSwitch, SwitchConfig, switch_id};

use crate::rpc::forge::forge_server::Forge as _;
use crate::{Api, TestHarness, rpc};

pub struct TestRack {
    pub id: RackId,
}

impl TestRack {
    pub(crate) async fn create(test_harness: &TestHarness, rack_profile_id: RackProfileId) -> Self {
        let id = RackId::new(uuid::Uuid::new_v4().to_string());
        let mut txn = test_harness.db_txn().await;
        db::rack::create(
            &mut txn,
            &id,
            Some(&rack_profile_id),
            &RackConfig::default(),
            None,
        )
        .await
        .expect("rack should be created");
        txn.commit()
            .await
            .expect("database transaction should commit");
        Self { id }
    }
}

pub struct TestSwitch {
    pub id: SwitchId,
}

impl TestSwitch {
    pub(crate) async fn create(
        test_harness: &TestHarness,
        slot_number: i32,
        tray_index: i32,
    ) -> Self {
        let name = format!("Test Switch {}", &uuid::Uuid::new_v4().to_string()[..8]);
        let id = switch_id::from_hardware_info(
            &name,
            "NVIDIA",
            "Switch",
            SwitchIdSource::ProductBoardChassisSerial,
            SwitchType::NvLink,
        )
        .expect("switch id should be derived from test hardware info");
        let new_switch = NewSwitch {
            id,
            config: SwitchConfig {
                name,
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: None,
            metadata: None,
            rack_id: None,
            slot_number: Some(slot_number),
            tray_index: Some(tray_index),
        };

        let mut txn = test_harness.db_txn().await;
        db::switch::create(&mut txn, &new_switch)
            .await
            .expect("switch should be created");
        txn.commit()
            .await
            .expect("database transaction should commit");
        Self { id }
    }
}

/// An expected-switch fixture identified by its Forge API record.
pub struct TestExpectedSwitch {
    api: Arc<Api>,
    id: rpc::common::Uuid,
}

impl TestExpectedSwitch {
    pub(crate) async fn create(
        test_harness: &TestHarness,
        expected_switch: rpc::forge::ExpectedSwitch,
    ) -> Self {
        let bmc_mac_address = expected_switch.bmc_mac_address.clone();
        let api = test_harness.api_arc();
        api.add_expected_switch(tonic::Request::new(expected_switch))
            .await
            .expect("expected switch should be created");

        let expected_switch = api
            .get_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitchRequest {
                bmc_mac_address,
                expected_switch_id: None,
            }))
            .await
            .expect("created expected switch should be found")
            .into_inner();
        let id = expected_switch
            .expected_switch_id
            .expect("created expected switch should have an id");

        Self { api, id }
    }

    async fn load(&self) -> rpc::forge::ExpectedSwitch {
        self.api
            .get_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitchRequest {
                bmc_mac_address: String::new(),
                expected_switch_id: Some(self.id.clone()),
            }))
            .await
            .expect("expected switch should be found")
            .into_inner()
    }

    /// Creates a switch record from the current expected-switch state.
    pub async fn create_switch(&self, slot_number: i32, tray_index: i32) -> TestSwitch {
        let expected_switch = self.load().await;
        let bmc_mac_address: mac_address::MacAddress = expected_switch
            .bmc_mac_address
            .parse()
            .expect("expected switch BMC MAC address should be valid");
        let name = expected_switch
            .metadata
            .as_ref()
            .map(|metadata| metadata.name.as_str())
            .filter(|name| !name.is_empty())
            .unwrap_or(&expected_switch.switch_serial_number)
            .to_string();
        let id = switch_id::from_hardware_info(
            &expected_switch.switch_serial_number,
            "NVIDIA",
            "Switch",
            SwitchIdSource::ProductBoardChassisSerial,
            SwitchType::NvLink,
        )
        .expect("switch id should be derived from expected switch hardware info");
        let new_switch = NewSwitch {
            id,
            config: SwitchConfig {
                name,
                enable_nmxc: false,
                fabric_manager_config: None,
            },
            bmc_mac_address: Some(bmc_mac_address),
            metadata: None,
            rack_id: expected_switch.rack_id,
            slot_number: Some(slot_number),
            tray_index: Some(tray_index),
        };

        let mut txn = self
            .api
            .database_connection
            .begin()
            .await
            .expect("database transaction should start");
        db::switch::create(&mut txn, &new_switch)
            .await
            .expect("switch should be created");
        txn.commit()
            .await
            .expect("database transaction should commit");
        TestSwitch { id }
    }
}

pub struct TestPowerShelf {
    pub id: PowerShelfId,
}

impl TestPowerShelf {
    pub(crate) async fn create(test_harness: &TestHarness, config: PowerShelfConfig) -> Self {
        let id = power_shelf_id::from_hardware_info(
            &config.name,
            "NVIDIA",
            "PowerShelf",
            PowerShelfIdSource::ProductBoardChassisSerial,
            PowerShelfType::Rack,
        )
        .expect("power shelf id should be derived from test hardware info");
        let new_power_shelf = NewPowerShelf {
            id,
            config,
            bmc_mac_address: None,
            metadata: None,
            rack_id: None,
        };

        let mut txn = test_harness.db_txn().await;
        db::power_shelf::create(&mut txn, &new_power_shelf)
            .await
            .expect("power shelf should be created");
        txn.commit()
            .await
            .expect("database transaction should commit");
        Self { id }
    }
}
