/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::convert::Infallible;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::stream;
use http_body_util::{BodyExt, StreamBody};
use hyper::body::{Bytes, Frame, Incoming};
use hyper::server::conn::http2;
use hyper::service::service_fn;
use hyper::{Request, Response, header};
use hyper_util::rt::{TokioExecutor, TokioIo};
use prost::Message;
use rpc::forge;
use tokio::net::TcpListener;
use tokio::process::Command;

#[tokio::test]
async fn spx_maintenance_public_commands_send_requests_and_show_requested_state() {
    let machine = "fm100psbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80";
    let cases = [
        ("show", vec!["show"], None, true),
        ("show without DPUs", vec!["show"], None, false),
        (
            "all rails off",
            vec!["multiplane", "rail", "all", "plane", "6", "off"],
            Some((None, Some((6, 6)), false)),
            true,
        ),
        (
            "single rail on",
            vec![
                "multiplane",
                "rail",
                "2",
                "plane",
                "6",
                "on",
                "--reference",
                "https://tickets.example.com/SPX-42",
            ],
            Some((Some((2, 2)), Some((6, 6)), true)),
            true,
        ),
        (
            "all planes on one rail",
            vec![
                "multiplane",
                "rail",
                "2",
                "plane",
                "all",
                "on",
                "--reference",
                "https://tickets.example.com/SPX-42",
            ],
            Some((Some((2, 2)), None, true)),
            true,
        ),
        (
            "rail and plane ranges",
            vec![
                "multiplane",
                "rail",
                "0-2",
                "plane",
                "1-4",
                "off",
                "--reference",
                "https://tickets.example.com/SPX-42",
            ],
            Some((Some((0, 2)), Some((1, 4)), false)),
            true,
        ),
        (
            "all planes on all rails",
            vec!["multiplane", "rail", "all", "plane", "all", "off"],
            Some((None, None, false)),
            true,
        ),
    ];
    for (scenario, arguments, expected, has_dpus) in cases {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let api_url = format!("http://{}", listener.local_addr().unwrap());
        let called = Arc::new(Mutex::new(false));
        let server_called = called.clone();
        let first_dpu = "fm100dsbtmb15tgh6q5duqb8ke5grng7ksd96hetbeie9nc5pvcca6eol80";
        let second_dpu = "fm100dt038bg3qsho433vkg684heguv282qaggmrsh2ugn1qk096n2c6hcg";
        let revision = "V2-T1791792000000000";
        let configuration = forge::MachineSpxMultiplane {
            machine_id: Some(machine.parse().unwrap()),
            interfaces: vec![
                forge::MachineSpxMultiplaneInterface {
                    logical_ifname: "eth_r2_p6".into(),
                    rail: 2,
                    plane: 6,
                    cfg_maintenance_mode: Some(false),
                    reference: Some("https://tickets.example.com/SPX-42".into()),
                    revision: revision.into(),
                },
                forge::MachineSpxMultiplaneInterface {
                    logical_ifname: "eth_r0_p0".into(),
                    rail: 0,
                    plane: 0,
                    cfg_maintenance_mode: Some(true),
                    revision: "V1-T1791791900000000".into(),
                    ..Default::default()
                },
                forge::MachineSpxMultiplaneInterface {
                    logical_ifname: "eth_r3_p0".into(),
                    rail: 3,
                    plane: 0,
                    cfg_maintenance_mode: None,
                    revision: "V1-T1791791900000000".into(),
                    ..Default::default()
                },
                forge::MachineSpxMultiplaneInterface {
                    logical_ifname: "eth_r1_p0".into(),
                    rail: 1,
                    plane: 0,
                    cfg_maintenance_mode: Some(true),
                    revision: "V1-T1791791900000000".into(),
                    ..Default::default()
                },
            ],
            dpu_statuses: if has_dpus {
                vec![
                    forge::MachineSpxMultiplaneDpuStatus {
                        dpu_machine_id: Some(first_dpu.parse().unwrap()),
                        interfaces: vec![
                            forge::AstraMultiplaneInterfaceStatus {
                                logical_ifname: "eth_r2_p6".into(),
                                maintenance_mode: Some(false),
                                revision: revision.into(),
                                netdev: Some("A56p0".into()),
                                observed_at: Some(chrono::Utc::now().into()),
                                ..Default::default()
                            },
                            forge::AstraMultiplaneInterfaceStatus {
                                logical_ifname: "eth_r0_p0".into(),
                                ..Default::default()
                            },
                            forge::AstraMultiplaneInterfaceStatus {
                                logical_ifname: "eth_r3_p0".into(),
                                maintenance_mode: Some(false),
                                revision: "V1-T1791791900000000".into(),
                                observed_at: Some(chrono::Utc::now().into()),
                                ..Default::default()
                            },
                            forge::AstraMultiplaneInterfaceStatus {
                                logical_ifname: "eth_r1_p0".into(),
                                revision: "V1-T1791791900000000".into(),
                                error: Some("Xplane unavailable".into()),
                                observed_at: Some(chrono::Utc::now().into()),
                                ..Default::default()
                            },
                        ],
                    },
                    forge::MachineSpxMultiplaneDpuStatus {
                        dpu_machine_id: Some(second_dpu.parse().unwrap()),
                        interfaces: vec![forge::AstraMultiplaneInterfaceStatus {
                            logical_ifname: "eth_r2_p6".into(),
                            revision: revision.into(),
                            error: Some("missing logical interface".into()),
                            observed_at: Some(chrono::Utc::now().into()),
                            ..Default::default()
                        }],
                    },
                ]
            } else {
                Vec::new()
            },
        };
        let server = tokio::spawn(async move {
            let (connection, _) = listener.accept().await.unwrap();
            http2::Builder::new(TokioExecutor::new()).serve_connection(TokioIo::new(connection), service_fn(move |request: Request<Incoming>| {
                let configuration = configuration.clone();
                let called = server_called.clone();
                async move {
                    let method = request.uri().path().to_string();
                    let payload = if method == "/forge.Forge/Version" { forge::BuildInfo::default().encode_to_vec() } else {
                        let body = request.into_body().collect().await.unwrap().to_bytes();
                        if let Some((rail, plane, mode)) = expected {
                            assert_eq!(method, "/forge.Forge/UpdateMachineSpxMultiplaneMaintenance");
                            let request = forge::MachineSpxMultiplaneMaintenanceUpdateRequest::decode(body.slice(5..)).unwrap();
                            assert_eq!(request.machine_id, Some(machine.parse().unwrap()));
                            use forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector;
                            assert_eq!(request.plane_selector, Some(match plane { None => PlaneSelector::AllPlanes(()), Some((start, end)) if start == end => PlaneSelector::Plane(start), Some((start, end)) => PlaneSelector::PlaneRange(forge::SpxMultiplaneRange { start: Some(start), end: Some(end) }) }));
                            assert_eq!(request.maintenance_mode, Some(mode));
                            use forge::machine_spx_multiplane_maintenance_update_request::RailSelector;
                            assert_eq!(request.rail_selector, Some(match rail { None => RailSelector::AllRails(()), Some((start, end)) if start == end => RailSelector::Rail(start), Some((start, end)) => RailSelector::RailRange(forge::SpxMultiplaneRange { start: Some(start), end: Some(end) }) }));
                            assert_eq!(request.reference.as_deref(), rail.map(|_| "https://tickets.example.com/SPX-42"));
                        } else {
                            assert_eq!(method, "/forge.Forge/GetMachineSpxMultiplane");
                            let request = forge::MachineSpxMultiplaneRequest::decode(body.slice(5..)).unwrap();
                            assert_eq!(request.machine_id, Some(machine.parse().unwrap()));
                        }
                        *called.lock().unwrap() = true;
                        configuration.encode_to_vec()
                    };
                    let mut data = vec![0];
                    data.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_be_bytes());
                    data.extend_from_slice(&payload);
                    let mut trailers = hyper::HeaderMap::new();
                    trailers.insert("grpc-status", header::HeaderValue::from_static("0"));
                    let body = StreamBody::new(stream::iter([Ok::<_, Infallible>(Frame::data(Bytes::from(data))), Ok(Frame::trailers(trailers))]));
                    Ok::<_, Infallible>(Response::builder().header(header::CONTENT_TYPE, "application/grpc+tonic").body(body).unwrap())
                }
            })).await.unwrap();
        });
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            Command::new(env!("CARGO_BIN_EXE_nico-admin-cli"))
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
                    "spx-maintenance",
                    "--machine-id",
                    machine,
                ])
                .args(arguments)
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output(),
        )
        .await;
        server.abort();
        if let Err(error) = server.await {
            assert!(error.is_cancelled(), "{scenario}: server failed: {error}");
        }
        let output = output
            .expect("command completed within ten seconds")
            .unwrap();
        assert!(
            output.status.success(),
            "{scenario}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            *called.lock().unwrap(),
            "{scenario}: public command must call Core"
        );
        let text = String::from_utf8(output.stdout).unwrap();
        for value in [
            "Logical interface",
            "Requested maintenance mode",
            "Reference",
            "Revision",
            "Config updated at (UTC)",
            "unset",
            "eth_r2_p6",
            "eth_r0_p0",
            "off",
            "on",
            "https://tickets.example.com/SPX-42",
            revision,
        ] {
            assert!(
                text.contains(value),
                "{scenario}: missing {value} in {text}"
            );
        }
        let desired_table = text.split("DPU ").next().unwrap();
        for (logical, updated_at) in [("eth_r0_p0", "2026-10-12 07:58:20"), ("eth_r3_p0", "-")] {
            let row = desired_table
                .lines()
                .find(|line| line.contains(logical))
                .unwrap();
            assert_eq!(
                row.split('|').nth(7).unwrap().trim(),
                updated_at,
                "{scenario}: {row}"
            );
        }
        assert!(
            !desired_table.contains("Status"),
            "host configuration has no aggregate status"
        );
        if !has_dpus {
            assert!(text.contains("No BF4 Astra DPUs present."));
            assert!(!text.contains("Applied maintenance mode"));
            continue;
        }
        for value in [
            "Applied maintenance mode",
            "Observed revision",
            "Observed at",
            "Status",
            "Netdev",
            "Error",
            "unknown",
            "A56p0",
            first_dpu,
            second_dpu,
        ] {
            assert!(
                text.contains(value),
                "{scenario}: missing {value} in {text}"
            );
        }
        let first_section = text
            .split(&format!("DPU {first_dpu}"))
            .nth(1)
            .unwrap()
            .split(&format!("DPU {second_dpu}"))
            .next()
            .unwrap();
        for (name, status) in [
            ("eth_r2_p6", "Applied"),
            ("eth_r0_p0", "Pending"),
            ("eth_r1_p0", "Error"),
            ("eth_r3_p0", "Observed"),
        ] {
            let row = first_section
                .lines()
                .find(|line| line.contains(name))
                .unwrap();
            assert!(
                row.contains(status),
                "{scenario}: incorrect first-DPU status in {row}"
            );
            let timestamp = row.split('|').nth(6).unwrap().trim();
            if status == "Pending" {
                assert!(row.contains("unknown") && timestamp.is_empty());
            } else {
                assert!(
                    !timestamp.is_empty(),
                    "{scenario}: observed timestamp must be visible"
                );
            }
        }
        let second_section = text.split(&format!("DPU {second_dpu}")).nth(1).unwrap();
        let row = second_section
            .lines()
            .find(|line| line.contains("eth_r2_p6"))
            .unwrap();
        assert!(
            row.contains("Error") && row.contains("missing logical interface"),
            "{scenario}: DPU observations must remain distinguishable"
        );
        assert!(text.contains("Xplane unavailable"));
    }
}
