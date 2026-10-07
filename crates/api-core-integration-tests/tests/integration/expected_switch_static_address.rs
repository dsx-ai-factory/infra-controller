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

use carbide_api_core::test_support::network_segment::create_static_assignments_segment;
use carbide_instrument::testing::MetricsCapture;
use carbide_test_harness::prelude::*;
use mac_address::MacAddress;
use rpc::forge::forge_server::Forge;

#[sqlx_test]
async fn test_update_with_bmc_and_nvos_ips_preallocates_both_interfaces(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = TestHarness::builder(pool).build().await;
    let network_controller = env.network_controller();
    let domain = env.test_domain().await;
    network_controller.create_admin_segment(&domain).await;
    network_controller.create_underlay_segment(&domain).await;
    create_static_assignments_segment(env.api(), Some(domain.id)).await;

    let bmc_mac: MacAddress = "6A:6B:6C:6D:6E:90".parse()?;
    let nvos_mac: MacAddress = "7A:7B:7C:7D:7E:90".parse()?;
    let bmc_ip = "192.0.2.190";
    let nvos_ip = "192.0.2.191";

    env.api()
        .add_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            nvos_mac_addresses: vec![nvos_mac.to_string()],
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-STATIC-UPDATE-001".into(),
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await?;

    let metrics = MetricsCapture::start();

    env.api()
        .update_expected_switch(tonic::Request::new(rpc::forge::ExpectedSwitch {
            bmc_mac_address: bmc_mac.to_string(),
            nvos_mac_addresses: vec![nvos_mac.to_string()],
            bmc_username: "ADMIN".into(),
            bmc_password: "PASS".into(),
            switch_serial_number: "SW-STATIC-UPDATE-001".into(),
            bmc_ip_address: bmc_ip.into(),
            nvos_ip_address: Some(nvos_ip.into()),
            metadata: Some(rpc::forge::Metadata::default()),
            ..Default::default()
        }))
        .await?;

    let mut txn = env.api().database_connection.begin().await?;
    for (mac, ip) in [(bmc_mac, bmc_ip), (nvos_mac, nvos_ip)] {
        let interfaces = db::machine_interface::find_by_mac_address(&mut *txn, mac).await?;
        assert_eq!(interfaces.len(), 1, "should create one interface for {mac}");
        assert!(
            interfaces[0].addresses.contains(&ip.parse()?),
            "interface {mac} should contain static address {ip}"
        );
    }
    txn.rollback().await?;

    // This update creates two interfaces and therefore emits two `created`
    // outcomes. Other tests share the global metrics registry and can only
    // increase this delta, so use a lower bound rather than exact equality.
    assert!(
        metrics.counter_delta(
            "carbide_static_address_preallocations_total",
            &[("outcome", "created")],
        ) >= 2.0
    );

    Ok(())
}
