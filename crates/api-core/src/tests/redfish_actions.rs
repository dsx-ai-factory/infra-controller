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

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use carbide_authn::middleware::{ExternalUserInfo, Principal};
use carbide_utils::HostPortPair;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use model::redfish::ActionRequest;
use rpc::forge::forge_server::Forge;
use rpc::forge::{RedfishActionId, RedfishCreateActionRequest};
use rustls::pki_types::PrivateKeyDer;
use sqlx::PgPool;
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_rustls::TlsAcceptor;

use crate::api::Api;
use crate::auth::AuthContext;
use crate::tests::common::api_fixtures::{create_managed_host, create_test_env};
use crate::tests::common::postgres::wait_for_blocked_query;

const ACTION_TARGET: &str = "/redfish/v1/Systems/System.Embedded.1/Actions/ComputerSystem.Reset";

fn request_with_username<T>(user: &str, message: T) -> tonic::Request<T> {
    let mut request = tonic::Request::new(message);
    let mut context = AuthContext::default();
    context
        .principals
        .push(Principal::ExternalUser(ExternalUserInfo::new(
            Some("test_org".to_string()),
            "test_group".to_string(),
            Some(user.to_string()),
        )));
    request.extensions_mut().insert(context);
    request
}

async fn create_approved_action(api: &Api, ips: Vec<String>) -> i64 {
    let request_id = api
        .redfish_create_action(request_with_username(
            "user1",
            RedfishCreateActionRequest {
                ips,
                action: "#ComputerSystem.Reset".to_string(),
                target: ACTION_TARGET.to_string(),
                parameters: "{\"ResetType\":\"ForceOff\"}".to_string(),
            },
        ))
        .await
        .expect("create action")
        .into_inner()
        .request_id;
    api.redfish_approve_action(request_with_username(
        "user2",
        RedfishActionId { request_id },
    ))
    .await
    .expect("approve action");
    request_id
}

async fn fetch_action(pool: &PgPool, request_id: i64) -> ActionRequest {
    db::redfish_actions::fetch_request(
        request_id.into(),
        &mut pool.acquire().await.expect("acquire action reader"),
    )
    .await
    .expect("read action")
}

#[crate::sqlx_test]
async fn failed_claim_commit_does_not_dispatch_and_same_action_can_retry(pool: PgPool) {
    let env = Box::pin(create_test_env(pool.clone())).await;
    let host = Box::pin(create_managed_host(&env)).await;
    let bmc_ip = host
        .host()
        .rpc_machine()
        .await
        .bmc_info
        .expect("host BMC")
        .ip()
        .to_string();
    let request_id = create_approved_action(&env.api, vec![bmc_ip]).await;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind fake BMC");
    let address = listener.local_addr().unwrap();
    env.api
        .dynamic_settings
        .bmc_proxy
        .store(Arc::new(Some(HostPortPair::HostAndPort(
            address.ip().to_string(),
            address.port(),
        ))));

    // The claim UPDATE succeeds; only COMMIT checks this foreign key.
    // This table and constraint live in the test's isolated database.
    sqlx::raw_sql(
        "CREATE TABLE allowed_action_appliers (name text PRIMARY KEY);
         ALTER TABLE redfish_bmc_actions ADD CONSTRAINT test_applier_commit_failure
         FOREIGN KEY (applier) REFERENCES allowed_action_appliers (name)
         DEFERRABLE INITIALLY DEFERRED;",
    )
    .execute(&pool)
    .await
    .expect("install commit-only fault");

    let error = env
        .api
        .redfish_apply_action(request_with_username(
            "user1",
            RedfishActionId { request_id },
        ))
        .await
        .expect_err("claim commit must fail");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(
        error.message().contains("test_applier_commit_failure"),
        "{error}"
    );
    let action = fetch_action(&pool, request_id).await;
    assert!(action.applied_at.is_none());
    assert!(action.applier.is_none());
    assert_eq!(action.results.len(), 1);
    assert!(action.results.iter().all(Option::is_none));
    // Watch for even a TCP connection, not just a completed POST. This is a
    // bounded negative observation because dispatch tasks have no join handle.
    assert!(
        timeout(Duration::from_secs(1), listener.accept())
            .await
            .is_err(),
        "failed claim commit dispatched a request"
    );

    sqlx::query("INSERT INTO allowed_action_appliers (name) VALUES ('user1')")
        .execute(&pool)
        .await
        .expect("allow the same action's retry to commit");
    let rcgen::CertifiedKey { cert, signing_key } =
        rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![cert.der().clone()],
        PrivateKeyDer::Pkcs8(signing_key.serialize_der().into()),
    )
    .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(tls));
    let pool = &pool;

    timeout(Duration::from_secs(5), async {
        let (apply, ()) = tokio::join!(
            env.api.redfish_apply_action(request_with_username(
                "user1",
                RedfishActionId { request_id },
            )),
            async {
                let (stream, _) = listener.accept().await.expect("receive action connection");
                let stream = acceptor.accept(stream).await.expect("accept action TLS");
                hyper::server::conn::http1::Builder::new()
                    .keep_alive(false)
                    .serve_connection(
                        TokioIo::new(stream),
                        service_fn(
                            |request: hyper::Request<hyper::body::Incoming>| async move {
                                assert_eq!(request.method(), http::Method::POST);
                                assert_eq!(request.uri().path(), ACTION_TARGET);
                                let body =
                                    axum::body::to_bytes(Body::new(request.into_body()), 1024)
                                        .await
                                        .expect("read action parameters");
                                assert_eq!(body.as_ref(), b"{\"ResetType\":\"ForceOff\"}");
                                let action = fetch_action(pool, request_id).await;
                                assert!(action.applied_at.is_some(), "claim must precede the POST");
                                assert_eq!(action.applier.as_deref(), Some("user1"));
                                Ok::<_, Infallible>(hyper::Response::new(Body::from(
                                    "action completed",
                                )))
                            },
                        ),
                    )
                    .await
                    .expect("serve action response");
            },
        );
        apply.expect("same action dispatches after its claim commits");
        loop {
            let action = fetch_action(pool, request_id).await;
            if let Some(result) = &action.results[0] {
                assert_eq!(result.status, "200 OK");
                assert_eq!(result.body, "action completed");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("successful action dispatch and result storage finish");
}

#[crate::sqlx_test]
async fn cancellation_after_read_is_not_reported_as_prior_approval_or_apply(pool: PgPool) {
    struct Case {
        scenario: &'static str,
        approve: bool,
        query_fragment: &'static str,
        expected_message: &'static str,
    }

    let env = Box::pin(create_test_env(pool.clone())).await;
    for case in [
        Case {
            scenario: "approval after cancellation",
            approve: true,
            query_fragment: "SET approvers = array_prepend",
            expected_message: "request no longer exists or user already approved it",
        },
        Case {
            scenario: "apply after cancellation",
            approve: false,
            query_fragment: "SET applied_at = now()",
            expected_message: "request no longer exists or was already applied",
        },
    ] {
        let request_id = create_approved_action(&env.api, Vec::new()).await;
        let mut cancellation = pool.begin().await.expect("begin cancellation");
        let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
            .fetch_one(cancellation.as_mut())
            .await
            .unwrap();
        db::redfish_actions::delete_request(request_id.into(), cancellation.as_mut())
            .await
            .expect("delete unclaimed action");

        let api = env.api.clone();
        let handler = tokio::spawn(async move {
            let request = request_with_username("user3", RedfishActionId { request_id });
            if case.approve {
                api.redfish_approve_action(request).await.map(|_| ())
            } else {
                api.redfish_apply_action(request).await.map(|_| ())
            }
        });

        // The uncommitted deletion is invisible to the initial SELECT. Wait
        // for the conditional UPDATE before making cancellation visible.
        wait_for_blocked_query(&pool, blocker_pid, case.query_fragment).await;
        cancellation.commit().await.expect("commit cancellation");
        let error = timeout(Duration::from_secs(5), handler)
            .await
            .expect("handler finishes after cancellation")
            .expect("handler task joins")
            .expect_err("cancelled action cannot be approved or applied");
        assert_eq!(
            error.code(),
            tonic::Code::InvalidArgument,
            "{}",
            case.scenario
        );
        assert_eq!(error.message(), case.expected_message, "{}", case.scenario);
        let remaining: i64 =
            sqlx::query_scalar("SELECT count(*) FROM redfish_bmc_actions WHERE request_id = $1")
                .bind(request_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0, "{}", case.scenario);
    }
}
