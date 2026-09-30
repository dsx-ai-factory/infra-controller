/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Disposable cross-language Domain cancellation test: the Go REST handler
//! dispatches its actual Site workflow/activity through a loopback Core server.
//! No shared service or Temporal frontend is used.
use std::net::TcpListener;
use std::path::Path;
use std::process::Command;

use carbide_authn::middleware::Principal;
use carbide_test_harness::prelude::*;
use rpc::forge::forge_server::Forge;
use tonic::{Request, Status};

#[sqlx_test]
async fn domain_rest_site_core_joined_cancellation(pool: PgPool) {
    let env = TestHarness::builder(pool.clone()).build().await;
    let listener = TcpListener::bind("127.0.0.1:0").expect("fixture loopback bind");
    let addr = listener.local_addr().expect("fixture address");
    listener
        .set_nonblocking(true)
        .expect("fixture nonblocking socket");
    // Force a genuine Core namespace conflict in the second scenario.
    env.api()
        .create_domain(Request::new(rpc::protos::dns::CreateDomainRequest {
            name: "joined-conflict.example.com".into(),
            default_ttl: None,
            reserved_id: None,
        }))
        .await
        .expect("Core conflicting legacy Domain fixture");
    let api = env.api_arc();
    let server = tokio::spawn(async move {
        tonic::transport::Server::builder()
            // This isolates transport/error mapping, not TLS/mTLS RBAC. Explicitly
            // supply the same SiteAgent AuthContext used by direct Core fixtures.
            .add_service(tonic::service::interceptor(
                rpc::forge::forge_server::ForgeServer::from_arc(api),
                |mut req: Request<()>| -> Result<Request<()>, Status> {
                    req.extensions_mut().insert(carbide_api_core::AuthContext {
                        principals: vec![Principal::SpiffeServiceIdentifier(
                            "elektra-site-agent".into(),
                        )],
                        authorization: None,
                    });
                    Ok(req)
                },
            ))
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(
                tokio::net::TcpListener::from_std(listener).expect("fixture Tokio socket"),
            ))
            .await
            .expect("Core fixture server");
    });
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let rest_dir = manifest.join("../../rest-api");
    let result_file = tempfile::NamedTempFile::new().expect("fixture result file");
    let result_path = result_file.path().to_owned();
    let result = tokio::task::spawn_blocking(move || {
        Command::new("timeout")
            .args([
                "180s",
                "go",
                "test",
                "-p",
                "1",
                "./api/pkg/api/handler",
                "-run",
                "^TestDomainJoinedRealSiteCore$",
                "-count=1",
                "-v",
                "-timeout=150s",
            ])
            .current_dir(rest_dir)
            .env("CORE_JOINED_ADDR", addr.to_string())
            .env("CORE_JOINED_RESULT", result_path)
            .output()
            .expect("go executable/timeout fixture setup")
    })
    .await
    .expect("fixture subprocess join");
    server.abort();
    assert!(
        result.status.success(),
        "joined Go fixture failed: status={} stdout={} stderr={}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    let ids: Vec<String> = std::fs::read_to_string(result_file.path())
        .expect("joined REST test must record both actual reserved IDs")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(ids.len(), 2, "each joined scenario must report one ID");
    for id in ids {
        let uuid = uuid::Uuid::parse_str(&id).expect("recorded reserved UUID");
        let cancelled: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM domain_reserved_id_cancellations WHERE id = $1)",
        )
        .bind(uuid)
        .fetch_one(&pool)
        .await
        .expect("Core cancellation query");
        assert!(cancelled, "Core must commit cancellation for {id}");
        let live: i64 =
            sqlx::query_scalar("SELECT count(*) FROM domains WHERE id = $1 AND deleted IS NULL")
                .bind(uuid)
                .fetch_one(&pool)
                .await
                .expect("Core live domain query");
        assert_eq!(live, 0, "cancelled Core ID must not have a live Domain");
    }
}
