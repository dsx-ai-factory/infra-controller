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
use std::time::Duration;

use ::rpc::forge::forge_server::Forge;
use carbide_api_core::AuthContext;
use carbide_api_core::test_support::fixture_config::FixtureDefault as _;
use carbide_api_core::test_support::redfish::test_behavior;
use carbide_authn::middleware::{ExternalUserInfo, Principal};
use carbide_secrets::credentials::Credentials;
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_site_explorer::config::SiteExplorerConfig;
use carbide_test_harness::TestNetworkSegment;
use carbide_test_harness::prelude::*;
use model::test_support::ManagedHostConfig;
use rpc::forge::{RedfishAction, RedfishActionResult};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tokio::time::Instant;

struct TestEnv {
    api: Arc<Api>,
    harness: TestHarness,
    admin_segment: TestNetworkSegment,
    underlay_segment: TestNetworkSegment,
    site_explorer: TestSiteExplorer,
}

async fn create_test_env(pool: PgPool) -> TestEnv {
    let harness = TestHarness::builder(pool)
        .with_api_builder_fn(|builder| {
            builder.with_credential_manager(Arc::new(TestCredentialManager::new(
                Credentials::UsernamePassword {
                    username: "root".to_string(),
                    password: "notforprod".to_string(),
                },
            )))
        })
        .build()
        .await;
    let domain = harness.test_domain().await;
    let network_controller = harness.network_controller();
    let admin_segment = network_controller.create_admin_segment(&domain).await;
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    let site_explorer = harness.test_site_explorer(SiteExplorerConfig::default());
    let api = harness.api_arc();

    TestEnv {
        api,
        harness,
        admin_segment,
        underlay_segment,
        site_explorer,
    }
}

async fn create_managed_host(env: &TestEnv) -> TestManagedHost {
    let mut managed_host = env
        .harness
        .managed_host_builder(&env.site_explorer, env.underlay_segment)
        .with_config(ManagedHostConfig::default())
        .with_dpu_network_status_reported()
        .build()
        .await
        .0;
    managed_host
        .host
        .discover_primary_iface(env.admin_segment)
        .await;
    managed_host.advance_to_converged_ready().await;
    managed_host
}

#[sqlx_test]
async fn test_create_and_approve_action(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let bmc_ip = mh
        .host
        .rpc_machine()
        .await
        .bmc_info
        .as_ref()
        .unwrap()
        .ip()
        .to_string();

    let request = ::rpc::forge::RedfishCreateActionRequest {
        ips: vec![bmc_ip.clone()],
        action: "#ComputerSystem.Reset".to_string(),
        target: "/redfish/v1/Systems/System.Embedded.1/Actions/ComputerSystem.Reset".to_string(),
        parameters:
            serde_json::json!({"ResetType": "ForceOff", "__TEST_BEHAVIOR__": test_behavior::success()})
                .to_string(),
    };

    let response = env
        .api
        .redfish_create_action(request_with_username("user1", request.clone()))
        .await
        .unwrap()
        .into_inner();
    let request_id = response.request_id;

    let mut actions = list_actions(&env, Some(bmc_ip.clone())).await;
    assert_eq!(actions.len(), 1);
    let action = actions.remove(0);
    assert_eq!(action.request_id, request_id);
    assert_eq!(action.target, request.target);
    assert_eq!(action.parameters, request.parameters);
    assert_eq!(action.action, request.action);
    assert_eq!(&action.requester, "user1");
    assert_eq!(action.approvers, vec!["user1".to_string()]);
    assert_eq!(
        action.results,
        vec![::rpc::forge::OptionalRedfishActionResult { result: None }]
    );

    // Trying to apply the action without approvals fails
    let err = env
        .api
        .redfish_apply_action(request_with_username(
            "user1",
            rpc::forge::RedfishActionId { request_id },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(err.message(), "insufficient approvals");

    // Try to approve again with same username
    let err = env
        .api
        .redfish_approve_action(request_with_username(
            "user1",
            rpc::forge::RedfishActionId { request_id },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(err.message(), "user already approved request");

    // Approve by second user is ok
    env.api
        .redfish_approve_action(request_with_username(
            "user2",
            rpc::forge::RedfishActionId { request_id },
        ))
        .await
        .unwrap()
        .into_inner();
    let mut actions = list_actions(&env, Some(bmc_ip.clone())).await;
    assert_eq!(actions.len(), 1);
    let action = actions.remove(0);
    assert_eq!(
        action.approvers,
        vec!["user2".to_string(), "user1".to_string()]
    );
    assert_eq!(
        action.results,
        vec![::rpc::forge::OptionalRedfishActionResult { result: None }]
    );

    // Approve by second user again
    let err = env
        .api
        .redfish_approve_action(request_with_username(
            "user2",
            rpc::forge::RedfishActionId { request_id },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(err.message(), "user already approved request");

    // Test whether the raw DB query allows double insertion of approver
    let mut txn = env.harness.db_txn().await;
    assert!(
        !db::redfish_actions::approve_request("user2".to_string(), request_id.into(), &mut txn)
            .await
            .unwrap()
    );
    txn.commit().await.unwrap();

    let mut actions = list_actions(&env, Some(bmc_ip.clone())).await;
    assert_eq!(actions.len(), 1);
    let action = actions.remove(0);
    assert_eq!(
        action.approvers,
        vec!["user2".to_string(), "user1".to_string()]
    );

    env.api
        .redfish_apply_action(request_with_username(
            "user1",
            rpc::forge::RedfishActionId { request_id },
        ))
        .await
        .expect("request should have succeeded");

    let results = wait_for_action_results(&env, &bmc_ip).await;

    for result in results {
        assert_eq!(result.status, "OK")
    }
}

#[sqlx_test]
async fn test_action_failure_at_bmc_request(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let bmc_ip = mh
        .host
        .rpc_machine()
        .await
        .bmc_info
        .as_ref()
        .unwrap()
        .ip()
        .to_string();

    let request = ::rpc::forge::RedfishCreateActionRequest {
        ips: vec![bmc_ip.clone()],
        action: "#ComputerSystem.Reset".to_string(),
        target: "/redfish/v1/Systems/System.Embedded.1/Actions/ComputerSystem.Reset".to_string(),
        parameters: serde_json::json!({"ResetType": "ForceOff", "__TEST_BEHAVIOR__": test_behavior::failure_at_request()}).to_string(),
    };

    let response = env
        .api
        .redfish_create_action(request_with_username("user1", request.clone()))
        .await
        .unwrap()
        .into_inner();
    let request_id = response.request_id;

    env.api
        .redfish_approve_action(request_with_username(
            "user2",
            rpc::forge::RedfishActionId { request_id },
        ))
        .await
        .unwrap()
        .into_inner();

    env.api
        .redfish_apply_action(request_with_username(
            "user1",
            rpc::forge::RedfishActionId { request_id },
        ))
        .await
        .expect("request should have succeeded");

    let results = wait_for_action_results(&env, &bmc_ip).await;

    for result in results {
        assert_eq!(
            result.status,
            http::StatusCode::INTERNAL_SERVER_ERROR.to_string()
        );
        assert_eq!(result.body, test_behavior::request_failure_description(),);
    }
}

#[sqlx_test]
async fn test_action_failure_at_client_creation(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let bmc_ip = mh
        .host
        .rpc_machine()
        .await
        .bmc_info
        .as_ref()
        .unwrap()
        .ip()
        .to_string();

    let request = ::rpc::forge::RedfishCreateActionRequest {
        ips: vec![bmc_ip.clone()],
        action: "#ComputerSystem.Reset".to_string(),
        target: "/redfish/v1/Systems/System.Embedded.1/Actions/ComputerSystem.Reset".to_string(),
        parameters: serde_json::json!({"ResetType": "ForceOff", "__TEST_BEHAVIOR__": test_behavior::failure_at_client_creation()}).to_string(),
    };

    let response = env
        .api
        .redfish_create_action(request_with_username("user1", request.clone()))
        .await
        .unwrap()
        .into_inner();
    let request_id = response.request_id;

    env.api
        .redfish_approve_action(request_with_username(
            "user2",
            rpc::forge::RedfishActionId { request_id },
        ))
        .await
        .unwrap()
        .into_inner();

    env.api
        .redfish_apply_action(request_with_username(
            "user1",
            rpc::forge::RedfishActionId { request_id },
        ))
        .await
        .expect("request should have succeeded");

    let results = wait_for_action_results(&env, &bmc_ip).await;

    for result in results {
        assert_eq!(result.status, "not executed");
        assert!(
            result
                .body
                .contains("error creating redfish client, see logs")
        );
    }
}

async fn wait_for_action_results(env: &TestEnv, bmc_ip: &str) -> Vec<RedfishActionResult> {
    let start = Instant::now();
    let mut retry_interval = tokio::time::interval(Duration::from_millis(100));

    // Wait for it to be applied (it runs in the background, so we get a response before it's applied.)
    loop {
        if start.elapsed() > Duration::from_secs(1) {
            panic!("Did not see the action applied after timeout");
        }
        retry_interval.tick().await;

        let mut actions = list_actions(env, Some(bmc_ip.to_string())).await;
        assert_eq!(actions.len(), 1);
        let action = actions.remove(0);
        assert!(
            action.applied_at.is_some(),
            "action should have been applied before redfish_apply_action returned (even if no results yet)"
        );

        if action.results.is_empty() {
            // must not have applied yet.
            continue;
        }

        let inner_results = action
            .results
            .into_iter()
            .filter_map(|r| r.result)
            .collect::<Vec<_>>();

        if inner_results.is_empty() {
            // must not have applied yet.
            continue;
        }

        break inner_results;
    }
}

async fn list_actions(env: &TestEnv, bmc_ip: Option<String>) -> Vec<RedfishAction> {
    env.api
        .redfish_list_actions(tonic::Request::new(
            ::rpc::forge::RedfishListActionsRequest { machine_ip: bmc_ip },
        ))
        .await
        .unwrap()
        .into_inner()
        .actions
}

fn request_with_username<T>(user: &str, request: T) -> tonic::Request<T> {
    let mut request = tonic::Request::new(request);

    let mut auth_context = AuthContext::default();
    auth_context
        .principals
        .push(Principal::ExternalUser(ExternalUserInfo {
            org: Some("test_org".to_string()),
            group: "test_group".to_string(),
            user: Some(user.to_string()),
        }));

    request.extensions_mut().insert(auth_context);

    request
}
