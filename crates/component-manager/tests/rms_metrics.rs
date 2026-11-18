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

// Keep these checks in their own test target: RED metrics are process-global,
// and parallel RMS unit tests record observations with the same labels.

use std::sync::Arc;

use api_test_helper::mock_rms::MockRmsApi;
use carbide_instrument::testing::MetricsCapture;
use carbide_secrets::credentials::Credentials;
use carbide_uuid::rack::{RackId, RackProfileId};
use component_manager::firmware_inventory::FirmwareReadError;
use component_manager::power_shelf_manager::{
    PowerShelfEndpoint, PowerShelfManager, PowerShelfVendor,
};
use component_manager::rms::{RmsBackend, rms_rack_firmware_update_manager};
use component_manager::{
    FirmwareExpectationError, RackFirmwareExpectationsRequest, RackFirmwareUpdateManager,
};
use librms::RackManagerError;
use librms::protos::rack_manager as rms;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::rack::FirmwareUpgradeDeviceInfo;
use model::rack_type::{
    RackCapabilitiesSet, RackCapabilityPowerShelf, RackProductFamily, RackProfile,
    RackProfileConfig,
};

/// Counts each outbound batch once, including a locally expired firmware RPC,
/// so a new reader cannot silently omit or duplicate RED observations.
#[carbide_macros::sqlx_test]
async fn rms_calls_record_the_external_call_histogram_by_outcome(pool: sqlx::PgPool) {
    let device_mac = "AA:BB:CC:DD:EE:01".parse().unwrap();
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
    let peer_mac = "AA:BB:CC:DD:EE:02".parse().unwrap();
    let profile_id = RackProfileId::new("rms-metrics");
    let mut txn = pool.begin().await.unwrap();
    db::rack::create(
        &mut txn,
        &rack_id,
        Some(&profile_id),
        &Default::default(),
        None,
    )
    .await
    .unwrap();
    for (mac, serial) in [(device_mac, "PS-001"), (peer_mac, "PS-002")] {
        db::expected_power_shelf::create(
            &mut txn,
            ExpectedPowerShelf {
                bmc_mac_address: mac,
                serial_number: serial.to_owned(),
                rack_id: Some(rack_id.clone()),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    txn.commit().await.unwrap();

    let profiles = RackProfileConfig {
        rack_profiles: [(
            profile_id.to_string(),
            RackProfile {
                product_family: Some(RackProductFamily::Gb200),
                rack_capabilities: RackCapabilitiesSet {
                    power_shelf: RackCapabilityPowerShelf {
                        count: 2,
                        vendor: Some("LiteOn".to_owned()),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        )]
        .into_iter()
        .collect(),
    };
    let mock = Arc::new(MockRmsApi::new());
    mock.enqueue_batch_get_power_state(Ok(rms::BatchGetPowerStateResponse::default()))
        .await;
    mock.enqueue_batch_get_power_state(Err(RackManagerError::ApiInvocationError(
        tonic::Status::unavailable("down"),
    )))
    .await;
    let backend = RmsBackend::new(mock.clone(), None, pool, Arc::new(profiles), false);
    let mut endpoints = vec![PowerShelfEndpoint {
        pmc_ip: "10.0.0.1".parse().unwrap(),
        pmc_mac: device_mac,
        pmc_vendor: PowerShelfVendor::Liteon,
        pmc_credentials: Credentials::UsernamePassword {
            username: "admin".to_owned(),
            password: "pass".to_owned(),
        },
    }];

    let mut peer = endpoints[0].clone();
    peer.pmc_mac = peer_mac;
    peer.pmc_ip = "10.0.0.2".parse().unwrap();
    endpoints.push(peer);

    let metrics = MetricsCapture::start();
    let mut observed = Vec::new();
    for _ in 0..2 {
        observed.push(backend.get_power_state(&endpoints).await.unwrap());
    }
    assert!(
        observed[1][0].power_state.is_err(),
        "transport failure surfaces as an error"
    );

    // RED `ok` means the transport completed, not that inventory evidence is
    // valid. An absent envelope still fails both endpoint observations.
    mock.enqueue_batch_get_firmware_inventory(
        Ok(rms::BatchGetFirmwareInventoryResponse::default()),
    )
    .await;
    mock.enqueue_batch_get_firmware_inventory(Err(tonic::Status::unavailable("down").into()))
        .await;
    for _ in 0..2 {
        let observed = backend.get_firmware_inventory(&endpoints).await.unwrap();
        assert_eq!(observed.len(), 2);
        assert!(
            observed
                .iter()
                .all(|result| result.inventory == Err(FirmwareReadError::ReadFailed))
        );
    }
    let (arrival, release) = mock.block_next_batch_get_firmware_inventory().await;
    let read = backend.get_firmware_inventory(&endpoints);
    tokio::pin!(read);
    tokio::select! {
        arrived = tokio::time::timeout(std::time::Duration::from_secs(10), arrival) => arrived.unwrap().unwrap(),
        _ = &mut read => panic!("firmware read must block"),
    }

    // SQL preparation has completed before the gate. Advance only the owned
    // RPC deadline; virtual time cannot stand in for a database timeout.
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    let timed_out = tokio::time::timeout(std::time::Duration::from_secs(120), read)
        .await
        .unwrap()
        .unwrap();
    tokio::time::resume();
    assert_eq!(timed_out.len(), 2);
    assert!(
        timed_out
            .iter()
            .all(|result| result.inventory == Err(FirmwareReadError::DeadlineExceeded))
    );
    assert!(release.is_closed());
    let calls = mock.batch_get_firmware_inventory_calls().await;
    assert_eq!(calls.len(), 3);
    for call in calls {
        let nodes = &call.nodes.as_ref().unwrap().nodes;
        assert_eq!(nodes.len(), 2);
        assert!(nodes.iter().all(|node| node.rack_id == rack_id.to_string()));
        assert_eq!(
            nodes
                .iter()
                .map(|node| node.node_id.as_str())
                .collect::<Vec<_>>(),
            ["AA:BB:CC:DD:EE:01", "AA:BB:CC:DD:EE:02"]
        );
    }

    for (operation, outcome, count) in [
        ("batch_get_power_state", "ok", 1),
        ("batch_get_power_state", "error", 1),
        ("batch_get_firmware_inventory", "ok", 1),
        ("batch_get_firmware_inventory", "error", 2),
    ] {
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "rms"),
                    ("operation", operation),
                    ("outcome", outcome),
                ],
            ),
            count,
            "{operation} {outcome}"
        );
    }
}

/// Counts comparison RPCs rather than devices and rejects a reply at the deadline.
/// A later unsent batch must not add a transport observation.
#[tokio::test(start_paused = true)]
async fn firmware_comparison_metrics_count_batches_and_local_timeouts() {
    let rack_id = RackId::new("firmware-comparison-metrics");
    let profile = RackProfile {
        product_family: Some(RackProductFamily::Gb200),
        rack_capabilities: RackCapabilitiesSet {
            power_shelf: RackCapabilityPowerShelf {
                count: 65,
                vendor: Some("LiteOn".to_owned()),
                ..Default::default()
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let shelves: Vec<_> = (0..65)
        .map(|slot| FirmwareUpgradeDeviceInfo {
            node_id: format!("shelf-{slot:03}"),
            mac: format!("02:00:00:00:00:{slot:02x}"),
            bmc_ip: format!("192.0.2.{}", slot + 1),
            bmc_username: "admin".to_owned(),
            bmc_password: "test-password".to_owned(),
            // Shelves use their PMC endpoint without switch OS connection data.
            os_mac: None,
            os_ip: None,
            os_username: None,
            os_password: None,
            os_hostname: None,
        })
        .collect();
    let request = || RackFirmwareExpectationsRequest {
        rack_id: &rack_id,
        profile: &profile,
        config_json: "{}",
        components: &[],
        machines: &[],
        switches: &[],
        power_shelves: &shelves[..2],
    };
    let mock = Arc::new(MockRmsApi::new());
    let manager = rms_rack_firmware_update_manager(mock.clone());
    let metrics = MetricsCapture::start();

    // RED describes the RPC, so an unusable payload still counts as transport
    // success. Each call includes both shelves but contributes one observation.
    mock.enqueue_compare_firmware_object(Ok(rms::CompareFirmwareObjectResponse::default()))
        .await;
    let unusable = manager.get_firmware_expectations(request()).await.unwrap();
    assert_eq!(unusable.len(), 2);
    assert!(
        unusable
            .iter()
            .all(|device| device.expectations == Err(FirmwareExpectationError::ReadFailed))
    );
    mock.enqueue_compare_firmware_object(Err(tonic::Status::unavailable("down").into()))
        .await;
    let failed = manager.get_firmware_expectations(request()).await.unwrap();
    assert_eq!(failed.len(), 2);
    assert!(
        failed
            .iter()
            .all(|device| device.expectations == Err(FirmwareExpectationError::ReadFailed))
    );

    mock.enqueue_compare_firmware_object(Ok(rms::CompareFirmwareObjectResponse::default()))
        .await;
    let (arrival, release) = mock.block_next_compare_firmware_object().await;
    // Only the final attempt selects all 65 devices. Its first batch expires;
    // the remaining device must not cause another RPC or metric observation.
    let mut full_request = request();
    full_request.power_shelves = &shelves;
    let read = manager.get_firmware_expectations(full_request);
    tokio::pin!(read);
    tokio::select! {
        arrived = tokio::time::timeout(std::time::Duration::from_secs(10), arrival) => arrived.unwrap().unwrap(),
        _ = &mut read => panic!("comparison must remain blocked at the RPC gate"),
    }

    // The clock was paused before deadline creation, so this reaches the exact
    // boundary rather than merely passing it after real setup time.
    tokio::time::advance(std::time::Duration::from_secs(30)).await;
    // Both the response and timer will be ready on the next poll. Tokio polls
    // the response first, so the reader must check expiry before RED records it.
    assert!(!release.is_closed());
    release.send(()).unwrap();
    let timed_out = tokio::time::timeout(std::time::Duration::from_secs(120), read)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(timed_out.len(), 65);
    assert_eq!(
        timed_out
            .iter()
            .map(|device| device.node_id.as_str())
            .collect::<Vec<_>>(),
        shelves
            .iter()
            .map(|device| device.node_id.as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        timed_out.iter().all(|device| {
            device.expectations == Err(FirmwareExpectationError::DeadlineExceeded)
        })
    );
    let calls = mock.compare_firmware_object_calls().await;
    assert_eq!(calls.len(), 3);
    assert_eq!(
        calls
            .iter()
            .map(|call| call.nodes.as_ref().unwrap().nodes.len())
            .collect::<Vec<_>>(),
        vec![2, 2, 64]
    );
    for (outcome, count) in [("ok", 1), ("error", 2)] {
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "rms"),
                    ("operation", "compare_firmware_object"),
                    ("outcome", outcome),
                ],
            ),
            count,
            "comparison {outcome}"
        );
    }
}
