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

use std::collections::HashMap;

use carbide_test_harness::test_support::mac_address_pool::{
    EXPECTED_SWITCH_BMC_MAC_ADDRESS_POOL, EXPECTED_SWITCH_NVOS_MAC_ADDRESS_POOL,
};
use model::expected_switch::{ExpectedNvosInterface, ExpectedSwitch};
use model::metadata::Metadata;

/// Builds six expected-switch models without creating database records.
///
/// MAC addresses come from the shared fixture pools. Serials and names are fixed
/// (`SW-SN-001` through `SW-SN-006`). Use one set per database to keep switch
/// identities unique.
/// Switches four and five carry explicit NVOS credentials; the other four leave
/// them unset.
pub(crate) fn expected_switch_configs() -> [ExpectedSwitch; 6] {
    std::array::from_fn(|index| ExpectedSwitch {
        expected_switch_id: None,
        bmc_mac_address: EXPECTED_SWITCH_BMC_MAC_ADDRESS_POOL.allocate(),
        nvos_interfaces: vec![ExpectedNvosInterface {
            mac_address: EXPECTED_SWITCH_NVOS_MAC_ADDRESS_POOL.allocate(),
            is_primary: true,
        }],
        serial_number: format!("SW-SN-{:03}", index + 1),
        bmc_username: "ADMIN".into(),
        bmc_password: "Pwd2023x0x0x0x7".into(),
        nvos_username: if (3..=4).contains(&index) {
            Some(format!("nvos_admin{}", index - 2))
        } else {
            None
        },
        nvos_password: if (3..=4).contains(&index) {
            Some(format!("nvos_pass{}", index - 2))
        } else {
            None
        },
        bmc_ip_address: None,
        nvos_ip_address: None,
        metadata: Metadata {
            name: format!("Switch{}", index + 1),
            description: format!("Test Switch {}", index + 1),
            labels: HashMap::new(),
        },
        rack_id: None,
        bmc_retain_credentials: None,
    })
}
