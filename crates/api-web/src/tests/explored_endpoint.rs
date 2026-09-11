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
use http_body_util::BodyExt;
use hyper::http::StatusCode;
use model::site_explorer::SiteExplorerLastRun;
use rpc::forge::forge_server::Forge;
use rpc::forge::{
    AgentUpgradePolicy, CredentialCreationRequest, CredentialType as RpcCredentialType,
    DpuAgentUpgradePolicyRequest,
};
use tower::ServiceExt;

use crate::tests::env::TestEnv;
use crate::tests::{make_test_app, web_request_builder};

const BANNER_TITLE: &str = "Default credential configuration incomplete";

/// Specific details the banner must NOT expose (credential names + vault key
/// path fragments), so the admin UI doesn't leak which credentials are unset.
const MUST_NOT_LEAK: [&str; 4] = [
    "Site-wide BMC root password",
    "Host UEFI password",
    "DPU UEFI password",
    "uefi-metadata-items",
];

/// The pages that should surface the missing-credentials warning: the
/// explored-endpoint list views and the root /admin page (per issue #2248).
const PAGES: [&str; 4] = [
    "/admin",
    "/admin/explored-endpoint",
    "/admin/explored-endpoint/unpaired",
    "/admin/explored-endpoint/paired",
];

const SITE_EXPLORER_RUN_STATUS_PAGES: [&str; 3] = [
    "/admin/explored-endpoint",
    "/admin/explored-endpoint/unpaired",
    "/admin/explored-endpoint/paired",
];
const EXPECTED_MACHINE_RUN_STATUS_PAGE: &str = "/admin/expected-machine";
const RAW_CREDENTIAL_ERROR: &str = "SiteExplorer run failed due to: Internal { message: \"Missing credential machines/bmc/site/root\" }";
const SANITIZED_CREDENTIAL_ERROR: &str = "Site Explorer credentials are missing or invalid";

async fn get_page(app: &axum::Router, uri: &str) -> String {
    let response = app
        .clone()
        .oneshot(web_request_builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    let body_bytes = response
        .into_body()
        .collect()
        .await
        .expect("Empty response body?")
        .to_bytes();
    String::from_utf8_lossy(&body_bytes).into_owned()
}

async fn configure_default(env: &TestEnv, credential_type: RpcCredentialType) {
    env.api()
        .create_credential(tonic::Request::new(CredentialCreationRequest {
            credential_type: credential_type.into(),
            username: None,
            password: "configured-password".to_string(),
            vendor: None,
            mac_address: None,
            credential_name: None,
        }))
        .await
        .unwrap();
}

#[crate::sqlx_test]
async fn test_site_explorer_run_status_banner(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);
    let finished_at = chrono::Utc::now();
    let last_run = SiteExplorerLastRun {
        started_at: finished_at,
        finished_at,
        success: false,
        error: Some(RAW_CREDENTIAL_ERROR.to_string()),
        failure_category: Some("missing_credentials".to_string()),
        endpoint_explorations: 3,
        endpoint_explorations_success: 2,
        endpoint_explorations_failed: 1,
        last_successful_finished_at: Some(finished_at),
        last_failed_finished_at: Some(finished_at),
    };
    let mut txn = env.api().database_connection.begin().await.unwrap();
    db::site_explorer_run_status::upsert(&mut txn, &last_run)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    for uri in SITE_EXPLORER_RUN_STATUS_PAGES
        .into_iter()
        .chain(std::iter::once(EXPECTED_MACHINE_RUN_STATUS_PAGE))
    {
        let body = get_page(&app, uri).await;
        assert!(body.contains("Last Site Explorer Run"), "{uri}");
        assert!(body.contains("Failed"), "{uri}");
        assert!(body.contains(SANITIZED_CREDENTIAL_ERROR), "{uri}");
        assert!(!body.contains("machines/bmc/site/root"), "{uri}");
        assert!(body.contains("<dt>Failure Category</dt>"), "{uri}");
        assert!(body.contains("<dd>missing_credentials</dd>"), "{uri}");
        assert!(body.contains("<dt>Attempted</dt>"), "{uri}");
        assert!(body.contains("<dd>3</dd>"), "{uri}");
        assert!(body.contains("<dt>Successful</dt>"), "{uri}");
        assert!(body.contains("<dd>2</dd>"), "{uri}");
        assert!(body.contains("<dt>Errored</dt>"), "{uri}");
        assert!(body.contains("<dd>1</dd>"), "{uri}");
        assert!(body.contains("<dt>Last Successful</dt>"), "{uri}");
        assert!(body.contains("<dt>Last Failed</dt>"), "{uri}");
    }
}

#[crate::sqlx_test]
async fn test_missing_credentials_banner(pool: sqlx::PgPool) {
    let env = TestEnv::new(pool).await;
    let app = make_test_app(&env.test_harness);

    // The root /admin page renders the active DPU agent-upgrade policy, which has
    // no row in a fresh database; set one so the page returns 200.
    env.api()
        .dpu_agent_upgrade_policy_action(tonic::Request::new(DpuAgentUpgradePolicyRequest {
            new_policy: Some(AgentUpgradePolicy::Off as i32),
        }))
        .await
        .unwrap();

    // Fresh environment: none of the site-wide defaults are set, so every target
    // page shows the generic warning. The banner must not name the specific
    // credentials or expose their vault key paths.
    for uri in PAGES {
        let body = get_page(&app, uri).await;
        assert!(
            body.contains(BANNER_TITLE),
            "expected missing-credentials banner on {uri}"
        );
        for leak in MUST_NOT_LEAK {
            assert!(
                !body.contains(leak),
                "banner on {uri} must not expose {leak:?}"
            );
        }
    }

    // Configure all three required defaults.
    configure_default(&env, RpcCredentialType::SiteWideBmcRoot).await;
    configure_default(&env, RpcCredentialType::HostUefi).await;
    configure_default(&env, RpcCredentialType::DpuUefi).await;

    // The warning disappears once every default is configured.
    for uri in PAGES {
        let body = get_page(&app, uri).await;
        assert!(
            !body.contains(BANNER_TITLE),
            "banner should be gone on {uri} after configuring defaults"
        );
    }
}
