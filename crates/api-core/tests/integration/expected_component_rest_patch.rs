// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

use std::net::Ipv4Addr;
use std::path::Path;

use axum::Router;
use carbide_test_harness::prelude::*;
use rpc::forge::forge_server::ForgeServer;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

/// The Go test sends real REST requests through the production proxy workflow
/// and activity. Core serves those requests against this test's PostgreSQL
/// database, so dispatch alone cannot satisfy the preservation assertions.
#[sqlx_test]
#[ignore = "run: make core/tests TEST_ARGS='-p carbide-api-core --test integration expected_component_rest_patch::test_expected_component_rest_patch -- --ignored --exact'"]
async fn test_expected_component_rest_patch(pool: PgPool) {
    let env = crate::expected_power_shelf::create_test_env(pool).await;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .expect("bind Core test listener");
    let address = listener.local_addr().expect("read Core test address");
    let router = Router::new().route_service(
        rpc::service_path!("{*rpc}"),
        ForgeServer::from_arc(env.api_arc()),
    );
    let cancellation = CancellationToken::new();
    let server_cancellation = cancellation.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(server_cancellation.cancelled_owned())
            .await
    });

    // The Make target uses an isolated REST database and bounds its child to
    // 15 minutes, including compilation. Core's database belongs to sqlx_test.
    let status = Command::new("make")
        .current_dir(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
        .arg("rest-api/test-api-core-patch")
        .env("CORE_PATCH_TEST_ADDRESS", address.to_string())
        .env_remove("MAKEFLAGS")
        .env_remove("MFLAGS")
        .kill_on_drop(true)
        .status()
        .await;

    cancellation.cancel();
    server
        .await
        .expect("join Core test listener")
        .expect("serve Core test requests");
    assert!(status.expect("run REST integration test").success());
}
