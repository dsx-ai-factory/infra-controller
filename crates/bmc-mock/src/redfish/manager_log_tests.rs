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

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use serde_json::Value;
use tower::ServiceExt;

use crate::test_support::{TestBmcConfig, TestCallbacks, create_test_bmc, host_info};
use crate::{BmcState, HardwareType, MachineRouterOptions, ResourceResetType};

const LOG_SERVICES: &str = "/redfish/v1/Managers/1/LogServices";
const IEL: &str = "/redfish/v1/Managers/1/LogServices/IEL";
const ENTRIES: &str = "/redfish/v1/Managers/1/LogServices/IEL/Entries";

fn bmc(hardware: HardwareType) -> (Router, BmcState<TestCallbacks>) {
    create_test_bmc(
        &host_info(hardware),
        TestBmcConfig::default(),
        "integrated-event-log".into(),
        false,
        MachineRouterOptions::default(),
    )
}

async fn request(router: &Router, method: &str, path: &str) -> (StatusCode, Value) {
    let response = router
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .header("content-type", "application/json")
                .body(Body::from(if method == "POST" { "{}" } else { "" }))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice(&body).unwrap_or(Value::Null))
}

fn messages(entries: &Value) -> Vec<(String, String)> {
    entries["Members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| {
            (
                entry["MessageId"].as_str().unwrap().to_owned(),
                entry["Message"].as_str().unwrap().to_owned(),
            )
        })
        .collect()
}

#[tokio::test]
async fn hpe_integrated_event_log_records_completed_power_commands() {
    let (router, state) = bmc(HardwareType::HpeProliantDl380aGen11);

    let (status, services) = request(&router, "GET", LOG_SERVICES).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(services["Members"][0]["@odata.id"], IEL);
    let (status, service) = request(&router, "GET", IEL).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(service["Id"], "IEL");
    assert_eq!(service["Entries"]["@odata.id"], ENTRIES);
    assert_eq!(
        request(&router, "GET", ENTRIES).await.1["Members@odata.count"],
        0
    );

    for reset_type in [
        ResourceResetType::On,
        ResourceResetType::ForceOn,
        ResourceResetType::GracefulShutdown,
        ResourceResetType::ForceOff,
        ResourceResetType::GracefulRestart,
        ResourceResetType::ForceRestart,
        ResourceResetType::PowerCycle,
        ResourceResetType::Nmi,
        ResourceResetType::PushPowerButton,
    ] {
        state.record_power_command_completed(reset_type);
    }
    let (status, entries) = request(&router, "GET", ENTRIES).await;
    assert_eq!(status, StatusCode::OK);
    let restored = (
        "ResourceEvent.1.3.ResourcePoweredOn".to_owned(),
        "Server power restored.".to_owned(),
    );
    let removed = (
        "ResourceEvent.1.3.ResourcePoweredOff".to_owned(),
        "Server power removed.".to_owned(),
    );
    let reset = (
        "ResourceEvent.1.3.ResourceStateChanged".to_owned(),
        "Server reset.".to_owned(),
    );
    assert_eq!(
        messages(&entries),
        [
            restored.clone(),
            restored,
            removed.clone(),
            removed,
            reset.clone(),
            reset.clone(),
            reset,
        ],
        "iLO wording for each completed command; NMI and the power button log nothing"
    );

    let entry = &entries["Members"][0];
    let (status, fetched) = request(&router, "GET", entry["@odata.id"].as_str().unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(&fetched, entry);
    // NICo asks for entries created after a restart began.
    let (_, future) = request(
        &router,
        "GET",
        &format!("{ENTRIES}?$filter=Created%20ge%20%272999-01-01T00:00:00Z%27"),
    )
    .await;
    assert_eq!(future["Members@odata.count"], 0);

    let (status, _) = request(
        &router,
        "POST",
        &format!("{IEL}/Actions/LogService.ClearLog"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        request(&router, "GET", ENTRIES).await.1["Members@odata.count"],
        0
    );
    assert_eq!(
        request(&router, "GET", &format!("{LOG_SERVICES}/Missing"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn other_managers_keep_no_log_service() {
    let (router, state) = bmc(HardwareType::DellPowerEdgeR750);
    state.record_power_command_completed(ResourceResetType::ForceRestart);
    let manager = state.manager.primary_odata_id().unwrap();
    assert_eq!(
        request(&router, "GET", &format!("{manager}/LogServices"))
            .await
            .0,
        StatusCode::NOT_IMPLEMENTED
    );
    assert_eq!(
        request(
            &router,
            "GET",
            &format!("{manager}/LogServices/IEL/Entries")
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
}
