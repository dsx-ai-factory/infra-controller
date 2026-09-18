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

use carbide_api_core::test_support::mac_address_pool::EXPECTED_POWER_SHELF_BMC_MAC_ADDRESS_POOL;
use carbide_api_core::test_support::network_segment::create_static_assignments_segment;
use carbide_test_harness::prelude::*;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::metadata::Metadata;
use sqlx::PgConnection;

pub(crate) async fn create_test_env(pool: PgPool) -> TestHarness {
    let env = TestHarness::builder(pool).build().await;
    let network_controller = env.network_controller();
    let domain = env.test_domain().await;
    network_controller.create_admin_segment(&domain).await;
    network_controller.create_underlay_segment(&domain).await;
    create_static_assignments_segment(env.api(), Some(domain.id)).await;
    env
}

/// Seeds six expected power shelves with the legacy fixture defaults.
pub(crate) async fn create_expected_power_shelves(
    txn: &mut PgConnection,
) -> Vec<ExpectedPowerShelf> {
    let mut created = Vec::new();
    for i in 0..6 {
        let power_shelf = ExpectedPowerShelf {
            expected_power_shelf_id: None,
            bmc_mac_address: EXPECTED_POWER_SHELF_BMC_MAC_ADDRESS_POOL.allocate(),
            serial_number: format!("PS-SN-{:03}", i + 1),
            bmc_username: "ADMIN".into(),
            bmc_password: "Pwd2023x0x0x0x0x7".into(),
            bmc_ip_address: if (3..=4).contains(&i) {
                Some(format!("192.168.1.{}", 100 + i - 3).parse().unwrap())
            } else {
                None
            },
            metadata: Metadata::default(),
            rack_id: None,
            bmc_retain_credentials: None,
        };
        let result = db::expected_power_shelf::create(txn, power_shelf)
            .await
            .expect("unable to create expected power shelf");
        created.push(result);
    }
    created
}
