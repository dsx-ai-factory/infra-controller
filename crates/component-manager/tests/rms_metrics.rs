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

// Keep this check in its own test target: RED metrics are process-global,
// and parallel RMS unit tests record observations with the same labels.

use std::sync::Arc;

use api_test_helper::mock_rms::MockRmsApi;
use carbide_instrument::testing::MetricsCapture;
use carbide_secrets::credentials::Credentials;
use carbide_uuid::rack::{RackId, RackProfileId};
use component_manager::power_shelf_manager::{
    PowerShelfEndpoint, PowerShelfManager, PowerShelfVendor,
};
use component_manager::rms::RmsBackend;
use librms::RackManagerError;
use librms::protos::rack_manager as rms;
use model::expected_power_shelf::ExpectedPowerShelf;
use model::rack_type::{
    RackCapabilitiesSet, RackCapabilityPowerShelf, RackProductFamily, RackProfile,
    RackProfileConfig,
};

#[carbide_macros::sqlx_test]
async fn rms_calls_record_the_external_call_histogram_by_outcome(pool: sqlx::PgPool) {
    let device_mac = "AA:BB:CC:DD:EE:01".parse().unwrap();
    let rack_id = RackId::new(uuid::Uuid::new_v4().to_string());
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
    db::expected_power_shelf::create(
        &mut txn,
        ExpectedPowerShelf {
            bmc_mac_address: device_mac,
            serial_number: "PS-001".to_owned(),
            rack_id: Some(rack_id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let profiles = RackProfileConfig {
        rack_profiles: [(
            profile_id.to_string(),
            RackProfile {
                product_family: Some(RackProductFamily::Gb200),
                rack_capabilities: RackCapabilitiesSet {
                    power_shelf: RackCapabilityPowerShelf {
                        count: 1,
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
    let backend = RmsBackend::new(mock, None, pool, Arc::new(profiles), false);
    let endpoints = [PowerShelfEndpoint {
        pmc_ip: "10.0.0.1".parse().unwrap(),
        pmc_mac: device_mac,
        pmc_vendor: PowerShelfVendor::Liteon,
        pmc_credentials: Credentials::UsernamePassword {
            username: "admin".to_owned(),
            password: "pass".to_owned(),
        },
    }];

    let metrics = MetricsCapture::start();
    let mut observed = Vec::new();
    for _ in 0..2 {
        observed.push(backend.get_power_state(&endpoints).await.unwrap());
    }
    assert!(
        observed[1][0].power_state.is_err(),
        "transport failure surfaces as an error"
    );

    for outcome in ["ok", "error"] {
        assert_eq!(
            metrics.histogram_count_delta(
                "carbide_external_call_duration_milliseconds",
                &[
                    ("backend", "rms"),
                    ("operation", "batch_get_power_state"),
                    ("outcome", outcome),
                ],
            ),
            1,
            "{outcome}"
        );
    }
}
