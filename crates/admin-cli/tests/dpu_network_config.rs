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
use rpc::forge::{
    AddressFamily, BuildInfo, FlatInterfaceConfig, FlatInterfaceIpv6Config, InterfaceAddressConfig,
    ManagedHostNetworkConfig, ManagedHostNetworkConfigRequest, ManagedHostNetworkConfigResponse,
};
use tokio::net::TcpListener;
use tokio::process::Command;

const DPU_ID: &str = "fm100dtjtiaehv1n5vh67tbmqq4eabcjdng40f7jupsadbedhruh6rag1l0";

#[tokio::test]
#[allow(deprecated)]
async fn network_config_displays_ipv6_and_preserves_compatibility_fields() {
    let legacy_ipv6 = FlatInterfaceIpv6Config {
        ip: "2001:db8:3::10".to_string(),
        interface_prefix: "2001:db8:3::10/128".to_string(),
        svi_ip: Some("2001:db8:3::1/64".to_string()),
    };
    let ipv4 = InterfaceAddressConfig {
        address_family: AddressFamily::V4.into(),
        ip: "192.0.2.10".to_string(),
        prefix: "192.0.2.0/24".to_string(),
        ..Default::default()
    };
    let config = ManagedHostNetworkConfigResponse {
        managed_host_config: Some(ManagedHostNetworkConfig {
            loopback_ip: "192.0.2.254".to_string(),
            loopback_ip_v6: Some("2001:db8:ffff::1".to_string()),
            ..Default::default()
        }),
        admin_interface: Some(FlatInterfaceConfig {
            ip: Some("192.0.2.10".to_string()),
            gateway: Some("192.0.2.1/24".to_string()),
            prefix: Some("192.0.2.0/24".to_string()),
            svi_ip: Some("192.0.2.2".to_string()),
            tenant_vrf_loopback_ip: Some("192.0.2.3".to_string()),
            ipv6_interface_config: Some(legacy_ipv6.clone()),
            addresses: vec![
                ipv4.clone(),
                InterfaceAddressConfig {
                    address_family: AddressFamily::V6.into(),
                    ip: "2001:db8:1::10".to_string(),
                    interface_prefix: "2001:db8:1::10/128".to_string(),
                    prefix: "2001:db8:1::/64".to_string(),
                    svi_ip: Some("2001:db8:1::1/64".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        tenant_interfaces: vec![
            FlatInterfaceConfig {
                // SLAAC leaves the host address empty even when an older
                // sidecar contains one. The current entry must stay intact.
                addresses: vec![InterfaceAddressConfig {
                    address_family: AddressFamily::V6.into(),
                    interface_prefix: "2001:db8:2::/64".to_string(),
                    prefix: "2001:db8:2::/64".to_string(),
                    tenant_vrf_loopback_ip: Some("2001:db8:ffff::2".to_string()),
                    ..Default::default()
                }],
                ipv6_interface_config: Some(FlatInterfaceIpv6Config {
                    interface_prefix: String::new(),
                    ..legacy_ipv6.clone()
                }),
                ..Default::default()
            },
            FlatInterfaceConfig {
                ipv6_interface_config: Some(legacy_ipv6.clone()),
                ..Default::default()
            },
            FlatInterfaceConfig {
                addresses: vec![ipv4.clone()],
                ..Default::default()
            },
            FlatInterfaceConfig {
                addresses: vec![ipv4],
                ipv6_interface_config: Some(FlatInterfaceIpv6Config {
                    interface_prefix: String::new(),
                    ..legacy_ipv6
                }),
                ..Default::default()
            },
        ],
        ..Default::default()
    };

    let output = network_config_output(config).await;
    let (host, interfaces) = output
        .split_once("Admin Interface:")
        .expect("admin interface heading is rendered");
    let (admin, tenants) = interfaces
        .split_once("Tenant Interfaces:")
        .expect("tenant interfaces heading is rendered");
    let tenants: Vec<_> = tenants.split("Interface #").skip(1).collect();
    assert_eq!(tenants.len(), 4, "one table per tenant interface");

    for (scenario, table, expected) in [
        (
            "DPU loopbacks",
            host,
            &[
                ("Config Loopback IP", "192.0.2.254"),
                ("Config Loopback IPv6", "2001:db8:ffff::1"),
            ][..],
        ),
        (
            "dual-stack admin interface",
            admin,
            &[
                ("IP", "192.0.2.10"),
                ("Gateway", "192.0.2.1/24"),
                ("Prefix", "192.0.2.0/24"),
                ("SVI IP", "192.0.2.2"),
                ("Tenant VRF Loopback", "192.0.2.3"),
                ("IPv6 IP", "2001:db8:1::10"),
                ("IPv6 Interface Prefix", "2001:db8:1::10/128"),
                ("IPv6 Prefix", "2001:db8:1::/64"),
                ("IPv6 SVI IP", "2001:db8:1::1/64"),
                ("IPv6 Tenant VRF Loopback", ""),
            ],
        ),
        (
            "IPv6-only SLAAC interface preserves empty current fields",
            tenants[0],
            &[
                ("IP", ""),
                ("Gateway", ""),
                ("Prefix", ""),
                ("Tenant VRF Loopback", ""),
                ("IPv6 IP", ""),
                ("IPv6 Interface Prefix", "2001:db8:2::/64"),
                ("IPv6 Prefix", "2001:db8:2::/64"),
                ("IPv6 SVI IP", ""),
                ("IPv6 Tenant VRF Loopback", "2001:db8:ffff::2"),
            ],
        ),
        (
            "legacy IPv6 sidecar",
            tenants[1],
            &[
                ("IPv6 IP", "2001:db8:3::10"),
                ("IPv6 Interface Prefix", "2001:db8:3::10/128"),
                ("IPv6 Prefix", ""),
                ("IPv6 SVI IP", "2001:db8:3::1/64"),
                ("IPv6 Tenant VRF Loopback", ""),
            ],
        ),
        (
            "IPv4-only interface has no IPv6 configuration",
            tenants[2],
            &[
                ("IPv6 IP", ""),
                ("IPv6 Interface Prefix", ""),
                ("IPv6 Prefix", ""),
                ("IPv6 SVI IP", ""),
                ("IPv6 Tenant VRF Loopback", ""),
            ],
        ),
        (
            "prefixless IPv6 sidecar omitted from current address entries",
            tenants[3],
            &[
                ("IPv6 IP", "2001:db8:3::10"),
                ("IPv6 Interface Prefix", ""),
                ("IPv6 Prefix", ""),
                ("IPv6 SVI IP", "2001:db8:3::1/64"),
                ("IPv6 Tenant VRF Loopback", ""),
            ],
        ),
    ] {
        let rows: Vec<_> = table
            .lines()
            .filter_map(|line| line.trim().strip_prefix('|')?.strip_suffix('|'))
            .map(|line| line.split('|').map(str::trim).collect::<Vec<_>>())
            .collect();
        for &(label, value) in expected {
            assert!(
                rows.iter().any(|row| row == &[label, value]),
                "{scenario}: missing row {label:?} = {value:?} in:\n{table}",
            );
        }
    }
}

async fn network_config_output(config: ManagedHostNetworkConfigResponse) -> String {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind private Core listener");
    let api_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.unwrap();
        http2::Builder::new(TokioExecutor::new())
            .serve_connection(
                TokioIo::new(connection),
                service_fn(move |request: Request<Incoming>| {
                    let config = config.clone();
                    async move {
                        let payload = match request.uri().path() {
                            "/forge.Forge/Version" => BuildInfo::default().encode_to_vec(),
                            "/forge.Forge/GetManagedHostNetworkConfig" => {
                                let body = request.into_body().collect().await.unwrap().to_bytes();
                                assert_eq!(body.first(), Some(&0));
                                let request =
                                    ManagedHostNetworkConfigRequest::decode(body.slice(5..))
                                        .expect("network configuration request decodes");
                                assert_eq!(request.dpu_machine_id, Some(DPU_ID.parse().unwrap()));
                                config.encode_to_vec()
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
            .expect("serve DPU network configuration");
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
                "dpu",
                "network",
                "config",
                "--machine-id",
                DPU_ID,
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
        .expect("DPU network configuration command completes within ten seconds")
        .expect("run DPU network configuration command");
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("command output is UTF-8")
}
