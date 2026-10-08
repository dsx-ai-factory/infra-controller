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
use rpc::forge;
use tokio::net::TcpListener;
use tokio::process::Command;

#[tokio::test]
async fn instance_detail_displays_reported_prefixes_separately_from_addresses() {
    let instance_id = "12345678-1234-5678-90ab-cdef01234567".parse().unwrap();
    let instance = forge::Instance {
        id: Some(instance_id),
        config: Some(forge::InstanceConfig {
            network: Some(forge::InstanceNetworkConfig {
                interfaces: vec![Default::default(), Default::default()],
                ..Default::default()
            }),
            ..Default::default()
        }),
        status: Some(forge::InstanceStatus {
            network: Some(forge::InstanceNetworkStatus {
                interfaces: vec![
                    forge::InstanceInterfaceStatus {
                        addresses: vec!["192.0.2.10".into()],
                        prefixes: vec!["192.0.2.0/24".into(), "2001:db8::/64".into()],
                        ..Default::default()
                    },
                    forge::InstanceInterfaceStatus::default(),
                ],
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let api_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.unwrap();
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request: Request<Incoming>| {
                    let instance = instance.clone();
                    async move {
                        let payload = match request.uri().path() {
                            "/forge.Forge/Version" => forge::BuildInfo::default().encode_to_vec(),
                            "/forge.Forge/FindInstancesByIds" => {
                                let body = request.into_body().collect().await.unwrap().to_bytes();
                                assert_eq!(body.first(), Some(&0));
                                let request =
                                    forge::InstancesByIdsRequest::decode(body.slice(5..)).unwrap();
                                assert_eq!(request.instance_ids, vec![instance_id]);
                                forge::InstanceList {
                                    instances: vec![instance],
                                }
                                .encode_to_vec()
                            }
                            path => panic!("unexpected Core request: {path}"),
                        };
                        let mut data = vec![0];
                        data.extend_from_slice(
                            &u32::try_from(payload.len()).unwrap().to_be_bytes(),
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
                                .unwrap(),
                        )
                    }
                }),
            )
            .await
            .unwrap();
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
                "instance",
                "show",
                &instance_id.to_string(),
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
        .expect("instance show completes")
        .expect("run instance show");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = String::from_utf8(output.stdout).unwrap();
    for (label, expected) in [
        ("ADDRESSES", vec!["192.0.2.10", ""]),
        ("PREFIXES", vec!["192.0.2.0/24, 2001:db8::/64", ""]),
    ] {
        let values: Vec<_> = output
            .lines()
            .filter_map(|line| line.trim().split_once(':'))
            .filter_map(|(key, value)| (key.trim() == label).then_some(value.trim()))
            .collect();
        assert_eq!(values, expected, "{label} rows");
    }
}
