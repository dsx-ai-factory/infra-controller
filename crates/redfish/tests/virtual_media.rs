/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Focused local proofs for the external-DHCP boot transport's write boundaries.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request};
use bmc_mock::test_support::{TEST_MAC_POOL, TestCallbacks, serve_https};
use bmc_mock::{HardwareType, HostMachineInfo, MachineInfo, MachineRouterOptions, machine_router};
use carbide_redfish::virtual_media::{
    VirtualMediaClient, VirtualMediaObservation, VirtualMediaSlot, dispatch_reset_once,
};
use carbide_secrets::credentials::Credentials;
use carbide_utils::redfish::BmcAccessInfo;
use libredfish::{Endpoint, RedfishClientPool};
use serde_json::json;
use tokio::io::AsyncReadExt;

/// A lost reset response must not trigger the HTTP library's automatic retry.
#[tokio::test]
async fn uncertain_reset_submits_exactly_once() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let mut tls = bmc_mock::tls::server_config(None::<&str>).unwrap();
    tls.alpn_protocols = vec![b"http/1.1".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls));
    let submitted = Arc::new(AtomicUsize::new(0));
    let observed = submitted.clone();
    let server = tokio::spawn(async move {
        loop {
            let (socket, _) = listener.accept().await.unwrap();
            let mut socket = acceptor.accept(socket).await.unwrap();
            let mut request = Vec::new();
            loop {
                let mut chunk = [0; 1024];
                let count = socket.read(&mut chunk).await.unwrap();
                if count == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..count]);
                let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                else {
                    continue;
                };
                let header_end = header_end + 4;
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + content_length {
                    assert!(
                        headers.starts_with(
                            "POST /redfish/v1/Systems/1/Actions/ComputerSystem.Reset "
                        )
                    );
                    let body: serde_json::Value =
                        serde_json::from_slice(&request[header_end..]).unwrap();
                    assert_eq!(body, json!({"ResetType": "ForceRestart"}));
                    observed.fetch_add(1, Ordering::SeqCst);
                    break;
                }
            }
            // Deliberately close after accepting the operation, before any response.
            drop(socket);
        }
    });
    let result = tokio::time::timeout(
        Duration::from_secs(5),
        dispatch_reset_once(
            &BmcAccessInfo {
                host: address.ip().to_string(),
                port: Some(address.port()),
                mac_address: "00:11:22:33:44:55".parse().unwrap(),
            },
            Credentials::UsernamePassword {
                username: "local-test".into(),
                password: "local-test".into(),
            },
            "/redfish/v1/Systems/1",
            false,
        ),
    )
    .await
    .unwrap();
    assert!(result.is_err());
    assert_eq!(submitted.load(Ordering::SeqCst), 1);
    server.abort();
}

/// Occupied media and unsupported boot capabilities must fail before any PATCH/POST.
#[tokio::test]
async fn foreign_media_and_unsupported_boot_make_no_mutations() {
    let host = {
        let mut pool = TEST_MAC_POOL.lock().unwrap();
        let hardware_macs = pool.allocate_range_config().unwrap();
        HostMachineInfo::new(
            HardwareType::GenericAmi,
            Vec::new(),
            &mut pool,
            hardware_macs,
        )
    };
    let (router, _) = machine_router(
        &MachineInfo::Host(host),
        Arc::new(TestCallbacks::default()),
        "vm-boundary-test".into(),
        false,
        MachineRouterOptions::default(),
    );
    let writes = Arc::new(AtomicUsize::new(0));
    let observed = writes.clone();
    let router = router.layer(axum::middleware::from_fn(
        move |request: Request<Body>, next: axum::middleware::Next| {
            let observed = observed.clone();
            async move {
                if matches!(
                    *request.method(),
                    Method::POST | Method::PATCH | Method::DELETE
                ) {
                    observed.fetch_add(1, Ordering::SeqCst);
                }
                next.run(request).await
            }
        },
    ));
    let (_server, url) = serve_https("virtual-media-boundaries", router);
    let client = RedfishClientPool::builder()
        .danger_accept_invalid_certs()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap()
        .create_client(Endpoint {
            host: url.host_str().unwrap().into(),
            port: url.port(),
            user: None,
            password: None,
        })
        .await
        .unwrap();
    let baseline = writes.load(Ordering::SeqCst);
    let media = VirtualMediaClient::new(client.as_ref());
    let slot = VirtualMediaSlot {
        uri: "/redfish/v1/Managers/1/VirtualMedia/CD1".into(),
        image: Some("https://media.invalid/foreign.iso".into()),
        inserted: true,
        optical: true,
        insert_target: Some(
            "/redfish/v1/Managers/1/VirtualMedia/CD1/Actions/VirtualMedia.InsertMedia".into(),
        ),
        eject_target: Some(
            "/redfish/v1/Managers/1/VirtualMedia/CD1/Actions/VirtualMedia.EjectMedia".into(),
        ),
        lenovo_ext: false,
    };
    assert!(
        media
            .insert(&slot, "https://media.invalid/owned.iso")
            .await
            .is_err()
    );
    assert!(
        media
            .eject(&slot, "https://media.invalid/owned.iso")
            .await
            .is_err()
    );
    let mut cannot_eject = slot.clone();
    cannot_eject.image = None;
    cannot_eject.inserted = false;
    cannot_eject.eject_target = None;
    assert!(!cannot_eject.available());
    assert!(
        media
            .insert(&cannot_eject, "https://media.invalid/owned.iso")
            .await
            .is_err()
    );
    let mut observation = VirtualMediaObservation {
        system_uri: "/redfish/v1/Systems/1".into(),
        boot: json!({}),
        power_state: None,
        serial_number: None,
        architecture: None,
        boot_targets: vec!["Cd".into()],
        slots: vec![],
    };
    assert!(media.arm(&observation, "Cd").await.is_err());
    observation.boot = json!({"BootSourceOverrideMode": "UEFI", "BootSourceOverrideEnabled@Redfish.AllowableValues": ["Disabled", "Continuous"]});
    assert!(media.arm(&observation, "Cd").await.is_err());
    observation.boot = json!({"BootSourceOverrideMode": "UEFI", "BootSourceOverrideEnabled": "Once", "BootSourceOverrideTarget": "Cd"});
    assert!(observation.optical_boot_armed());
    observation.boot["BootSourceOverrideMode"] = json!("Legacy");
    assert!(!observation.optical_boot_armed());
    assert_eq!(writes.load(Ordering::SeqCst), baseline);
}
