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
use carbide_uuid::rack::RackProfileId;
use db::health_history::HealthHistoryTableId;
use health_report::HealthReport;
use http_body_util::BodyExt;
use hyper::http::{Method, StatusCode};
use model::test_support::power_shelf_config;
use rpc::forge::AdminForceDeleteMachineRequest;
use rpc::forge::forge_server::Forge;
use tower::ServiceExt;

use crate::tests::env::TestEnv;
use crate::tests::{make_test_app, web_request_builder};

#[crate::sqlx_test]
async fn test_health_of_nonexisting_machine(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);

    // Health page for Machine which was never ingested
    verify_health_page_not_found(
        &app,
        "/admin/machine/fm100ht09g4atrqgjb0b83b2to1qa1hfugks9mhutb0umcng1rkr54vliqg/health",
    )
    .await;

    // Health page for Machine which was force deleted
    let mh = env.create_ready_managed_host(1).await.0;
    let host_machine_id = mh.host.id;
    env.api()
        .admin_force_delete_machine(tonic::Request::new(AdminForceDeleteMachineRequest {
            host_query: host_machine_id.to_string(),
            delete_interfaces: false,
            delete_bmc_interfaces: false,
            delete_bmc_credentials: false,
            allow_delete_with_orphaned_dpf_crds: false,
            delete_bmc_suppressions: false,
            delete_retained_boot_interfaces: false,
            allow_delete_with_instance_type: false,
            allow_delete_with_instance: false,
        }))
        .await
        .unwrap()
        .into_inner();

    assert!(
        env.test_harness
            .find_machine(host_machine_id.into())
            .await
            .is_empty()
    );

    verify_health_page_not_found(&app, &format!("/admin/machine/{host_machine_id}/health")).await;

    let response = app
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/machine/{host_machine_id}/health-history"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    assert!(String::from_utf8_lossy(&body_bytes).contains("History"));
}

#[crate::sqlx_test]
async fn test_health_of_nonexisting_components(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);

    for path in [
        "/admin/rack/missing-rack/health",
        "/admin/power-shelf/ps100htjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0/health",
        "/admin/switch/sw100ntjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0/health",
        "/admin/nvlink-domain/00000000-0000-0000-0000-000000000001/health",
    ] {
        verify_health_page_not_found(&app, path).await;
    }
}

#[crate::sqlx_test]
async fn test_add_remove_health_report_via_web_ui(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let mh = env.create_ready_managed_host(1).await.0;
    let host_machine_id = mh.host.id;

    let payload = r#"{
        "mode": "Merge",
        "health_report": {
            "source": "web-health-test",
            "triggered_by": null,
            "observed_at": null,
            "successes": [],
            "alerts": []
        }
    }"#;

    let machine_id = host_machine_id.to_string();
    let status = post_machine_health_report_with_request_context(
        &app,
        &machine_id,
        "add-report",
        payload,
        "cross-site",
        "https://attacker.example",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let body = get_machine_health_page(&app, &machine_id).await;
    assert!(!body.contains("web-health-test"));

    post_machine_health_report(&app, &machine_id, "add-report", payload).await;

    let body = get_machine_health_page(&app, &machine_id).await;
    assert!(body.contains("web-health-test"));

    let status = post_machine_health_report_with_request_context(
        &app,
        &machine_id,
        "remove-report",
        r#"{"source":"web-health-test"}"#,
        "cross-site",
        "https://attacker.example",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    let body = get_machine_health_page(&app, &machine_id).await;
    assert!(body.contains("web-health-test"));

    post_machine_health_report(
        &app,
        &machine_id,
        "remove-report",
        r#"{"source":"web-health-test"}"#,
    )
    .await;

    let body = get_machine_health_page(&app, &machine_id).await;
    assert!(!body.contains("web-health-test"));
}

#[crate::sqlx_test]
async fn test_add_remove_nvlink_domain_health_report_via_web_ui(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let domain_id = "00000000-0000-0000-0000-000000000001";

    let payload = r#"{
        "mode": "Merge",
        "health_report": {
            "source": "web-nvlink-domain-health-test",
            "triggered_by": null,
            "observed_at": null,
            "successes": [],
            "alerts": [{
                "id": "NvLinkDomainWebHealth",
                "target": null,
                "in_alert_since": null,
                "message": "nvlink domain web health",
                "tenant_message": null,
                "classifications": ["PreventAllocations"]
            }]
        }
    }"#;

    post_nvlink_domain_health_report(&app, domain_id, "add-report", payload).await;

    let body = get_nvlink_domain_health_page(&app, domain_id).await;
    let aggregate_health = aggregate_health_section(&body);
    assert!(aggregate_health.contains("NvLinkDomainWebHealth"));
    assert!(aggregate_health.contains("nvlink domain web health"));
    assert!(body.contains("web-nvlink-domain-health-test"));

    post_nvlink_domain_health_report(
        &app,
        domain_id,
        "remove-report",
        r#"{"source":"web-nvlink-domain-health-test"}"#,
    )
    .await;

    let body = get_nvlink_domain_health_page(&app, domain_id).await;
    assert!(!body.contains("web-nvlink-domain-health-test"));
}

#[crate::sqlx_test]
async fn test_add_replace_remove_dpu_health_report_via_web_ui(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let mh = env.create_ready_managed_host(1).await.0;
    let host_machine_id = mh.host.id;
    let dpu_machine_id = mh.dpu(0).id;

    let merge_payload = r#"{
        "mode": "Merge",
        "health_report": {
            "source": "web-dpu-merge-health-test",
            "triggered_by": null,
            "observed_at": null,
            "successes": [],
            "alerts": [{
                "id": "DpuMergeWebHealth",
                "target": null,
                "in_alert_since": null,
                "message": "dpu merge web health",
                "tenant_message": null,
                "classifications": ["PreventAllocations"]
            }]
        }
    }"#;
    post_machine_health_report(
        &app,
        &dpu_machine_id.to_string(),
        "add-report",
        merge_payload,
    )
    .await;

    let dpu_health = get_machine_health_page(&app, &dpu_machine_id.to_string()).await;
    let dpu_aggregate_health = aggregate_health_section(&dpu_health);
    assert!(dpu_aggregate_health.contains("DpuMergeWebHealth"));
    assert!(dpu_aggregate_health.contains("dpu merge web health"));

    let host_health = get_machine_health_page(&app, &host_machine_id.to_string()).await;
    let host_aggregate_health = aggregate_health_section(&host_health);
    assert!(host_aggregate_health.contains("DpuMergeWebHealth"));
    assert!(host_aggregate_health.contains("dpu merge web health"));

    let replace_payload = r#"{
        "mode": "Replace",
        "health_report": {
            "source": "web-dpu-replace-health-test",
            "triggered_by": null,
            "observed_at": null,
            "successes": [],
            "alerts": [{
                "id": "DpuReplaceWebHealth",
                "target": null,
                "in_alert_since": null,
                "message": "dpu replace web health",
                "tenant_message": null,
                "classifications": ["PreventAllocations"]
            }]
        }
    }"#;
    post_machine_health_report(
        &app,
        &dpu_machine_id.to_string(),
        "add-report",
        replace_payload,
    )
    .await;

    let dpu_health = get_machine_health_page(&app, &dpu_machine_id.to_string()).await;
    let dpu_aggregate_health = aggregate_health_section(&dpu_health);
    assert!(dpu_aggregate_health.contains("DpuReplaceWebHealth"));
    assert!(dpu_aggregate_health.contains("dpu replace web health"));
    assert!(!dpu_aggregate_health.contains("DpuMergeWebHealth"));
    assert!(!dpu_aggregate_health.contains("dpu merge web health"));

    let host_health = get_machine_health_page(&app, &host_machine_id.to_string()).await;
    let host_aggregate_health = aggregate_health_section(&host_health);
    assert!(host_aggregate_health.contains("DpuReplaceWebHealth"));
    assert!(host_aggregate_health.contains("dpu replace web health"));
    assert!(!host_aggregate_health.contains("DpuMergeWebHealth"));
    assert!(!host_aggregate_health.contains("dpu merge web health"));

    post_machine_health_report(
        &app,
        &dpu_machine_id.to_string(),
        "remove-report",
        r#"{"source":"web-dpu-replace-health-test"}"#,
    )
    .await;

    let dpu_health = get_machine_health_page(&app, &dpu_machine_id.to_string()).await;
    let dpu_aggregate_health = aggregate_health_section(&dpu_health);
    assert!(dpu_aggregate_health.contains("DpuMergeWebHealth"));
    assert!(!dpu_aggregate_health.contains("DpuReplaceWebHealth"));

    let host_health = get_machine_health_page(&app, &host_machine_id.to_string()).await;
    let host_aggregate_health = aggregate_health_section(&host_health);
    assert!(host_aggregate_health.contains("DpuMergeWebHealth"));
    assert!(!host_aggregate_health.contains("DpuReplaceWebHealth"));

    post_machine_health_report(
        &app,
        &dpu_machine_id.to_string(),
        "remove-report",
        r#"{"source":"web-dpu-merge-health-test"}"#,
    )
    .await;

    let dpu_health = get_machine_health_page(&app, &dpu_machine_id.to_string()).await;
    let dpu_aggregate_health = aggregate_health_section(&dpu_health);
    assert!(!dpu_aggregate_health.contains("DpuMergeWebHealth"));
    assert!(!dpu_aggregate_health.contains("DpuReplaceWebHealth"));
    assert!(!dpu_health.contains("web-dpu-merge-health-test"));
    assert!(!dpu_health.contains("web-dpu-replace-health-test"));

    let host_health = get_machine_health_page(&app, &host_machine_id.to_string()).await;
    let host_aggregate_health = aggregate_health_section(&host_health);
    assert!(!host_aggregate_health.contains("DpuMergeWebHealth"));
    assert!(!host_aggregate_health.contains("DpuReplaceWebHealth"));
}

#[crate::sqlx_test]
async fn test_health_of_rack(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let rack_id = env
        .test_harness
        .create_rack(RackProfileId::new("rack"))
        .await
        .id;

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/rack/{rack_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(body.contains("Rack Health"));
    assert!(body.contains("Health Report Management"));
    assert!(body.contains("Health History"));
    assert!(body.contains(&format!("/admin/rack/{rack_id}/health-history")));

    let payload = r#"{
        "mode": "Merge",
        "health_report": {
            "source": "web-rack-health-test",
            "triggered_by": null,
            "observed_at": null,
            "successes": [],
            "alerts": [{
                "id": "RackWebHealth",
                "target": null,
                "in_alert_since": null,
                "message": "rack web health",
                "tenant_message": null,
                "classifications": ["PreventAllocations"]
            }]
        }
    }"#;
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!("/admin/rack/{rack_id}/health/add-report"))
                .header("Content-Type", "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/rack/{rack_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(body.contains("web-rack-health-test"));
    assert!(body.contains("rack web health"));

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!("/admin/rack/{rack_id}/health/remove-report"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"source":"web-rack-health-test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/rack/{rack_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(!body.contains("web-rack-health-test"));
}

#[crate::sqlx_test]
async fn test_health_of_switch(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let switch_id = env.test_harness.create_switch(1, 1).await.id;

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/switch/{switch_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(body.contains("Switch Health"));
    assert!(body.contains("Health Report Management"));
    assert!(body.contains("Health History"));
    assert!(body.contains(&format!("/admin/switch/{switch_id}/health-history")));

    let payload = r#"{
        "mode": "Merge",
        "health_report": {
            "source": "web-switch-health-test",
            "triggered_by": null,
            "observed_at": null,
            "successes": [],
            "alerts": [{
                "id": "SwitchWebHealth",
                "target": null,
                "in_alert_since": null,
                "message": "switch web health",
                "tenant_message": null,
                "classifications": ["PreventAllocations"]
            }]
        }
    }"#;
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!("/admin/switch/{switch_id}/health/add-report"))
                .header("Content-Type", "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/switch/{switch_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(body.contains("web-switch-health-test"));
    assert!(body.contains("switch web health"));

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!("/admin/switch/{switch_id}/health/remove-report"))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"source":"web-switch-health-test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/switch/{switch_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(!body.contains("web-switch-health-test"));
}

#[crate::sqlx_test]
async fn test_health_of_power_shelf(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let power_shelf_id = env
        .test_harness
        .create_power_shelf(power_shelf_config("Power Shelf Health Test"))
        .await
        .id;

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/power-shelf/{power_shelf_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(body.contains("Power Shelf Health"));
    assert!(body.contains("Health Report Management"));
    assert!(body.contains("Health History"));
    assert!(body.contains(&format!(
        "/admin/power-shelf/{power_shelf_id}/health-history"
    )));

    let payload = r#"{
        "mode": "Merge",
        "health_report": {
            "source": "web-power-shelf-health-test",
            "triggered_by": null,
            "observed_at": null,
            "successes": [],
            "alerts": [{
                "id": "PowerShelfWebHealth",
                "target": null,
                "in_alert_since": null,
                "message": "power shelf web health",
                "tenant_message": null,
                "classifications": ["PreventAllocations"]
            }]
        }
    }"#;
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!(
                    "/admin/power-shelf/{power_shelf_id}/health/add-report"
                ))
                .header("Content-Type", "application/json")
                .body(Body::from(payload))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/power-shelf/{power_shelf_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(body.contains("web-power-shelf-health-test"));
    assert!(body.contains("power shelf web health"));

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!(
                    "/admin/power-shelf/{power_shelf_id}/health/remove-report"
                ))
                .header("Content-Type", "application/json")
                .body(Body::from(r#"{"source":"web-power-shelf-health-test"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let response = app
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/power-shelf/{power_shelf_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(!body.contains("web-power-shelf-health-test"));
}

#[crate::sqlx_test]
async fn test_health_history_pages(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool.clone()).await;
    let app = make_test_app(&env.test_harness);

    let rack_id = env
        .test_harness
        .create_rack(RackProfileId::new("rack"))
        .await
        .id
        .to_string();
    let switch_id = env.test_harness.create_switch(1, 1).await.id.to_string();
    let power_shelf_id = env
        .test_harness
        .create_power_shelf(power_shelf_config("Power Shelf History Test"))
        .await
        .id
        .to_string();

    // Seed one health-history record per object directly through the persistence
    // layer (there is no controller loop in these tests) so we exercise the full
    // read path: RPC -> handler -> fetch helper -> template.
    seed_health_history(
        &pool,
        HealthHistoryTableId::Rack,
        &rack_id,
        "web-rack-history-probe",
    )
    .await;
    seed_health_history(
        &pool,
        HealthHistoryTableId::Switch,
        &switch_id,
        "web-switch-history-probe",
    )
    .await;
    seed_health_history(
        &pool,
        HealthHistoryTableId::PowerShelf,
        &power_shelf_id,
        "web-power-shelf-history-probe",
    )
    .await;

    for (path, title, source) in [
        (
            format!("/admin/rack/{rack_id}/health-history"),
            "Rack Health History",
            "web-rack-history-probe",
        ),
        (
            format!("/admin/switch/{switch_id}/health-history"),
            "Switch Health History",
            "web-switch-history-probe",
        ),
        (
            format!("/admin/power-shelf/{power_shelf_id}/health-history"),
            "Power Shelf Health History",
            "web-power-shelf-history-probe",
        ),
    ] {
        let body = get_page(&app, &path).await;
        assert!(body.contains(title), "{path} missing title {title}");
        assert!(
            body.contains(&format!("{path}.json")),
            "{path} missing JSON link"
        );
        assert!(
            body.contains(source),
            "{path} should render the seeded record source {source}"
        );

        let json = get_page(&app, &format!("{path}.json")).await;
        assert!(
            json.contains(source),
            "{path}.json should include the seeded record source {source}"
        );
    }
}

/// Persists a single health-history record for `object_id` via the shared
/// persistence layer, tagged with `source` so tests can assert it renders.
async fn seed_health_history(
    pool: &sqlx::PgPool,
    table: HealthHistoryTableId,
    object_id: &str,
    source: &str,
) {
    let report = HealthReport {
        source: source.to_string(),
        triggered_by: None,
        observed_at: None,
        successes: vec![],
        alerts: vec![],
    };
    let mut conn = pool.acquire().await.expect("acquire db connection");
    db::health_history::persist(&mut conn, table, &object_id, &report)
        .await
        .expect("persist seeded health history");
}

async fn get_page(app: &axum::Router, path: &str) -> String {
    let response = app
        .clone()
        .oneshot(web_request_builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "{path}");

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    String::from_utf8_lossy(&body_bytes).into_owned()
}

async fn post_machine_health_report(
    app: &axum::Router,
    machine_id: &str,
    action: &str,
    payload: &str,
) {
    let status = post_machine_health_report_with_request_context(
        app,
        machine_id,
        action,
        payload,
        "same-origin",
        "https://with.the.most",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

async fn post_machine_health_report_with_request_context(
    app: &axum::Router,
    machine_id: &str,
    action: &str,
    payload: &str,
    sec_fetch_site: &str,
    origin: &str,
) -> StatusCode {
    app.clone()
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!("/admin/machine/{machine_id}/health/{action}"))
                .header("Content-Type", "application/json")
                .header("Sec-Fetch-Site", sec_fetch_site)
                .header("Origin", origin)
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
}

async fn get_machine_health_page(app: &axum::Router, machine_id: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/machine/{machine_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    String::from_utf8_lossy(&body_bytes).into_owned()
}

async fn post_nvlink_domain_health_report(
    app: &axum::Router,
    domain_id: &str,
    action: &str,
    payload: &str,
) {
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!("/admin/nvlink-domain/{domain_id}/health/{action}"))
                .header("Content-Type", "application/json")
                .body(Body::from(payload.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
}

async fn get_nvlink_domain_health_page(app: &axum::Router, domain_id: &str) -> String {
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/nvlink-domain/{domain_id}/health"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    String::from_utf8_lossy(&body_bytes).into_owned()
}

async fn verify_health_page_not_found(app: &axum::Router, path: &str) {
    let response = app
        .clone()
        .oneshot(web_request_builder().uri(path).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");

    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    let body = String::from_utf8_lossy(&body_bytes);
    assert!(!body.contains("Health Report Management"));
}

fn aggregate_health_section(body: &str) -> &str {
    let active_reports_start = body
        .find("<h2>Active Health Reports</h2>")
        .unwrap_or(usize::MAX);
    let management_start = body
        .find("<h2>Health Report Management</h2>")
        .unwrap_or(usize::MAX);
    let end = active_reports_start.min(management_start).min(body.len());
    &body[..end]
}
