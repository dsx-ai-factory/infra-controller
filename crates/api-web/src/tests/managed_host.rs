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
use std::str::FromStr as _;

use axum::body::Body;
use axum::response::Response;
use carbide_rpc_utils::ManagedHostOutput;
use carbide_test_harness::TestMachine as _;
use carbide_uuid::machine::{MachineId, MachineInterfaceId};
use db::machine;
use health_report::{
    HealthAlertClassification, HealthProbeAlert, HealthProbeId, HealthReport, HealthReportApplyMode,
};
use http_body_util::BodyExt;
use hyper::http::header::CONTENT_TYPE;
use hyper::http::{Method, StatusCode};
use model::machine::{InstanceState, ManagedHostState, RetryInfo};
use model::machine_boot_interface::BootInterfaceSelectionSource;
use tower::ServiceExt;

use crate::tests::env::TestEnv;
use crate::tests::{make_test_app, web_request_builder};

/// Loads the owned interface rows used by the managed-host boot-interface
/// page and its exact-UUID actions.
async fn load_machine_interfaces(
    env: &TestEnv,
    machine_id: MachineId,
) -> Vec<model::machine::MachineInterfaceSnapshot> {
    let mut txn = env.test_harness.db_txn().await;
    let interfaces = db::machine_interface::find_by_machine_ids(txn.as_mut(), &[machine_id])
        .await
        .expect("machine interfaces should load")
        .remove(&machine_id)
        .expect("managed host should have interface rows");
    txn.commit()
        .await
        .expect("interface lookup transaction should commit");
    interfaces
}

/// Submits the exact managed interface UUID used by both the operator
/// selection and one-shot system-default action.
async fn post_desired_boot_interface(
    app: &axum::Router,
    machine_id: MachineId,
    machine_interface_id: MachineInterfaceId,
) -> Response {
    app.clone()
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!("/admin/machine/{machine_id}/boot-interface"))
                .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
                .body(Body::from(format!(
                    "machine_interface_id={machine_interface_id}"
                )))
                .unwrap(),
        )
        .await
        .unwrap()
}

/// Isolates the desired boot-interface section so assertions cannot be
/// satisfied by the older interface or SKU tables elsewhere on the page.
fn desired_boot_interface_section(body: &str) -> &str {
    const HEADING: &str = "<h3 id=\"desired_boot_interface\">";
    let start = body
        .find(HEADING)
        .expect("desired boot-interface section should render");
    let content_start = start + HEADING.len();
    let end = body[content_start..]
        .find("<h3")
        .map_or(body.len(), |offset| content_start + offset);
    &body[start..end]
}

#[crate::sqlx_test]
async fn test_ok(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    _ = env.create_ready_managed_host(1).await;

    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/managed-host.json")
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

    let body_str = std::str::from_utf8(&body_bytes).expect("Invalid UTF-8 in body");
    let hosts: Vec<ManagedHostOutput> =
        serde_json::from_str(body_str).expect("Could not deserialize response");

    assert_eq!(hosts.len(), 1, "One host should have been returned");
    let host = hosts.first().unwrap();
    assert_eq!(host.dpus.len(), 1, "Host should have 1 dpu");
    let dpu = host.dpus.first().unwrap();
    assert!(
        !host.discovery_info.network_interfaces.is_empty(),
        "Host discovery info should be populated and non-default"
    );
    assert_ne!(
        dpu.machine_id, host.machine_id,
        "DPU should not have the same machine ID as the host"
    );
    assert!(
        !dpu.discovery_info.network_interfaces.is_empty(),
        "DPU discovery info should be populated and non-default"
    );
}

#[crate::sqlx_test]
async fn machine_detail_manages_the_desired_boot_interface(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let host = env.create_ready_managed_host(2).await.0;
    let machine_id = host.host.id;
    let interfaces = load_machine_interfaces(&env, machine_id.into()).await;
    let default_interface = model::machine::pick_default_boot_interface(&interfaces)
        .expect("managed host should have a system-default interface")
        .clone();
    let selected_interface = interfaces
        .iter()
        .find(|interface| {
            !interface.primary_interface
                && interface.id != default_interface.id
                && interface.attached_dpu_machine_id.is_some()
        })
        .expect("two-DPU host should have another selectable DPU interface")
        .clone();

    sqlx::query(
        "UPDATE machine_boot_interfaces
         SET selection_source = 'legacy_unknown', selection_updated_at = NULL
         WHERE machine_id = $1",
    )
    .bind(machine_id)
    .execute(&env.api().database_connection)
    .await
    .expect("the legacy selection fixture should be persisted");

    // The page combines persisted reconciliation state with every exact
    // managed row an operator can select.
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/machine/{machine_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("machine detail body should be readable")
        .to_bytes();
    let body = std::str::from_utf8(&body).expect("machine detail should be UTF-8");
    let boot_interface_section = desired_boot_interface_section(body);

    assert!(boot_interface_section.contains("Desired Boot Interface"));
    assert!(boot_interface_section.contains("Converged"));
    assert!(boot_interface_section.contains("Redfish verified"));
    assert!(boot_interface_section.contains("<th>Selection Source</th>"));
    assert!(boot_interface_section.contains("<th>Selection Updated At</th>"));
    assert!(
        boot_interface_section.contains("<tr><th>Selection Source</th><td>LegacyUnknown</td></tr>")
    );
    assert!(boot_interface_section.contains("<tr><th>Selection Updated At</th><td>-</td></tr>"));
    assert!(boot_interface_section.contains(&host.host.primary_mac().to_string()));
    assert!(boot_interface_section.contains("<strong>Current</strong>"));
    assert!(boot_interface_section.contains("Matches system default"));
    assert!(boot_interface_section.contains("Set desired interface"));
    assert!(boot_interface_section.contains("Request reconciliation"));
    assert!(
        boot_interface_section.contains("<input type=\"submit\" value=\"Use system default\">")
    );
    assert!(boot_interface_section.contains(&format!(
        "name=\"machine_interface_id\" value=\"{}\"",
        default_interface.id
    )));
    for interface in &interfaces {
        assert!(boot_interface_section.contains(&interface.id.to_string()));
        assert!(boot_interface_section.contains(&interface.mac_address.to_string()));
        if let Some(redfish_interface_id) = &interface.boot_interface_id {
            assert!(boot_interface_section.contains(redfish_interface_id));
        }
    }
    assert!(!body.contains("action_reminder(event)"));

    // Selecting an exact row moves primary + desired state atomically, while
    // the request path leaves the BMC untouched.
    let redfish_timepoint = env.redfish_sim.timepoint();
    let response =
        post_desired_boot_interface(&app, machine_id.into(), selected_interface.id).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts()
            .is_empty(),
        "the web request should leave Redfish work to machine-controller",
    );

    let selected_desired =
        db::machine_desired_boot_interface::get(&env.api().database_connection, &machine_id)
            .await
            .unwrap()
            .expect("selecting an interface should persist a desired target");
    assert_eq!(
        selected_desired.value.mac_address(),
        selected_interface.mac_address
    );
    assert_eq!(
        selected_desired.value.interface_id(),
        selected_interface.boot_interface_id.as_deref()
    );
    assert!(
        load_machine_interfaces(&env, machine_id.into())
            .await
            .iter()
            .find(|interface| interface.id == selected_interface.id)
            .is_some_and(|interface| interface.primary_interface),
        "the selected row should become primary",
    );

    let selection = host
        .host
        .machine()
        .await
        .config
        .boot_interface_selection
        .expect("the operator selection should retain its source");
    assert_eq!(selection.source, BootInterfaceSelectionSource::Operator);
    let selection_updated_at = selection
        .updated_at
        .expect("the operator selection should retain its decision time")
        .to_string();

    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/machine/{machine_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("machine detail body should be readable")
        .to_bytes();
    let body = std::str::from_utf8(&body).expect("machine detail should be UTF-8");
    let boot_interface_section = desired_boot_interface_section(body);
    assert!(boot_interface_section.contains("<tr><th>Selection Source</th><td>Operator</td></tr>"));
    assert!(boot_interface_section.contains(&format!(
        "<tr><th>Selection Updated At</th><td>{selection_updated_at}</td></tr>"
    )));

    // `Use system default` is the same exact-row write with the server's
    // current default mapped back to its managed UUID.
    let redfish_timepoint = env.redfish_sim.timepoint();
    let response = post_desired_boot_interface(&app, machine_id.into(), default_interface.id).await;
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts()
            .is_empty(),
        "restoring the default should leave Redfish work to machine-controller",
    );

    let default_desired =
        db::machine_desired_boot_interface::get(&env.api().database_connection, &machine_id)
            .await
            .unwrap()
            .expect("restoring the system default should retain a desired target");
    assert_eq!(
        default_desired.value.mac_address(),
        default_interface.mac_address
    );
    assert_eq!(
        default_desired.value.interface_id(),
        default_interface.boot_interface_id.as_deref()
    );

    // Reconciliation keeps that target and only advances its generation for
    // machine-controller.
    let redfish_timepoint = env.redfish_sim.timepoint();
    let response = app
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!(
                    "/admin/machine/{machine_id}/boot-interface/reconcile"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts()
            .is_empty(),
        "requesting reconciliation should leave Redfish work to machine-controller",
    );

    let reconciled_desired =
        db::machine_desired_boot_interface::get(&env.api().database_connection, &machine_id)
            .await
            .unwrap()
            .expect("requesting reconciliation should retain the desired target");
    assert_eq!(reconciled_desired.value, default_desired.value);
    assert_ne!(reconciled_desired.version, default_desired.version);
}

#[crate::sqlx_test]
async fn machine_detail_shows_an_uninitialized_desired_boot_interface(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let host = env.create_ready_managed_host(1).await.0;
    let machine_id = host.host.id;
    sqlx::query("DELETE FROM machine_boot_interfaces WHERE machine_id = $1")
        .bind(machine_id)
        .execute(&env.api().database_connection)
        .await
        .expect("test desired boot-interface row should be deleted");

    let app = make_test_app(&env.test_harness);
    let response = app
        .clone()
        .oneshot(
            web_request_builder()
                .uri(format!("/admin/machine/{machine_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let body = response
        .into_body()
        .collect()
        .await
        .expect("machine detail body should be readable")
        .to_bytes();
    let body = std::str::from_utf8(&body).expect("machine detail should be UTF-8");
    let boot_interface_section = desired_boot_interface_section(body);
    assert!(boot_interface_section.contains("Desired Boot Interface"));
    assert!(boot_interface_section.contains("Not yet initialized"));
    assert!(boot_interface_section.contains("Set desired interface"));
    assert!(!boot_interface_section.contains("/boot-interface/reconcile"));

    // The button is hidden, and a direct POST also refuses to turn a
    // reconciliation request into implicit initialization.
    let redfish_timepoint = env.redfish_sim.timepoint();
    let response = app
        .oneshot(
            web_request_builder()
                .method(Method::POST)
                .uri(format!(
                    "/admin/machine/{machine_id}/boot-interface/reconcile"
                ))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SEE_OTHER);
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .all_hosts()
            .is_empty(),
        "an uninitialized request should not touch Redfish",
    );
    assert!(
        db::machine_desired_boot_interface::get(&env.api().database_connection, &machine_id)
            .await
            .unwrap()
            .is_none(),
        "reconciliation without a desired target should not initialize one",
    );
}

#[crate::sqlx_test]
async fn test_multi_dpu(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let mh = env.create_ready_managed_host(2).await.0;
    let host_machine_id = mh.host.id;

    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/managed-host.json")
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

    let body_str = std::str::from_utf8(&body_bytes).expect("Invalid UTF-8 in body");
    let hosts: Vec<ManagedHostOutput> =
        serde_json::from_str(body_str).expect("Could not deserialize response");

    assert!(
        !hosts.is_empty(),
        "At least one host should have been returned"
    );
    let host = hosts
        .into_iter()
        .find(|h| {
            h.machine_id
                .as_ref()
                .map(|m| m == &host_machine_id.to_string())
                .unwrap_or(false)
        })
        .unwrap_or_else(|| {
            panic!("Could not find expected host {host_machine_id} in managed_hosts output")
        });
    assert!(host.hostname.is_some(), "Hostname should be set");
    assert_eq!(host.dpus.len(), 2, "Host should have 2 dpus");
    for dpu in host.dpus.iter() {
        assert_ne!(
            dpu.machine_id, host.machine_id,
            "DPU should not have the same machine ID as the host"
        );
    }
}

#[crate::sqlx_test]
async fn test_managed_host_html_includes_health_alert_details(
    pool: sqlx::PgPool,
) -> eyre::Result<()> {
    let env = TestEnv::new(pool).await;
    let mh = env.create_ready_managed_host(1).await.0;

    let report = HealthReport {
        source: "mock-bmc-intrusion".to_string(),
        triggered_by: None,
        observed_at: None,
        successes: vec![],
        alerts: vec![HealthProbeAlert {
            id: HealthProbeId::from_str("IntrusionSensorTriggered")?,
            target: Some("HostBMC".to_string()),
            in_alert_since: None,
            message: "Physical Chassis Intrusion Alert".to_string(),
            tenant_message: None,
            classifications: vec![
                HealthAlertClassification::hardware(),
                HealthAlertClassification::sensor_critical(),
                HealthAlertClassification::prevent_allocations(),
            ],
        }],
    };

    let mut txn = env.test_harness.db_txn().await;
    db::machine::insert_health_report(
        &mut txn,
        &mh.host.id,
        HealthReportApplyMode::Merge,
        &report,
        false,
    )
    .await?;
    txn.commit().await?;

    let app = make_test_app(&env.test_harness);
    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/managed-host")
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
    let body_str = std::str::from_utf8(&body_bytes).expect("Invalid UTF-8 in body");

    assert!(
        body_str.contains(
            "IntrusionSensorTriggered [Target: HostBMC]: Physical Chassis Intrusion Alert"
        )
    );
    assert!(body_str.contains("will be removed in a future release"));
    assert!(!body_str.contains("v2.1"));

    Ok(())
}

#[crate::sqlx_test]
async fn test_managed_host_html_uses_runtime_sla_config(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let mh = env.create_ready_managed_host(1).await.0;

    let assigned_booting_state = ManagedHostState::Assigned {
        instance_state: InstanceState::BootingWithDiscoveryImage {
            retry: RetryInfo { count: 0 },
        },
    };
    let state_changed_at = chrono::Utc::now() - chrono::Duration::minutes(5);
    let state_version: config_version::ConfigVersion =
        format!("V999-T{}", state_changed_at.timestamp_micros())
            .parse()
            .unwrap();

    let mut txn = env.test_harness.db_txn().await;
    let host_machine = mh.host.db_machine(&mut txn).await;
    machine::advance(
        &host_machine,
        &mut txn,
        &assigned_booting_state,
        Some(state_version),
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let app = make_test_app(&env.test_harness);
    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/managed-host?time-in-state-above-sla-filter=true")
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
    let body_str = std::str::from_utf8(&body_bytes).expect("Invalid UTF-8 in body");

    assert!(body_str.contains("Filtered Managed Hosts (1)"));
    assert!(body_str.contains("bubble warning"));
    assert!(body_str.contains("Assigned/BootingWithDiscoveryImage"));
}

#[crate::sqlx_test]
async fn test_managed_host_health_alert_exact_filter(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env = TestEnv::new(pool).await;
    let mh_alerting = env.create_ready_managed_host(1).await.0;
    let mh_healthy = env.create_ready_managed_host(2).await.0;

    let report = HealthReport {
        source: "mock-bmc-intrusion".to_string(),
        triggered_by: None,
        observed_at: None,
        successes: vec![],
        alerts: vec![HealthProbeAlert {
            id: HealthProbeId::from_str("IntrusionSensorTriggered")?,
            target: Some("HostBMC".to_string()),
            in_alert_since: None,
            message: "Physical Chassis Intrusion Alert".to_string(),
            tenant_message: None,
            classifications: vec![
                HealthAlertClassification::hardware(),
                HealthAlertClassification::sensor_critical(),
                HealthAlertClassification::prevent_allocations(),
            ],
        }],
    };

    let mut txn = env.test_harness.db_txn().await;
    db::machine::insert_health_report(
        &mut txn,
        &mh_alerting.host.id,
        HealthReportApplyMode::Merge,
        &report,
        false,
    )
    .await?;
    txn.commit().await?;

    let app = make_test_app(&env.test_harness);
    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/managed-host?health-alerts-filter=IntrusionSensorTriggered")
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
    let body_str = std::str::from_utf8(&body_bytes).expect("Invalid UTF-8 in body");

    // Only the alerting host should show up, not the healthy one.
    assert!(body_str.contains(&mh_alerting.host.id.to_string()));
    assert!(!body_str.contains(&mh_healthy.host.id.to_string()));

    // The dynamic dropdown should include this alert ID as the selected option.
    assert!(
        body_str.contains(r#"<option value="IntrusionSensorTriggered" selected>"#),
        "expected the alert ID to appear as a selected dropdown option"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_managed_host_health_alert_filter_empty_state(pool: sqlx::PgPool) -> eyre::Result<()> {
    let env = TestEnv::new(pool).await;
    let _mh = env.create_ready_managed_host(1).await.0;

    let app = make_test_app(&env.test_harness);
    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/managed-host?health-alerts-filter=SomeAlertNoOneHas")
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
    let body_str = std::str::from_utf8(&body_bytes).expect("Invalid UTF-8 in body");

    assert!(body_str.contains("No machines match the selected filters."));
    assert!(
        body_str.contains(r#"<option value="SomeAlertNoOneHas" selected>"#),
        "expected the stale alert ID to appear as a selected dropdown option"
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_managed_host_empty_health_alert_filter_is_unfiltered(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let mh = env.create_ready_managed_host(1).await.0;

    let app = make_test_app(&env.test_harness);
    let response = app
        .oneshot(
            web_request_builder()
                .uri("/admin/managed-host?health-alerts-filter=")
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
    let body_str = std::str::from_utf8(&body_bytes).expect("Invalid UTF-8 in body");

    assert!(
        !body_str.contains(r#"<option value="" selected>"#),
        "expected an empty health-alert filter not to render a blank selected option"
    );
    assert!(body_str.contains("All Managed Hosts (1)"));
    assert!(body_str.contains(&mh.host.id.to_string()));
}

#[crate::sqlx_test]
async fn test_managed_host_group_drilldown_preserves_other_filters(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    _ = env.create_ready_managed_host(1).await;

    let app = make_test_app(&env.test_harness);
    let response = app
        .oneshot(
            web_request_builder()
                .uri(
                    "/admin/managed-host?health-alerts-filter=healthy&state-filter=ready&time-in-state-above-sla-filter=false&group-by=state&current_page=7&limit=25",
                )
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
    let body_str = std::str::from_utf8(&body_bytes).expect("Invalid UTF-8 in body");

    let drilldown_link = r#"href="/admin/managed-host?health-alerts-filter=healthy&#38;time-in-state-above-sla-filter=false&#38;state-filter=ready""#;
    assert!(
        body_str.contains(drilldown_link),
        "expected grouped-host drilldown link to preserve active filters: {body_str}"
    );
    assert_eq!(
        body_str.matches("state-filter=ready").count(),
        1,
        "group filter must replace, not duplicate, the active state filter"
    );
}
