/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::time::Duration;

use carbide_rpc_utils::dhcp::{DhcpConfig, DhcpV6ServerId};

// File startup must save the legacy DUID before a later YAML update removes
// its IPv4 seed. Controller mode with no interfaces avoids sockets and HBN files.
#[tokio::test]
async fn installed_binary_preserves_legacy_identity_across_file_restarts() {
    let directory = tempfile::tempdir().unwrap();
    let live = directory.path().join("dhcp.yaml");
    let sidecar = directory.path().join("dhcp.yaml.duid");
    let legacy = DhcpConfig {
        carbide_dhcp_server: Some("192.0.2.1".parse().unwrap()),
        carbide_provisioning_server_ipv4: Some("192.0.2.2".parse().unwrap()),
        ..Default::default()
    };
    let legacy_identity = [0, 2, 0, 0, 0x16, 0x47, 192, 0, 2, 1];
    let replacement = DhcpConfig {
        dhcpv6_server_id: Some(DhcpV6ServerId::from_remote_id("test-dpu").unwrap()),
        ..Default::default()
    };
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_forge-dhcp-server"));
    command
        .kill_on_drop(true)
        .args(["--mode", "controller"])
        .arg("--dhcp-config")
        .arg(&live);

    for (scenario, config) in [
        ("first startup", legacy),
        ("IPv6-only restart", replacement),
    ] {
        let yaml = serde_yaml::to_string(&config).unwrap();
        std::fs::write(&live, &yaml).unwrap();
        let output = tokio::time::timeout(Duration::from_secs(10), command.output())
            .await
            .expect("file startup did not exit without interfaces")
            .unwrap();
        assert!(
            output.status.success(),
            "{scenario}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(&sidecar).unwrap(),
            legacy_identity,
            "{scenario}"
        );
        assert_eq!(std::fs::read_to_string(&live).unwrap(), yaml, "{scenario}");
    }

    // An unreadable sidecar must stop normal startup too, without replacing it
    // with the different identity from YAML. A directory fails even as root.
    std::fs::remove_file(&sidecar).unwrap();
    std::fs::create_dir(&sidecar).unwrap();
    let yaml = std::fs::read_to_string(&live).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .expect("unreadable identity must fail file startup")
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("dhcp.yaml.duid") && stderr.contains("IoError"),
        "{stderr}"
    );
    assert!(sidecar.is_dir());
    assert_eq!(std::fs::read_to_string(&live).unwrap(), yaml);
}

#[tokio::test]
async fn installed_binary_validates_without_writing_identity_or_configuration() {
    let directory = tempfile::tempdir().unwrap();
    let live = directory.path().join("dhcp.yaml");
    let candidate = directory.path().join("candidate.yaml");
    let host = directory.path().join("host.yaml");
    let sidecar = directory.path().join("dhcp.yaml.duid");
    let legacy = DhcpConfig {
        carbide_dhcp_server: Some("192.0.2.1".parse().unwrap()),
        carbide_provisioning_server_ipv4: Some("192.0.2.2".parse().unwrap()),
        ..Default::default()
    };
    let proposed = DhcpConfig {
        dhcpv6_server_id: Some(DhcpV6ServerId::from_remote_id("test-dpu").unwrap()),
        ..Default::default()
    };
    let live_yaml = serde_yaml::to_string(&legacy).unwrap();
    let candidate_yaml = serde_yaml::to_string(&proposed).unwrap();
    std::fs::write(&live, &live_yaml).unwrap();
    std::fs::write(&candidate, &candidate_yaml).unwrap();
    std::fs::write(
        &host,
        // SLAAC-only hosts do not require stateful DHCPv6 lifetimes.
        "host_interface_id: 11111111-1111-1111-1111-111111111111\nhost_ip_addresses:\n  lo:\n    fqdn: host.example.com\n    ipv6:\n      prefix: '2001:db8::/64'\n",
    )
    .unwrap();
    let validate = |extra_args: Vec<&'static str>| {
        let (candidate, live, host) = (&candidate, &live, &host);
        async move {
            tokio::time::timeout(
                Duration::from_secs(10),
                tokio::process::Command::new(env!("CARGO_BIN_EXE_forge-dhcp-server"))
                    .kill_on_drop(true)
                    .arg("--validate-config")
                    .arg(candidate)
                    .arg("--dhcp-config")
                    .arg(live)
                    .arg("--host-config")
                    .arg(host)
                    .args(extra_args)
                    .output(),
            )
            .await
            .expect("candidate validation did not exit")
            .unwrap()
        }
    };
    let result = validate(vec![]).await;
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(!sidecar.exists());
    assert_eq!(std::fs::read_to_string(&live).unwrap(), live_yaml);
    assert_eq!(std::fs::read_to_string(&candidate).unwrap(), candidate_yaml);

    let result = validate(vec![
        "--mode",
        "controller",
        "--interfaces",
        "lo",
        "--interfaces",
        "other",
    ])
    .await;
    assert!(!result.status.success());
    assert!(
        String::from_utf8_lossy(&result.stderr)
            .contains("MultipleInterfacesProvidedOneSupported(2)")
    );

    std::fs::write(&candidate, "[").unwrap();
    let result = validate(vec![]).await;
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("candidate.yaml") && stderr.contains("SerdeYaml"),
        "{stderr}"
    );
    std::fs::write(&candidate, &candidate_yaml).unwrap();

    // A persisted identity error must also reach the file writer's preflight.
    std::fs::write(&sidecar, b"corrupt identity").unwrap();
    let result = validate(vec![]).await;
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("dhcp.yaml.duid") && stderr.contains("InvalidServerIdentifier"),
        "{stderr}"
    );
    assert_eq!(std::fs::read(&sidecar).unwrap(), b"corrupt identity");
    assert_eq!(std::fs::read_to_string(&live).unwrap(), live_yaml);
}

// Damaged YAML may leave control available only when the saved DUID is usable;
// accepting a different identity would break clients that selected this server.
#[tokio::test]
async fn grpc_startup_keeps_control_available_with_bad_yaml_and_a_valid_identity() {
    let directory = tempfile::tempdir().unwrap();
    let live = directory.path().join("dhcp.yaml");
    let sidecar = directory.path().join("dhcp.yaml.duid");
    let identity = DhcpV6ServerId::from_remote_id("test-dpu").unwrap();
    std::fs::write(&live, "invalid DHCP YAML").unwrap();
    std::fs::write(&sidecar, identity.as_bytes()).unwrap();

    let reservation = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    let endpoint = tonic::transport::Endpoint::from_shared(format!("http://{address}")).unwrap();
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_forge-dhcp-server"));
    command
        .kill_on_drop(true)
        .arg("--grpc-listen-addr")
        .arg(address.to_string())
        .arg("--dhcp-config")
        .arg(&live)
        .arg("--host-config")
        .arg(directory.path().join("host.yaml"))
        .args(["--interfaces", "lo"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    drop(reservation);
    let mut child = command.spawn().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(10), async {
        let channel = loop {
            if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
                return Err(format!("control server exited during startup: {status}"));
            }
            if let Ok(channel) = endpoint.connect().await {
                break channel;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let mut client = tonic::client::Grpc::new(channel);
        client.ready().await.map_err(|error| error.to_string())?;
        // StopServer has empty protobuf messages and does not start listeners.
        let _: tonic::Response<()> = client
            .unary(
                tonic::Request::new(()),
                tonic::codegen::http::uri::PathAndQuery::from_static(
                    "/dhcp_server_control.DhcpServerControl/StopServer",
                ),
                tonic_prost::ProstCodec::default(),
            )
            .await
            .map_err(|error| error.to_string())?;
        Ok::<_, String>(())
    })
    .await;

    // Reap the child before asserting, including startup and RPC failures.
    if child.try_wait().unwrap().is_none() {
        child.start_kill().unwrap();
    }
    let output = tokio::time::timeout(Duration::from_secs(5), child.wait_with_output())
        .await
        .expect("control server did not stop")
        .unwrap();
    assert!(
        matches!(result, Ok(Ok(()))),
        "control request failed: {result:?}; stderr: {}",
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "invalid DHCP YAML");
    assert_eq!(std::fs::read(&sidecar).unwrap(), identity.as_bytes());

    // Without a recoverable identity, startup cannot safely accept a new DUID.
    std::fs::remove_file(&sidecar).unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .expect("bad YAML without a saved identity must fail startup")
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("dhcp.yaml") && stderr.contains("SerdeYaml"),
        "{stderr}"
    );
    assert!(!sidecar.exists());
    assert_eq!(std::fs::read_to_string(&live).unwrap(), "invalid DHCP YAML");

    std::fs::write(&sidecar, b"corrupt identity").unwrap();
    let output = tokio::time::timeout(Duration::from_secs(10), command.output())
        .await
        .expect("corrupt identity must fail startup")
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("dhcp.yaml.duid") && stderr.contains("InvalidServerIdentifier"),
        "{stderr}"
    );
    assert_eq!(std::fs::read(&sidecar).unwrap(), b"corrupt identity");
}
