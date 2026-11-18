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

use axum::body::Body;
use carbide_test_harness::TestMachine as _;
use carbide_uuid::machine::MachineInterfaceId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::rack::RackProfileId;
use http_body_util::BodyExt;
use hyper::http::StatusCode;
use model::allocation_type::AllocationType;
use model::site_explorer::{Inventory, Service};
use model::test_support::power_shelf_config;
use tower::ServiceExt;

use crate::tests::env::TestEnv;
use crate::tests::{make_test_app, web_request_builder};

#[crate::sqlx_test]
async fn test_component_detail_pages_return_not_found_for_missing_resources(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);

    let rack_id = env
        .test_harness
        .create_rack(RackProfileId::new("rack"))
        .await
        .id;
    let switch_id = env.test_harness.create_switch(1, 1).await.id;
    let power_shelf_id = env
        .test_harness
        .create_power_shelf(power_shelf_config("Component Detail Test Power Shelf"))
        .await
        .id;

    for route in [
        format!("/admin/rack/{rack_id}"),
        format!("/admin/rack/{rack_id}.json"),
        format!("/admin/switch/{switch_id}"),
        format!("/admin/switch/{switch_id}.json"),
        format!("/admin/power-shelf/{power_shelf_id}"),
        format!("/admin/power-shelf/{power_shelf_id}.json"),
    ] {
        let response = app
            .clone()
            .oneshot(
                web_request_builder()
                    .uri(&route)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{route}");
    }

    for route in [
        "/admin/rack/missing-rack",
        "/admin/rack/missing-rack.json",
        "/admin/switch/sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
        "/admin/switch/sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0.json",
        "/admin/power-shelf/ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0",
        "/admin/power-shelf/ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0.json",
    ] {
        let response = app
            .clone()
            .oneshot(
                web_request_builder()
                    .uri(route)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{route}");
    }
}

#[crate::sqlx_test]
async fn test_component_firmware_inventory_matches_bmc(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let (host, host_data) = env.create_ready_managed_host(1).await;
    let switch = env.test_harness.create_switch(1, 1).await;
    let shelf = env
        .test_harness
        .create_power_shelf(power_shelf_config("Firmware Inventory Shelf"))
        .await;

    let mut txn = env.test_harness.db_txn().await;
    let segment_id: NetworkSegmentId = sqlx::query_scalar(
        "INSERT INTO network_segments (name, version, network_segment_type)
         VALUES ('firmware-bmcs', 'V1-T0', 'underlay') RETURNING id",
    )
    .fetch_one(txn.as_mut())
    .await
    .unwrap();
    let switch_interface: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
            (switch_id, association_type, segment_id, mac_address,
             primary_interface, hostname, interface_type)
         VALUES ($1, 'Switch', $2, '02:00:00:00:0b:01', false, 'switch-bmc', 'Bmc')
         RETURNING id",
    )
    .bind(switch.id)
    .bind(segment_id)
    .fetch_one(txn.as_mut())
    .await
    .unwrap();
    let shelf_interface: MachineInterfaceId = sqlx::query_scalar(
        "INSERT INTO machine_interfaces
            (power_shelf_id, association_type, segment_id, mac_address,
             primary_interface, hostname, interface_type)
         VALUES ($1, 'PowerShelf', $2, '02:00:00:00:0b:02', false, 'shelf-bmc', 'Bmc')
         RETURNING id",
    )
    .bind(shelf.id)
    .bind(segment_id)
    .fetch_one(txn.as_mut())
    .await
    .unwrap();
    let switch_ip = "192.0.2.10".parse().unwrap();
    let shelf_ip = "2001:db8::20".parse().unwrap();
    for (interface, ip) in [(switch_interface, switch_ip), (shelf_interface, shelf_ip)] {
        db::machine_interface_address::insert(txn.as_mut(), interface, ip, AllocationType::Dhcp)
            .await
            .unwrap();
    }

    let cases = [
        (
            format!("/admin/machine/{}", host.host.id()),
            host_data.host_bmc_ip(),
            "host-fw",
        ),
        (
            format!("/admin/machine/{}", host.dpus[0].id()),
            host_data.first_dpu_bmc_ip(),
            "dpu-fw",
        ),
        (
            format!("/admin/switch/{}", switch.id),
            switch_ip,
            "switch-fw",
        ),
        (
            format!("/admin/power-shelf/{}", shelf.id),
            shelf_ip,
            "shelf-fw",
        ),
    ];
    for (_, ip, version) in &cases {
        db::explored_endpoints::insert(*ip, &Default::default(), false, &mut txn)
            .await
            .unwrap();
        let endpoint = db::explored_endpoints::find_by_ips(txn.as_mut(), vec![*ip])
            .await
            .unwrap()
            .remove(0);
        let mut report = endpoint.report;
        report.service = vec![
            Service {
                id: "SoftwareInventory".to_string(),
                inventories: vec![Inventory {
                    id: "not-firmware".to_string(),
                    ..Default::default()
                }],
            },
            Service {
                id: "FirmwareInventory".to_string(),
                inventories: vec![
                    Inventory {
                        id: "Z-no-version".to_string(),
                        ..Default::default()
                    },
                    Inventory {
                        id: "BMC".to_string(),
                        description: Some("Controller <firmware>".to_string()),
                        version: Some(version.to_string()),
                        release_date: Some("2026-01-01".to_string()),
                        ..Default::default()
                    },
                ],
            },
        ];
        assert!(matches!(
            db::explored_endpoints::try_update(
                *ip,
                endpoint.report_version,
                &report,
                false,
                &mut txn
            )
            .await
            .unwrap(),
            db::ConditionalWrite::Applied(())
        ));
    }
    txn.commit().await.unwrap();

    for (route, _, version) in &cases {
        let response = app
            .clone()
            .oneshot(
                web_request_builder()
                    .uri(route)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{route}");
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = std::str::from_utf8(&body).unwrap();
        let inventory = body
            .split_once("<details id=\"firmware-inventory\">")
            .unwrap()
            .1;
        let inventory = inventory.split_once("</details>").unwrap().0;
        assert!(
            inventory.contains("Firmware Inventory · 2 components</summary>"),
            "{route}"
        );
        assert!(inventory.contains("<th>Version</th>"), "{route}");
        assert!(
            inventory.contains(&format!("<td>{version}</td>")),
            "{route}"
        );
        assert!(
            inventory.contains("Controller &#60;firmware&#62;"),
            "{route}"
        );
        assert!(inventory.contains("2026-01-01"), "{route}");
        assert!(
            inventory.find("BMC").unwrap() < inventory.find("Z-no-version").unwrap(),
            "{route}"
        );
        assert!(inventory.contains("<td></td>"), "{route}");
        assert!(!inventory.contains("not-firmware"), "{route}");
        for (_, _, other_version) in &cases {
            if version != other_version {
                assert!(!inventory.contains(other_version), "{route}");
            }
        }
    }
}
