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
use std::process::Stdio;
use std::time::Duration;

use futures::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message as _;
use rpc::dns::{Domain, DomainList, DomainSearchQuery};
use rpc::forge::BuildInfo;
use tokio::net::TcpListener;
use tokio::process::Command;

#[tokio::test]
async fn domain_show_displays_vpc_ownership_in_table() {
    let vpc_id = "12345678-1234-5678-90ab-cdef01234567";
    let domains = DomainList {
        domains: vec![
            Domain {
                id: Some(
                    "abcdef01-2345-6789-abcd-ef0123456789"
                        .parse()
                        .expect("domain UUID"),
                ),
                name: "tenant.example".to_string(),
                vpc_id: Some(vpc_id.parse().expect("VPC UUID")),
                ..Default::default()
            },
            Domain {
                id: Some(
                    "22345678-1234-5678-90ab-cdef01234567"
                        .parse()
                        .expect("domain UUID"),
                ),
                name: "site.example".to_string(),
                vpc_id: None,
                ..Default::default()
            },
        ],
    };
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind private Core listener");
    let api_url = format!("http://{}", listener.local_addr().expect("bound listener"));
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.expect("accept CLI connection");
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request: Request<Incoming>| {
                    let domains = domains.clone();
                    async move {
                        let payload = match request.uri().path() {
                            "/forge.Forge/Version" => BuildInfo::default().encode_to_vec(),
                            "/forge.Forge/FindDomain" => {
                                let body = request
                                    .into_body()
                                    .collect()
                                    .await
                                    .expect("read request")
                                    .to_bytes();
                                assert_eq!(body.first(), Some(&0));
                                let request = DomainSearchQuery::decode(body.slice(5..))
                                    .expect("domain search request decodes");
                                assert_eq!(request, DomainSearchQuery::default());
                                domains.encode_to_vec()
                            }
                            path => panic!("unexpected Core request: {path}"),
                        };
                        let mut data = vec![0];
                        data.extend_from_slice(
                            &u32::try_from(payload.len())
                                .expect("small fixture")
                                .to_be_bytes(),
                        );
                        data.extend_from_slice(&payload);
                        let mut trailers = hyper::HeaderMap::new();
                        trailers.insert("grpc-status", header::HeaderValue::from_static("0"));
                        let body = StreamBody::new(stream::iter([
                            Ok::<_, Infallible>(Frame::data(Bytes::from(data))),
                            Ok(Frame::trailers(trailers)),
                        ]));
                        Ok::<_, Infallible>(
                            Response::builder()
                                .header(header::CONTENT_TYPE, "application/grpc+tonic")
                                .body(body)
                                .expect("valid gRPC response"),
                        )
                    }
                }),
            )
            .await
            .expect("serve domain search");
    });
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        Command::new(env!("CARGO_BIN_EXE_nico-admin-cli"))
            // Ignore per-user CLI configuration and proxy settings for the mock.
            .env_clear()
            .args([
                "--api-url",
                &api_url,
                "--root-ca-path",
                "/unused/ca.crt",
                "--client-cert-path",
                "/unused/client.crt",
                "--client-key-path",
                "/unused/client.key",
                "domain",
                "show",
            ])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await;
    server.abort();
    if let Err(error) = server.await {
        assert!(error.is_cancelled(), "mock Core server failed: {error}");
    }
    let output = output
        .expect("domain show completes within ten seconds")
        .expect("run domain show");
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    let output = String::from_utf8(output.stdout).expect("command output is UTF-8");
    let mut rows = output
        .lines()
        .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
        .map(|line| line.split('|').map(str::trim).collect::<Vec<_>>());
    assert_eq!(
        rows.next(),
        Some(vec!["Id", "Name", "Vpc", "Created"]),
        "{output}"
    );
    let owners: Vec<_> = rows
        .map(|row| {
            assert_eq!(row.len(), 4, "{output}");
            (row[1], row[2])
        })
        .collect();
    assert_eq!(
        owners,
        [("tenant.example", vpc_id), ("site.example", "")],
        "{output}",
    );
}
