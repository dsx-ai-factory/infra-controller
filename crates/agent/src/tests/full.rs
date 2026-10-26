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

use std::fs;
use std::io::Write;
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::extract::State as AxumState;
use axum::http::{StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use carbide_network::virtualization::{VpcVirtualizationType, get_svi_ip};
use carbide_uuid::domain::DomainId;
use carbide_uuid::machine::{DpuMachineId, MachineId, MachineInterfaceId};
use carbide_uuid::network::NetworkSegmentId;
use chrono::{DateTime, TimeZone, Utc};
use eyre::WrapErr;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioExecutor;
use ipnetwork::IpNetwork;
use prost::Message;
use rpc::forge::{
    DpuInfo, FlatInterfaceNetworkSecurityGroupConfig, InterfaceAssociationType, InterfaceType,
};
use rpc::{Timestamp, common as rpc_common};
use tokio::sync::Mutex;

use crate::tests::common;
use crate::util::compare_lines;

#[derive(Default, Debug)]
struct State {
    has_discovered: bool,
    has_checked_for_upgrade: bool,
    num_netconf_fetches: AtomicUsize,
    num_health_reports: AtomicUsize,
    num_get_dpu_ips: AtomicUsize,
    virtualization_type: VpcVirtualizationType,
    loopback_ip_override: Option<&'static str>,
    last_network_status: Option<rpc::forge::DpuNetworkStatus>,
    reject_next_network_status: bool,
    network_status_reports: Vec<(Instant, bool)>,
    reject_dhcp_update: bool,
    dhcp_updates: Vec<(bool, Bytes)>,
    include_ipv6_tenant: bool,
    quarantine_host: bool,
    post_config_wait_seen: bool,
    astra_config: Option<rpc::forge::AstraConfig>,
}

const DHCP_GATE_CONFIG_VERSION: &str = "V1-T1748645613333257";

#[derive(Default, Debug)]
struct TestOut {
    is_skip: bool,
    hbn_root_dir: Option<tempfile::TempDir>,
}

// test_etv_nvue tests that config is being generated successfully
// for the OG networking config, but using nvue templating mechanism.
// NOTE: This is currently a _very_ light test because it takes the
// UseAdminNetwork paths in the template, which leaves out a lot
// of config.  Some of what's missing seems to be covered in
// ethernet_virtualization tests, though.
#[tokio::test(flavor = "multi_thread")]
async fn test_etv_nvue() -> eyre::Result<()> {
    let expected = include_str!("../../templates/tests/full_nvue_startup_etv.yaml.expected");
    test_nvue_generic(VpcVirtualizationType::EthernetVirtualizer, expected).await
}

// test_fnn_l3 tests that config is being generated successfully
// via nvue templating against the FNN L3 template.
#[tokio::test(flavor = "multi_thread")]
async fn test_fnn_l3() -> eyre::Result<()> {
    let expected = include_str!("../../templates/tests/full_nvue_startup_fnn_l3.yaml.expected");
    test_nvue_generic(VpcVirtualizationType::Fnn, expected).await
}

/// A transient DHCP rejection must leave the desired version pending, then
/// retry the identical request after the endpoint recovers.
#[tokio::test(flavor = "multi_thread")]
async fn dhcp_unavailability_retries_the_same_ipv6_network_configuration() -> eyre::Result<()> {
    network_update_retries_the_same_configuration(false).await
}

/// Failed IPv6 readiness must not withhold quarantine or its OVS transition.
/// Readiness and route recovery must retry the same version before acknowledging it.
#[tokio::test(flavor = "multi_thread")]
async fn fmds_failure_applies_quarantine_and_retries_the_same_network_configuration()
-> eyre::Result<()> {
    network_update_retries_the_same_configuration(true).await
}

/// An actual Weave failure must reach Core without withholding successful
/// HBN/DHCP acknowledgements, and the same desired configuration must recover.
#[tokio::test(flavor = "multi_thread")]
async fn astra_failure_reports_without_blocking_network_configuration_and_recovers()
-> eyre::Result<()> {
    let repo_root = std::env::var("REPO_ROOT")
        .or_else(|_| std::env::var("CONTAINER_REPO_ROOT"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    eyre::ensure!(
        repo_root.join("dev/bin/ip").is_file(),
        "astra recovery test requires repository fixtures; set REPO_ROOT to the checkout"
    );
    if std::env::var_os("ASTRA_RECOVERY_TEST_CHILD").is_none() {
        // Keep fixture commands and environment overrides in a child process
        // so those overrides cannot affect other tests in the parent process.
        let inherited_path =
            std::env::var_os("PATH").ok_or_else(|| eyre::eyre!("missing test PATH"))?;
        let path = std::env::join_paths(
            std::iter::once(repo_root.join("dev/bin"))
                .chain(std::env::split_paths(&inherited_path)),
        )?;
        let mut command = tokio::process::Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                "tests::full::astra_failure_reports_without_blocking_network_configuration_and_recovers",
                "--nocapture",
            ])
            .env("ASTRA_RECOVERY_TEST_CHILD", "true")
            .env("DISABLE_TLS_ENFORCEMENT", "true")
            .env("IGNORE_MGMT_VRF", "true")
            .env("NO_DPU_CONTAINERS", "true")
            .env("PATH", path)
            .kill_on_drop(true);
        // Both report phases allow 60 seconds, with time for startup and cleanup.
        let output = tokio::time::timeout(Duration::from_secs(210), command.output()).await??;
        eyre::ensure!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
            "astra recovery test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return Ok(());
    }

    // Keep the desired attachment and revision fixed through both phases.
    // Recovery must come from retrying Weave, without a new configuration.
    let expected_attachment = rpc::forge::AstraAttachment {
        mac_address: "02:aa:bb:cc:dd:ee".to_string(),
        vni: 100,
        subnet_ipv4: "192.0.2.0".to_string(),
        subnet_mask: 24,
        attachment_type: Some(rpc::forge::SpxAttachmentType::Physical.into()),
        revision: "test-revision".to_string(),
        ..Default::default()
    };
    let state = Arc::new(Mutex::new(State {
        virtualization_type: VpcVirtualizationType::EthernetVirtualizer,
        loopback_ip_override: Some("2001:db8::1"),
        astra_config: Some(rpc::forge::AstraConfig {
            astra_attachments: vec![expected_attachment.clone()],
        }),
        ..Default::default()
    }));
    let (addr, server) = common::run_grpc_server(mock_core(state.clone())).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let dhcp_addr = listener.local_addr()?;
    let dhcp_app = Router::new()
        .route(
            "/dhcp_server_control.DhcpServerControl/UpdateAndReloadConfig",
            post(handle_dhcp_update),
        )
        .route(
            "/dhcp_server_control.DhcpServerControl/GetDhcpTimestamps",
            post(|| async { common::respond(()) }),
        )
        .with_state(state.clone());
    let dhcp_server = tokio::spawn(async move { axum::serve(listener, dhcp_app).await });
    let (socket, weave_state, weave_server) =
        crate::astra_weave::tests::start_recording_weave_ew_vpc_mock_server_for_agent();
    // Fail the Weave read while Core and DHCP remain available, isolating
    // Astra failure from the other network configuration work.
    weave_state.lock().await.reject_network_lists = true;

    // The nested blocks save errors until their agent and server tasks have
    // been stopped and joined; an early `?` must not skip fixture cleanup.
    let result = async {
        let directory = tempfile::tempdir()?;
        let config_file = tempfile::NamedTempFile::new()?;
        let mut options =
            common::setup_agent_run_options(&addr, &directory, &config_file, false, &repo_root)?;
        let Some(crate::AgentCommand::Run(run)) = options.cmd.as_mut() else {
            eyre::bail!("test fixture did not configure an agent run");
        };
        run.dhcp_grpc_server = Some(format!("http://{dhcp_addr}"));
        let config = fs::read_to_string(config_file.path())?
            .replace("main-loop-idle-secs = 30", "main-loop-idle-secs = 1");
        fs::write(config_file.path(), config)?;
        fs::create_dir_all(directory.path().join("var/support/forge-dhcp/conf"))?;
        let startup_path = directory.path().join(crate::nvue::PATH);
        // A changed startup file proves HBN applied the configuration during
        // the Weave failure; reported versions alone could miss a skipped write.
        const PREVIOUS_NETWORK: &str = "previous network configuration\n";
        fs::write(&startup_path, PREVIOUS_NETWORK)?;

        let socket = socket
            .to_str()
            .ok_or_else(|| eyre::eyre!("test weave socket path is not UTF-8"))?
            .to_string();
        let agent = tokio::spawn(async move {
            crate::astra_weave::TEST_WEAVE_EW_VPC_SOCKET_PATH
                .scope(socket, crate::start(options))
                .await
        });
        let result = async {
            // First wait for the failed Astra attempt to reach the Core mock.
            // Its report must still acknowledge the successful HBN/DHCP work.
            let failed = wait_for_network_status(&state, |status| {
                status.network_config_error.as_deref().is_some_and(|error| {
                    error.contains("injected virtual network list failure")
                })
            })
            .await?;
            eyre::ensure!(
                failed.astra_config_status == Some(rpc::forge::AstraConfigStatus::default()),
                "failed astra attempt omitted its explicit unsuccessful report: {failed:?}"
            );
            eyre::ensure!(
                failed.network_config_version.as_deref() == Some(DHCP_GATE_CONFIG_VERSION)
                    && failed.instance_network_config_version.as_deref()
                        == Some(DHCP_GATE_CONFIG_VERSION),
                "astra failure withheld successful HBN/DHCP acknowledgements: {failed:?}"
            );
            eyre::ensure!(
                state.lock().await.dhcp_updates.iter().any(|(rejected, _)| !rejected),
                "DHCP did not accept the network configuration"
            );
            let startup = fs::read_to_string(&startup_path)?;
            eyre::ensure!(startup != PREVIOUS_NETWORK, "HBN configuration was not applied");
            serde_yaml::from_str::<Vec<serde_yaml::Value>>(&startup)?;

            // Only the service failure changes. The next ordinary iteration
            // must retry Astra while retaining the same network acknowledgements.
            weave_state.lock().await.reject_network_lists = false;
            let recovered = wait_for_network_status(&state, |status| {
                status.astra_config_status.as_ref().is_some_and(|astra| {
                    astra.astra_attachments_status.iter().any(|attachment| {
                        attachment.status.as_ref().is_some_and(|status| {
                            status.phase == rpc::forge::AstraPhase::PhaseReady as i32
                        })
                    })
                })
            })
            .await?;
            eyre::ensure!(
                recovered.network_config_version == failed.network_config_version
                    && recovered.instance_network_config_version
                        == failed.instance_network_config_version
                    && recovered.network_config_error.is_none(),
                "astra recovery changed network acknowledgements or retained an error: {recovered:?}"
            );
            let astra = recovered.astra_config_status.as_ref().unwrap();
            let [attachment] = astra.astra_attachments_status.as_slice() else {
                eyre::bail!("unexpected recovered astra attachments: {astra:?}");
            };
            eyre::ensure!(
                attachment.mac_address == expected_attachment.mac_address
                    && attachment.vni == expected_attachment.vni as i32
                    && attachment.revision == expected_attachment.revision,
                "astra recovery did not acknowledge the unchanged attachment: {attachment:?}"
            );
            eyre::ensure!(!agent.is_finished(), "agent exited during astra recovery");
            Ok::<_, eyre::Report>(())
        }
        .await;
        agent.abort();
        match agent.await {
            Ok(Err(error)) => return Err(error).wrap_err("agent exited during astra recovery"),
            Err(error) if !error.is_cancelled() => return Err(error).wrap_err("agent task failed"),
            _ => {}
        }
        result
    }
    .await;
    dhcp_server.abort();
    server.abort();
    let (dhcp_result, weave_result, core_result) =
        tokio::join!(dhcp_server, weave_server.shutdown(), server);
    match dhcp_result {
        Ok(result) => result.wrap_err("mock DHCP control server exited")?,
        Err(error) if !error.is_cancelled() => return Err(error).wrap_err("mock DHCP task failed"),
        _ => {}
    }
    weave_result?;
    if let Err(error) = core_result
        && !error.is_cancelled()
    {
        return Err(error).wrap_err("mock core task failed");
    }
    result
}

/// Exercise both network failure boundaries through the real agent and Core
/// status RPCs, with child-local environment overrides instead of global writes.
async fn network_update_retries_the_same_configuration(fail_fmds_route: bool) -> eyre::Result<()> {
    let repo_root = std::env::var("REPO_ROOT")
        .or_else(|_| std::env::var("CONTAINER_REPO_ROOT"))
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    eyre::ensure!(
        repo_root.join("dev/bin/ip").is_file(),
        "network recovery tests require repository fixtures; set REPO_ROOT to the checkout"
    );
    if std::env::var_os("NETWORK_RECOVERY_TEST_CHILD").is_none() {
        // Set the child's environment before its runtime starts. Calling the
        // shared environment fixture here would race unrelated test threads.
        let inherited_path =
            std::env::var_os("PATH").ok_or_else(|| eyre::eyre!("missing test PATH"))?;
        let fake_network = if fail_fmds_route {
            Some(fmds_route_failure_commands()?)
        } else {
            None
        };
        let path = std::env::join_paths(
            fake_network
                .iter()
                .map(|directory| directory.path().to_path_buf())
                .chain(std::iter::once(repo_root.join("dev/bin")))
                .chain(std::env::split_paths(&inherited_path)),
        )?;
        let mut command = tokio::process::Command::new(std::env::current_exe()?);
        command
            .args([
                "--exact",
                if fail_fmds_route {
                    "tests::full::fmds_failure_applies_quarantine_and_retries_the_same_network_configuration"
                } else {
                    "tests::full::dhcp_unavailability_retries_the_same_ipv6_network_configuration"
                },
                "--nocapture",
            ])
            .env("NETWORK_RECOVERY_TEST_CHILD", "true")
            .env("DISABLE_TLS_ENFORCEMENT", "true")
            .env("IGNORE_MGMT_VRF", "true")
            .env("NO_DPU_CONTAINERS", "true")
            .env("PATH", path)
            .kill_on_drop(true);
        if let Some(directory) = &fake_network {
            command
                .env("FMDS_TEST_COMMAND_DIR", directory.path())
                .env("FMDS_TEST_FALLBACK_IP", repo_root.join("dev/bin/ip"))
                .env_remove("NO_DPU_ARMOS_NETWORK")
                .env_remove("NO_DPU_ARMOS_INTERFACE");
        }
        // Each failure/recovery phase allows 60 seconds; also allow startup
        // and cleanup without leaving a stuck child behind.
        let output = tokio::time::timeout(Duration::from_secs(210), command.output()).await??;
        eyre::ensure!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("1 passed; 0 failed"),
            "network recovery test failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        return Ok(());
    }

    let state = Arc::new(Mutex::new(State {
        virtualization_type: VpcVirtualizationType::EthernetVirtualizer,
        loopback_ip_override: Some("2001:db8::1"),
        reject_next_network_status: true,
        reject_dhcp_update: !fail_fmds_route,
        include_ipv6_tenant: fail_fmds_route,
        quarantine_host: fail_fmds_route,
        ..Default::default()
    }));
    let (addr, server) = common::run_grpc_server(mock_core(state.clone())).await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let dhcp_addr = listener.local_addr()?;
    let dhcp_app = Router::new()
        .route(
            "/dhcp_server_control.DhcpServerControl/UpdateAndReloadConfig",
            post(handle_dhcp_update),
        )
        .route(
            "/dhcp_server_control.DhcpServerControl/GetDhcpTimestamps",
            post(|| async { common::respond(()) }),
        )
        .with_state(state.clone());
    let dhcp_server = tokio::spawn(async move { axum::serve(listener, dhcp_app).await });
    let result = async {
        let directory = tempfile::tempdir()?;
        let config_file = tempfile::NamedTempFile::new()?;
        let mut options =
            common::setup_agent_run_options(&addr, &directory, &config_file, false, &repo_root)?;
        let Some(crate::AgentCommand::Run(run)) = options.cmd.as_mut() else {
            eyre::bail!("test fixture did not configure an agent run");
        };
        run.dhcp_grpc_server = Some(format!("http://{dhcp_addr}"));
        if fail_fmds_route {
            // Failed FMDS updates can use the idle period when HBN remains
            // healthy. Keep both periods short without changing desired versions.
            let config = fs::read_to_string(config_file.path())?
                .replace("main-loop-idle-secs = 30", "main-loop-idle-secs = 1");
            fs::write(config_file.path(), config)?;
            let mut config_file = fs::OpenOptions::new().append(true).open(config_file.path())?;
            writeln!(config_file, "\n[fmds-armos-networking.config]")?;
            writeln!(config_file, "addresses = [\"169.254.169.254/30\", \"fd00:76b9::254/127\"]")?;
        }
        fs::create_dir_all(directory.path().join("var/support/forge-dhcp/conf"))?;
        let startup_path = directory.path().join(crate::nvue::PATH);
        const PREVIOUS_NETWORK: &str = "previous network configuration\n";
        fs::write(&startup_path, PREVIOUS_NETWORK)?;

        let agent = tokio::spawn(async move {
            if fail_fmds_route {
                // Use production command execution, but only the child-local
                // fake ip binary. No command can modify the host's networking.
                crate::dpu::REAL_KERNEL_NETWORKING
                    .scope((), crate::start(options))
                    .await
            } else {
                crate::start(options).await
            }
        });
        let result = async {
            // DHCP rejection still gates NVUE. FMDS readiness failure must leave
            // the version pending without withholding the quarantine update.
            let expected_error = if fail_fmds_route {
                "duplicate address detection failed for fd00:76b9::254"
            } else {
                "injected DHCP rejection"
            };
            let rejected = wait_for_network_status(&state, |status| {
                status.network_config_error.as_deref().is_some_and(|error| {
                    error.contains(expected_error)
                })
            })
            .await?;
            eyre::ensure!(
                rejected
                    .network_config_error
                    .as_deref()
                    .is_some_and(|error| { error.contains(expected_error) }),
                "unexpected network rejection: {rejected:?}"
            );
            eyre::ensure!(
                rejected.network_config_version.is_none()
                    && rejected.instance_network_config_version.is_none(),
                "rejected configuration was acknowledged: {rejected:?}"
            );
            if fail_fmds_route {
                let startup: Vec<serde_yaml::Value> =
                    serde_yaml::from_str(&fs::read_to_string(&startup_path)?)?;
                let config = &startup[1]["set"];
                assert!(config.is_mapping(), "missing NVUE set document: {startup:#?}");
                // Check the catch-all deny and its attachment, not merely that
                // NVUE saved some different configuration while FMDS was failing.
                let quarantine_acl = "p0004_security_policy_override_ipv6_host_ingress";
                let acl = &config["acl"][quarantine_acl];
                let rule = &acl["rule"]["2000"];
                assert!(rule["action"]["deny"].is_mapping(), "missing quarantine deny: {acl:#?}");
                assert_eq!(rule["match"]["ip"]["source-ip"].as_str(), Some("::/0"), "quarantine ACL: {acl:#?}");
                assert_eq!(rule["match"]["ip"]["dest-ip"].as_str(), Some("::/0"), "quarantine ACL: {acl:#?}");
                assert!(config["interface"]["pf0hpf_if"]["acl"][quarantine_acl]["outbound"].is_mapping(),
                    "quarantine ACL is not attached: {:#?}", config["interface"]);
                let directory = std::path::PathBuf::from(std::env::var("FMDS_TEST_COMMAND_DIR")?);
                assert_eq!(fs::read_to_string(directory.join("ovs-restarts"))?, "restart ovs-vswitchd.service\n");
                assert!(state.lock().await.post_config_wait_seen,
                    "an applied NVUE update must retain the post-config health wait after FMDS failure");
                assert!(!directory.join("attempts").exists(), "DAD failure must precede route application");
            } else {
                eyre::ensure!(
                    fs::read_to_string(&startup_path)? == PREVIOUS_NETWORK,
                    "DHCP rejection overwrote the existing network configuration"
                );
            }
            {
                let state = state.lock().await;
                let [(rejected_at, true), (retried_at, false), ..] =
                    state.network_status_reports.as_slice()
                else {
                    eyre::bail!(
                        "expected a successful status report after one rejected RPC: {:?}",
                        state.network_status_reports
                    );
                };
                // The shared fixture configures a one-second active period.
                // A failed status RPC must still wait before the next report.
                assert!(retried_at.duration_since(*rejected_at) >= Duration::from_secs(1));
            }

            let command_directory = if fail_fmds_route {
                let directory = std::path::PathBuf::from(std::env::var("FMDS_TEST_COMMAND_DIR")?);
                // Model the operator repairing the duplicate outside the agent.
                // The agent must not delete the address or bypass DAD itself.
                fs::remove_file(directory.join("reject-dad"))?;
                let route_rejected = wait_for_network_status(&state, |status| {
                    status.network_config_error.as_deref().is_some_and(|error| {
                        error.contains("injected FMDS route failure")
                    })
                }).await?;
                assert!(route_rejected.network_config_version.is_none()
                    && route_rejected.instance_network_config_version.is_none());
                // Wait for a second failed route attempt and its completed status
                // RPC, so clearing the failure cannot race the fake command itself.
                let failed_report_count = state.lock().await.network_status_reports.len();
                tokio::time::timeout(Duration::from_secs(10), async {
                    loop {
                        let attempts = fs::read_to_string(directory.join("attempts"))?;
                        if attempts.lines().count() >= 2
                            && state.lock().await.network_status_reports.len() > failed_report_count
                        {
                            return Ok::<_, eyre::Report>(());
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }).await??;
                let attempts = fs::read_to_string(directory.join("attempts"))?;
                eyre::ensure!(
                    attempts.lines().count() >= 2,
                    "FMDS route was not retried: {attempts}"
                );
                const EXPECTED_ROUTE: &str = "-6 route add 2001:db8:7176::/64 dev pf0dpu1 via fd00:76b9::255 src fd00:76b9::254";
                eyre::ensure!(
                    attempts.lines().all(|request| request == EXPECTED_ROUTE),
                    "unexpected FMDS route command: {attempts}"
                );
                eyre::ensure!(!agent.is_finished(), "agent exited on a transient FMDS error");
                Some(directory)
            } else {
                None
            };

            // Clear only the injected failure: no configuration or version changes.
            if let Some(directory) = &command_directory {
                fs::remove_file(directory.join("reject"))?;
            } else {
                state.lock().await.reject_dhcp_update = false;
            }
            let applied = wait_for_network_status(&state, |status| {
                status.network_config_version.as_deref() == Some(DHCP_GATE_CONFIG_VERSION)
                    && status.instance_network_config_version.as_deref()
                        == Some(DHCP_GATE_CONFIG_VERSION)
            })
            .await?;
            eyre::ensure!(
                applied.network_config_error.is_none(),
                "acknowledged configuration still reports an error: {applied:?}"
            );
            let startup = fs::read_to_string(&startup_path)?;
            eyre::ensure!(
                startup != PREVIOUS_NETWORK,
                "supported update did not replace the startup file"
            );
            serde_yaml::from_str::<Vec<serde_yaml::Value>>(&startup)?;
            if let Some(directory) = command_directory {
                let attempts = fs::read_to_string(directory.join("attempts"))?;
                let requests: Vec<_> = attempts.lines().collect();
                eyre::ensure!(
                    requests.len() >= 3 && requests.iter().all(|request| *request == requests[0]),
                    "FMDS recovery changed the retried route: {attempts}"
                );
                // Once acknowledged, the same version must stop issuing route
                // commands. Another status RPC proves a later tick ran.
                let report_count = state.lock().await.network_status_reports.len();
                tokio::time::timeout(Duration::from_secs(10), async {
                    while state.lock().await.network_status_reports.len() <= report_count {
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                })
                .await?;
                assert_eq!(fs::read_to_string(directory.join("attempts"))?, attempts);
                assert_eq!(fs::read_to_string(directory.join("ovs-restarts"))?, "restart ovs-vswitchd.service\n",
                    "retrying FMDS must not repeat an already-completed OVS transition");
            } else {
                let state = state.lock().await;
                let rejected_request = state
                    .dhcp_updates
                    .iter()
                    .find(|(rejected, _)| *rejected)
                    .expect("DHCP received a rejected update");
                let accepted_request = state
                    .dhcp_updates
                    .iter()
                    .find(|(rejected, _)| !rejected)
                    .expect("DHCP received an accepted retry");
                assert_eq!(rejected_request.1, accepted_request.1);
            }
            Ok::<_, eyre::Report>(())
        }
        .await;
        agent.abort();
        match agent.await {
            Ok(Err(error)) => {
                return Err(error).wrap_err("agent exited before reconciliation completed");
            }
            Err(error) if !error.is_cancelled() => return Err(error).wrap_err("agent task failed"),
            _ => {}
        }
        result
    }
    .await;
    dhcp_server.abort();
    server.abort();
    match dhcp_server.await {
        Ok(result) => result.wrap_err("mock DHCP control server exited")?,
        Err(error) if !error.is_cancelled() => return Err(error).wrap_err("mock DHCP task failed"),
        _ => {}
    }
    if let Err(error) = server.await
        && !error.is_cancelled()
    {
        return Err(error).wrap_err("mock core task failed");
    }
    result
}

/// Keep networking and service commands child-local while exposing DAD failure,
/// route failure, and the OVS restart independently to the reconciliation test.
fn fmds_route_failure_commands() -> eyre::Result<tempfile::TempDir> {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir()?;
    let script = directory.path().join("ip");
    fs::write(
        &script,
        r#"#!/bin/sh
set -eu
case "$*" in
    '-j link show'|'-j addr show dev pf0dpu1')
        if [ -f "$FMDS_TEST_COMMAND_DIR/reject-dad" ]; then
            cat "$FMDS_TEST_COMMAND_DIR/interface-dadfailed.json"
        else
            cat "$FMDS_TEST_COMMAND_DIR/interface.json"
        fi
        ;;
    '-j -4 route show dev pf0dpu1'|'-j -6 route show dev pf0dpu1')
        route_file="$FMDS_TEST_COMMAND_DIR/route$2"
        if [ -f "$route_file" ]; then cat "$route_file"; else printf '[]\n'; fi
        ;;
    '-4 route add '*|'-6 route add '*)
        [ "$5" = dev ] && [ "$6" = pf0dpu1 ] && [ "$7" = via ] && [ "$9" = src ] || exit 2
        if [ "$1" = -6 ]; then
            printf '%s\n' "$*" >> "$FMDS_TEST_COMMAND_DIR/attempts"
            if [ -f "$FMDS_TEST_COMMAND_DIR/reject" ]; then
                printf 'injected FMDS route failure\n' >&2
                exit 1
            fi
        fi
        printf '[{"dst":"%s","dev":"%s","gateway":"%s","prefsrc":"%s","protocol":"boot","flags":[]}]\n' "$4" "$6" "$8" "${10}" > "$FMDS_TEST_COMMAND_DIR/route$1"
        ;;
    *) exec "$FMDS_TEST_FALLBACK_IP" "$@" ;;
esac
"#,
    )?;
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755))?;
    // Never invoke the host's systemctl. The transition restart is observable
    // even though this fixture intentionally stops at NVUE startup-file output.
    let systemctl = directory.path().join("systemctl");
    fs::write(
        &systemctl,
        r#"#!/bin/sh
set -eu
[ "$*" = 'restart ovs-vswitchd.service' ] || exit 2
printf '%s\n' "$*" >> "$FMDS_TEST_COMMAND_DIR/ovs-restarts"
"#,
    )?;
    fs::set_permissions(&systemctl, fs::Permissions::from_mode(0o755))?;
    fs::write(
        directory.path().join("reject-dad"),
        "duplicate IPv6 address until operator repair\n",
    )?;
    fs::write(
        directory.path().join("reject"),
        "reject IPv6 route until recovery\n",
    )?;
    let mut interface = serde_json::json!([{
        "ifindex": 7, "ifname": "pf0dpu1", "flags": ["UP"], "mtu": 1500,
        "qdisc": "noqueue", "operstate": "UP", "group": "default",
        "address": "02:00:00:00:00:07", "broadcast": "ff:ff:ff:ff:ff:ff",
        "addr_info": [
            {"family": "inet", "local": "169.254.169.254", "prefixlen": 30,
             "scope": "global", "valid_life_time": 4294967295_u32,
             "preferred_life_time": 4294967295_u32},
            {"family": "inet6", "local": "fd00:76b9::254", "prefixlen": 127,
             "scope": "global", "valid_life_time": 4294967295_u32,
             "preferred_life_time": 4294967295_u32}
        ]
    }]);
    fs::write(
        directory.path().join("interface.json"),
        serde_json::to_vec(&interface)?,
    )?;
    // The address exists in both snapshots; only the kernel's DAD result changes
    // after external repair, so readiness cannot be confused with assignment.
    interface[0]["addr_info"][1]["dadfailed"] = true.into();
    fs::write(
        directory.path().join("interface-dadfailed.json"),
        serde_json::to_vec(&interface)?,
    )?;
    Ok(directory)
}

async fn handle_dhcp_update(
    AxumState(state): AxumState<Arc<Mutex<State>>>,
    body: Bytes,
) -> axum::response::Response {
    let mut state = state.lock().await;
    let rejected = state.reject_dhcp_update;
    // Compare the complete request bytes after recovery; only the mock's
    // availability changes, not the DHCP configuration the agent submits.
    state.dhcp_updates.push((rejected, body));
    if rejected {
        return (
            [
                ("content-type", "application/grpc"),
                ("grpc-status", "14"),
                ("grpc-message", "injected%20DHCP%20rejection"),
            ],
            "",
        )
            .into_response();
    }
    common::respond(()).into_response()
}

/// Waits for a matching report accepted by the Core mock. A timeout includes
/// the last report so a missing transition can be diagnosed from the failure.
async fn wait_for_network_status(
    state: &Arc<Mutex<State>>,
    matches: impl Fn(&rpc::forge::DpuNetworkStatus) -> bool,
) -> eyre::Result<rpc::forge::DpuNetworkStatus> {
    let result = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let status = state.lock().await.last_network_status.clone();
            if let Some(status) = status
                && matches(&status)
            {
                return status;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    match result {
        Ok(status) => Ok(status),
        Err(error) => {
            let state = state.lock().await;
            Err(error).wrap_err(format!(
                "timed out waiting for expected DPU network status; last report: {:?}",
                state.last_network_status
            ))
        }
    }
}

// All of the new tests are leveraging nvue for configs, regardless
// of template, so have a test_nvue_generic that just takes a virtualization
// type.
async fn test_nvue_generic(
    virtualization_type: VpcVirtualizationType,
    expected: &str,
) -> eyre::Result<()> {
    let out = run_common_parts(virtualization_type, false).await?;
    if out.is_skip {
        return Ok(());
    }

    // Make sure the nvue startup file was written where
    // it was supposed to be written (crate::nvue::PATH
    // within the test-specific temp dir).
    let td = out.hbn_root_dir.unwrap();
    let hbn_root = td.path();
    let startup_yaml = hbn_root.join(crate::nvue::PATH);
    assert!(
        startup_yaml.exists(),
        "could not find {} startup_yaml at path: {:?}",
        virtualization_type,
        startup_yaml.to_str()
    );

    // And now check that the output nvue config YAML
    // is actually valid YAML. If it's not, write out
    // whatever the error is to ERR_FILE, so we can go
    // check and see what's up.
    const ERR_FILE: &str = "/tmp/test_nvue_startup.yaml";
    let startup_yaml = fs::read_to_string(startup_yaml)?;
    let yaml_obj: Vec<serde_yaml::Value> = serde_yaml::from_str(&startup_yaml)
        .inspect_err(|_| {
            let mut f = fs::File::create(ERR_FILE).unwrap();
            f.write_all(startup_yaml.as_bytes()).unwrap();
        })
        .wrap_err(format!("YAML parser error. output written to {ERR_FILE}"))?;
    assert_eq!(yaml_obj.len(), 2); // 'header' and 'set'

    let r = compare_lines(startup_yaml.as_str(), expected, None);
    eprint!("Diff output:\n{}", r.report());
    assert!(
        r.is_identical(),
        "generated startup_yaml does not match expected startup_yaml for {virtualization_type}"
    );

    Ok(())
}

// Query the FMDS endpoint to retrieve tenant metadata
// and make sure it matches expected values. run_common_parts
// launches the forge_dpu_agent, and by passing in true to run_common_parts,
// we are asking it to launch the metadata service. run_common_parts also launches
// a gRPC server that returns data in response to GetManagedHostNetworkConfig call,
// and that data populates the data retrieved by the metadata endpoint server.
#[tokio::test(flavor = "multi_thread")]
// Test retrieving instance metadata using FMDS
async fn test_fmds_get_data() -> eyre::Result<()> {
    let out = run_common_parts(VpcVirtualizationType::EthernetVirtualizer, true).await?;
    if out.is_skip {
        return Ok(());
    }

    // Test get hostname
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri("http://0.0.0.0:7777/latest/meta-data/hostname".to_string())
        .body("".into())
        .unwrap();

    let response = client.request(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();

    assert_eq!(body_str, "9afaedd3-b36e-4603-a029-8b94a82b89a0");

    // Test get instance name
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri("http://0.0.0.0:7777/latest/meta-data/instance-name".to_string())
        .body("".into())
        .unwrap();

    let response = client.request(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();

    assert_eq!(body_str, "test-instance");

    // Core reports IPv6 first here, but FMDS still exposes each family through its own category.
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri("http://0.0.0.0:7777/latest/meta-data/public-ipv4".to_string())
        .body("".into())
        .unwrap();

    let response = client.request(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();

    assert_eq!(body_str, "10.217.104.146");

    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri("http://0.0.0.0:7777/latest/meta-data/public-ipv6".to_string())
        .body("".into())
        .unwrap();

    let response = client.request(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();

    assert_eq!(body_str, "2001:db8::146");

    // Test get machine_id
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri("http://0.0.0.0:7777/latest/meta-data/machine-id".to_string())
        .body("".into())
        .unwrap();

    let response = client.request(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();

    assert_eq!(
        body_str,
        "fm100htjsaledfasinabqqer70e2ua5ksqj4kfjii0v0a90vulps48c1h7g"
    );

    // Test get instance-id
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri("http://0.0.0.0:7777/latest/meta-data/instance-id".to_string())
        .body("".into())
        .unwrap();

    let response = client.request(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();

    assert_eq!(body_str, "9afaedd3-b36e-4603-a029-8b94a82b89a0");

    // Test get asn
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri("http://0.0.0.0:7777/latest/meta-data/asn".to_string())
        .body("".into())
        .unwrap();

    let response = client.request(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();

    assert_eq!(body_str, "65535");

    // Test get sitename
    let client = hyper_util::client::legacy::Client::builder(TokioExecutor::new()).build_http();
    let request: hyper::Request<Full<Bytes>> = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri("http://0.0.0.0:7777/latest/meta-data/sitename".to_string())
        .body("".into())
        .unwrap();

    let response = client.request(request).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = response.into_body().collect().await.unwrap().to_bytes();
    let body_str = std::str::from_utf8(&body).unwrap();

    assert_eq!(body_str, "testsite");

    Ok(())
}

// run_common_parts exists, because most of the test is
// shared between the [legacy] ETV files mechanism and the
// new nvue templating mechanism.
async fn run_common_parts(
    virtualization_type: VpcVirtualizationType,
    test_metadata_service: bool,
) -> eyre::Result<TestOut> {
    let state: Arc<Mutex<State>> = Arc::new(Mutex::new(Default::default()));
    state.lock().await.virtualization_type = virtualization_type;

    let (addr, join_handle) = common::run_grpc_server(mock_core(state.clone())).await?;

    let td: tempfile::TempDir = tempfile::tempdir()?;
    let agent_config_file = tempfile::NamedTempFile::new()?;
    let opts =
        match common::setup_agent_run_env(&addr, &td, &agent_config_file, test_metadata_service) {
            Ok(Some(opts)) => opts,
            Ok(None) => {
                return Ok(TestOut {
                    is_skip: true,
                    ..Default::default()
                });
            }
            Err(e) => {
                return Err(e);
            }
        };

    // Start forge-dpu-agent
    tokio::spawn(async move {
        if let Err(e) = crate::start(opts).await {
            tracing::error!(error = ?e, "Failed to start DPU agent");
        }
    });

    // Wait until we report health at least 2 times
    // At that point in time the first configuration should have been applied
    // and the check for updates should have occured
    let start = std::time::Instant::now();
    loop {
        let statel = state.lock().await;
        if statel.num_health_reports.load(Ordering::SeqCst) > 1
            && statel.num_netconf_fetches.load(Ordering::SeqCst) > 1
        {
            break;
        }

        if start.elapsed() > std::time::Duration::from_secs(60) {
            return Err(eyre::eyre!(
                "health report was not sent 2 times in 30s. state: {:?}",
                statel
            ));
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    join_handle.abort();

    // The gRPC calls were made
    let statel = state.lock().await;
    assert!(statel.has_discovered);
    assert!(statel.has_checked_for_upgrade);
    assert!(statel.num_health_reports.load(Ordering::SeqCst) > 1);
    // Since Network config fetching runs in a separate task, it might not have
    // happened 2 times but just a single time
    assert!(statel.num_netconf_fetches.load(Ordering::SeqCst) > 0);
    assert!(statel.num_get_dpu_ips.load(Ordering::SeqCst) > 0);
    Ok(TestOut {
        is_skip: false,
        hbn_root_dir: Some(td),
    })
}

fn mock_core(state: Arc<Mutex<State>>) -> Router {
    Router::new()
        .route("/up", get(handle_up))
        .route(
            ::rpc::service_path!("DiscoverMachine"),
            post(handle_discover),
        )
        .route(
            ::rpc::service_path!("GetManagedHostNetworkConfig"),
            post(handle_netconf),
        )
        .route(
            ::rpc::service_path!("RecordDpuNetworkStatus"),
            post(handle_record_netstat),
        )
        .route(
            ::rpc::service_path!("DpuAgentUpgradeCheck"),
            post(handle_dpu_agent_upgrade_check),
        )
        .route(
            ::rpc::service_path!("UpdateAgentReportedInventory"),
            post(handle_update_agent_reported_inventory),
        )
        .route(
            ::rpc::service_path!("GetDpuInfoList"),
            post(handle_get_dpu_info_list),
        )
        .route(
            ::rpc::service_path!("FindInterfaces"),
            post(handle_find_interfaces),
        )
        // ForgeApiClient needs a working Version route for connection retrying
        .route(::rpc::service_path!("Version"), post(handle_version))
        .fallback(handler)
        .with_state(state)
}

/// Health check. When this responds we know the mock server is ready.
async fn handle_up() -> &'static str {
    "OK"
}

async fn handle_discover(AxumState(state): AxumState<Arc<Mutex<State>>>) -> impl IntoResponse {
    state.lock().await.has_discovered = true;
    common::respond(rpc::forge::MachineDiscoveryResult {
        machine_id: Some(
            "fm100dsasb5dsh6e6ogogslpovne4rj82rp9jlf00qd7mcvmaadv85phk3g"
                .parse()
                .unwrap(),
        ),
        machine_certificate: None,
        attest_key_challenge: None,
        machine_interface_id: None,
    })
}

async fn handle_version() -> impl IntoResponse {
    let mut resp = rpc::forge::BuildInfo::default();

    let rc = rpc::forge::RuntimeConfig {
        sitename: Some("testsite".to_string()),
        ..Default::default()
    };

    resp.runtime_config = Some(rc);

    common::respond(resp)
}

// This mock omits `addresses` to exercise the rolling-upgrade fallback for older Core versions.
#[allow(deprecated)]
async fn handle_netconf(AxumState(state): AxumState<Arc<Mutex<State>>>) -> impl IntoResponse {
    {
        state
            .lock()
            .await
            .num_netconf_fetches
            .fetch_add(1, Ordering::SeqCst);
    }
    let (
        virtualization_type,
        loopback_ip_override,
        include_ipv6_tenant,
        quarantine_host,
        astra_config,
    ) = {
        let state = state.lock().await;
        (
            state.virtualization_type,
            state.loopback_ip_override,
            state.include_ipv6_tenant,
            state.quarantine_host,
            state.astra_config.clone(),
        )
    };
    let config_version = if loopback_ip_override.is_some() {
        DHCP_GATE_CONFIG_VERSION.to_string()
    } else {
        format!("V{}-T{}", 1, now().timestamp_micros())
    };

    let vpc_peer_prefixes = match virtualization_type {
        VpcVirtualizationType::EthernetVirtualizer => {
            vec!["10.217.6.176/29".to_string()]
        }
        VpcVirtualizationType::Fnn => {
            vec![]
        }
        _ => vec![],
    };

    let vpc_peer_vnis = match virtualization_type {
        VpcVirtualizationType::EthernetVirtualizer => {
            vec![]
        }
        VpcVirtualizationType::Fnn => {
            println!("Setting vpc_peer_vnis to fnn");
            vec![1025186, 1025197]
        }
        _ => vec![],
    };

    let admin_interface_prefix: IpNetwork = "192.168.0.12/32".parse().unwrap();
    let svi_ip = IpAddr::from_str("192.168.0.3").unwrap();

    let admin_interface = rpc::forge::FlatInterfaceConfig {
        function_type: rpc::forge::InterfaceFunctionType::Physical.into(),
        vlan_id: 10,
        vni: 10100,
        vpc_vni: 10101,
        gateway: Some("192.168.0.0/16".to_string()),
        ip: Some("192.168.0.12".to_string()),
        interface_prefix: Some(admin_interface_prefix.to_string()),
        virtual_function_id: None,
        vpc_prefixes: vec![],
        vpc_peer_prefixes: vec![],
        vpc_peer_vnis: vec![1025186, 1025197],
        prefix: Some("192.168.0.1/32".to_string()),
        fqdn: "host1".to_string(),
        booturl: loopback_ip_override.map(|_| "http://[2001:db8::80]/boot.efi".to_string()),
        svi_ip: get_svi_ip(&Some(svi_ip), virtualization_type, false, 28)
            .unwrap()
            .map(|ip| ip.to_string()),
        tenant_vrf_loopback_ip: Some("10.1.1.1".to_string()),
        is_l2_segment: false,
        network_security_group: None,
        internal_uuid: None,
        mtu: None,
        ipv6_interface_config: None,
        vpc_routing_profile: None,
        interface_routing_profile: None,
        addresses: vec![],
    };
    assert_eq!(admin_interface.svi_ip, None);

    let tenant_interface_prefix: IpNetwork = "192.168.1.12/32".parse().unwrap();

    let mut tenant_interface = rpc::forge::FlatInterfaceConfig {
        function_type: rpc::forge::InterfaceFunctionType::Physical.into(),
        vlan_id: 10,
        vni: 10100,
        vpc_vni: 10101,
        gateway: Some("192.168.1.0/16".to_string()),
        ip: Some("192.168.1.12".to_string()),
        interface_prefix: Some(tenant_interface_prefix.to_string()),
        virtual_function_id: None,
        vpc_prefixes: vec![],
        vpc_peer_prefixes,
        vpc_peer_vnis,
        prefix: Some("192.168.1.1/32".to_string()),
        fqdn: "host1".to_string(),
        // The quarantined tenant needs the same explicit IPv6 boot URL as the
        // admin fixture; its IPv4-only service discovery cannot generate one.
        booturl: quarantine_host.then(|| "http://[2001:db8::80]/boot.efi".to_string()),
        svi_ip: get_svi_ip(&Some(svi_ip), virtualization_type, false, 28)
            .unwrap()
            .map(|ip| ip.to_string()),
        tenant_vrf_loopback_ip: Some("10.1.1.1".to_string()),
        is_l2_segment: false,
        network_security_group: Some(FlatInterfaceNetworkSecurityGroupConfig {
            id: "5b931164-d9c6-11ef-8292-232e57575621".to_string(),
            version: "V1-1".to_string(),
            source: rpc::forge::NetworkSecurityGroupSource::NsgSourceVpc.into(),
            stateful_egress: true,
            rules: vec![rpc::forge::ResolvedNetworkSecurityGroupRule {
                src_prefixes: vec!["0.0.0.0/0".to_string()],
                dst_prefixes: vec!["0.0.0.0/0".to_string()],
                rule: Some(rpc::forge::NetworkSecurityGroupRuleAttributes {
                    id: Some("anything".to_string()),
                    direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                        .into(),
                    ipv6: false,
                    src_port_start: Some(80),
                    src_port_end: Some(81),
                    dst_port_start: Some(80),
                    dst_port_end: Some(81),
                    protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                    action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                    priority: 9001,
                    source_net: Some(
                        rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                            "0.0.0.0/0".to_string(),
                        ),
                    ),
                    destination_net: Some(
                        rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                            "0.0.0.0/0".to_string(),
                        ),
                    ),
                }),
            },
            rpc::forge::ResolvedNetworkSecurityGroupRule {
                src_prefixes: vec!["0.0.0.0/0".to_string()],
                dst_prefixes: vec!["1.0.0.0/0".to_string()],
                rule: Some(rpc::forge::NetworkSecurityGroupRuleAttributes {
                    id: Some("anything".to_string()),
                    direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress
                        .into(),
                    ipv6: false,
                    src_port_start: Some(80),
                    src_port_end: Some(81),
                    dst_port_start: Some(80),
                    dst_port_end: Some(81),
                    protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                    action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                    priority: 9001,
                    source_net: Some(
                        rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                            "1.0.0.0/0".to_string(),
                        ),
                    ),
                    destination_net: Some(
                        rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                            "1.0.0.0/0".to_string(),
                        ),
                    ),
                }),
            },
            rpc::forge::ResolvedNetworkSecurityGroupRule {
                src_prefixes: vec!["0.0.0.0/0".to_string()],
                dst_prefixes: vec!["1.0.0.0/0".to_string()],
                rule: Some(rpc::forge::NetworkSecurityGroupRuleAttributes {
                    id: Some("anything".to_string()),
                    direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress
                        .into(),
                    ipv6: false,
                    src_port_start: None,
                    src_port_end: None,
                    dst_port_start: Some(8080),
                    dst_port_end: Some(8080),
                    protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                    action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                    priority: 9001,
                    source_net: Some(
                        rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                            "1.0.0.0/0".to_string(),
                        ),
                    ),
                    destination_net: Some(
                        rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                            "1.0.0.0/0".to_string(),
                        ),
                    ),
                }),
            },
            rpc::forge::ResolvedNetworkSecurityGroupRule {
                src_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:8888/128".to_string()],
                dst_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:9999/128".to_string()],
                rule: Some(rpc::forge::NetworkSecurityGroupRuleAttributes {
                    id: Some("anything".to_string()),
                    direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                        .into(),
                    ipv6: true,
                    src_port_start: Some(80),
                    src_port_end: Some(81),
                    dst_port_start: Some(80),
                    dst_port_end: Some(81),
                    protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                    action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                    priority: 9001,
                    source_net: Some(
                        rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                            "2001:db8:3333:4444:5555:6666:7777:8888/128".to_string(),
                        ),
                    ),
                    destination_net: Some(
                        rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                            "2001:db8:3333:4444:5555:6666:7777:9999/128".to_string(),
                        ),
                    ),
                }),
            },
            rpc::forge::ResolvedNetworkSecurityGroupRule {
                src_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:8888/128".to_string()],
                dst_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:9999/128".to_string()],
                rule: Some(rpc::forge::NetworkSecurityGroupRuleAttributes {
                    id: Some("anything".to_string()),
                    direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress
                        .into(),
                    ipv6: true,
                    src_port_start: Some(80),
                    src_port_end: Some(81),
                    dst_port_start: Some(80),
                    dst_port_end: Some(81),
                    protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                    action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                    priority: 9001,
                    source_net: Some(
                        rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                            "2001:db8:3333:4444:5555:6666:7777:8888/128".to_string(),
                        ),
                    ),
                    destination_net: Some(
                        rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                            "2001:db8:3333:4444:5555:6666:7777:9999/128".to_string(),
                        ),
                    ),
                }),
            }],
        }),
        internal_uuid: None,
        mtu: None,
        ipv6_interface_config: None,
        vpc_routing_profile: None,
        interface_routing_profile: None,
        addresses: vec![],
    };

    if include_ipv6_tenant {
        // Keep the existing IPv4 projection while giving FMDS a real canonical
        // IPv6 tenant prefix to route. Only the FMDS retry case needs this input.
        tenant_interface.addresses = vec![
            rpc::forge::InterfaceAddressConfig {
                address_family: rpc::forge::AddressFamily::V4.into(),
                ip: tenant_interface.ip.clone().expect("fixture IPv4 address"),
                interface_prefix: tenant_interface
                    .interface_prefix
                    .clone()
                    .expect("fixture IPv4 interface prefix"),
                prefix: tenant_interface
                    .prefix
                    .clone()
                    .expect("fixture IPv4 tenant prefix"),
                gateway: tenant_interface.gateway.clone(),
                svi_ip: tenant_interface.svi_ip.clone(),
                tenant_vrf_loopback_ip: tenant_interface.tenant_vrf_loopback_ip.clone(),
            },
            rpc::forge::InterfaceAddressConfig {
                address_family: rpc::forge::AddressFamily::V6.into(),
                ip: "2001:db8:7176::12".to_string(),
                interface_prefix: "2001:db8:7176::12/128".to_string(),
                prefix: "2001:db8:7176::/64".to_string(),
                svi_ip: Some("2001:db8:7176::1/64".to_string()),
                ..Default::default()
            },
        ];
    }

    let network_security_policy_overrides = vec![
        rpc::forge::ResolvedNetworkSecurityGroupRule {
            src_prefixes: vec!["0.0.0.0/0".to_string()],
            dst_prefixes: vec!["0.0.0.0/0".to_string()],
            rule: Some(rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                    .into(),
                ipv6: false,
                src_port_start: Some(80),
                src_port_end: Some(81),
                dst_port_start: Some(80),
                dst_port_end: Some(81),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                priority: 9001,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "0.0.0.0/0".to_string(),
                    ),
                ),
            }),
        },
        rpc::forge::ResolvedNetworkSecurityGroupRule {
            src_prefixes: vec!["0.0.0.0/0".to_string()],
            dst_prefixes: vec!["1.0.0.0/0".to_string()],
            rule: Some(rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress
                    .into(),
                ipv6: false,
                src_port_start: Some(80),
                src_port_end: Some(81),
                dst_port_start: Some(80),
                dst_port_end: Some(81),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                priority: 9001,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "1.0.0.0/0".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "1.0.0.0/0".to_string(),
                    ),
                ),
            }),
        },
        rpc::forge::ResolvedNetworkSecurityGroupRule {
            src_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:8888/128".to_string()],
            dst_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:9999/128".to_string()],
            rule: Some(rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionIngress
                    .into(),
                ipv6: true,
                src_port_start: Some(80),
                src_port_end: Some(81),
                dst_port_start: Some(80),
                dst_port_end: Some(81),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                priority: 9001,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "2001:db8:3333:4444:5555:6666:7777:8888/128".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "2001:db8:3333:4444:5555:6666:7777:9999/128".to_string(),
                    ),
                ),
            }),
        },
        rpc::forge::ResolvedNetworkSecurityGroupRule {
            src_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:8888/128".to_string()],
            dst_prefixes: vec!["2001:db8:3333:4444:5555:6666:7777:9999/128".to_string()],
            rule: Some(rpc::forge::NetworkSecurityGroupRuleAttributes {
                id: Some("anything".to_string()),
                direction: rpc::forge::NetworkSecurityGroupRuleDirection::NsgRuleDirectionEgress
                    .into(),
                ipv6: true,
                src_port_start: Some(80),
                src_port_end: Some(81),
                dst_port_start: Some(80),
                dst_port_end: Some(81),
                protocol: rpc::forge::NetworkSecurityGroupRuleProtocol::NsgRuleProtoTcp.into(),
                action: rpc::forge::NetworkSecurityGroupRuleAction::NsgRuleActionDeny.into(),
                priority: 9001,
                source_net: Some(
                    rpc::forge::network_security_group_rule_attributes::SourceNet::SrcPrefix(
                        "2001:db8:3333:4444:5555:6666:7777:8888/128".to_string(),
                    ),
                ),
                destination_net: Some(
                    rpc::forge::network_security_group_rule_attributes::DestinationNet::DstPrefix(
                        "2001:db8:3333:4444:5555:6666:7777:9999/128".to_string(),
                    ),
                ),
            }),
        },
    ];

    let instance = rpc::Instance {
        id: Some("9afaedd3-b36e-4603-a029-8b94a82b89a0".parse().unwrap()),
        machine_id: Some("fm100htjsaledfasinabqqer70e2ua5ksqj4kfjii0v0a90vulps48c1h7g".parse().unwrap()),
        metadata: Some(rpc::Metadata {
            name: "test-instance".to_string(),
            description: String::new(),
            labels: vec![],
        }),
        instance_type_id: None,
        config: Some(rpc::InstanceConfig {
            tenant: Some(rpc::TenantConfig {
                tenant_organization_id: "Forge-simulation-tenant".to_string(),
                hostname: None,
                tenant_keyset_ids: vec![],
            }),
            os: Some(rpc::forge::InstanceOperatingSystemConfig {
                phone_home_enabled: false,
                run_provisioning_instructions_on_every_boot: false,
                user_data: Some("".to_string()),
                variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(rpc::forge::InlineIpxe {
                    ipxe_script: " chain http://10.217.126.4/public/blobs/internal/x86_64/qcow-imager.efi loglevel=7 console=ttyS0,115200 console=tty0 pci=realloc=off image_url=https://pbss.s8k.io/v1/AUTH_team-forge/images.qcow2/carbide-dev-environment/carbide-dev-environment-latest.qcow2".to_string(),
                })),
            }),
            network: Some(rpc::InstanceNetworkConfig {
                interfaces: vec![rpc::InstanceInterfaceConfig {
                    function_type: rpc::InterfaceFunctionType::Physical.into(),
                    network_segment_id: Some("a7cdeab1-84ec-48a2-ab59-62863d311f26".parse().unwrap()),
                    network_details: Some(rpc::forge::instance_interface_config::NetworkDetails::SegmentId(
                        "a7cdeab1-84ec-48a2-ab59-62863d311f26".parse().unwrap(),
                    )),
                    device: None,
                    device_instance: 0,
                    virtual_function_id: None,
                    ip_address: None,
                    ipv6_interface_config: None,
                    routing_profile: None,
                }],
                #[allow(deprecated)]
                auto: false,
                auto_config: None,
            }),
            infiniband: None,
            network_security_group_id: None,
            dpu_extension_services: None,
            nvlink: None,
            spxconfig: None,
            power_profile: None,
        }),
        status: Some(rpc::InstanceStatus {
            tenant: Some(rpc::InstanceTenantStatus {
                state: rpc::TenantState::Ready.into(),
                state_details: "".to_string(),
            }),
            network: Some(rpc::InstanceNetworkStatus {
                interfaces: vec![rpc::InstanceInterfaceStatus {
                    virtual_function_id: None,
                    mac_address: Some("5C:25:73:9E:92:F2".to_string()),
                    addresses: vec![
                        "2001:db8::146".to_string(),
                        "10.217.104.146".to_string(),
                    ],
                    gateways: vec![
                        "2001:db8::145/64".to_string(),
                        "10.217.104.145/30".to_string(),
                    ],
                    prefixes: vec![
                        "2001:db8::146/128".to_string(),
                        "10.217.104.146/32".to_string(),
                    ],
                    device: None,
                    device_instance: 0u32,
                    vpc_id: None,
                    resolved_vpc_prefixes: None,
                }],
                configs_synced: rpc::SyncState::Synced.into(),
            }),
            infiniband: Some(rpc::InstanceInfinibandStatus {
                ib_interfaces: vec![],
                configs_synced: rpc::SyncState::Synced.into(),
            }),
            dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesStatus {
                dpu_extension_services: vec![],
                configs_synced: rpc::SyncState::Synced.into(),
            }),
            nvlink: Some(rpc::forge::InstanceNvLinkStatus {
                gpu_statuses: vec![],
                configs_synced: rpc::SyncState::Synced.into(),
            }),
            configs_synced: rpc::SyncState::Synced.into(),
            update: None,
            spx_status: None,
        }),
        network_config_version: "V1-T1748645613333257".to_string(),
        ib_config_version: "V1-T1748645613333260".to_string(),
        config_version: "V1-T1748645613333260".to_string(),
        dpu_extension_service_version: "V1-T1748645613333257".to_string(),
        tpm_ek_certificate: None,
        nvlink_config_version: "V1-T1748645613333260".to_string(),
    };

    let netconf = rpc::forge::ManagedHostNetworkConfigResponse {
        service_interfaces: vec![],
        service_vpc_slot_inventory: None,
        bgp_leaf_session_password: Some("this_is_not_a_real_password".to_string()),
        site_global_vpc_vni: None,
        asn: 65535,
        datacenter_asn: 11414,
        common_internal_route_target: Some(rpc_common::RouteTarget {
            asn: 11415,
            vni: Some(200),
            vni_self: false,
        }),
        additional_route_target_imports: vec![rpc_common::RouteTarget {
            asn: 11111,
            vni: Some(22222),
            vni_self: false,
        }],
        routing_profile: Some(rpc::forge::RoutingProfile {
            tenant_leak_communities_accepted: false,
            leak_default_route_from_underlay: false,
            leak_tenant_host_routes_to_underlay: false,
            accepted_leaks_from_underlay: vec![],
            allowed_anycast_prefixes: vec![],
            route_target_imports: vec![rpc_common::RouteTarget {
                asn: 44444,
                vni: Some(55555),
                vni_self: false,
            }],
            route_targets_on_exports: vec![rpc_common::RouteTarget {
                asn: 77415,
                vni: Some(800),
                vni_self: false,
            }],
        }),

        anycast_site_prefixes: vec!["5.255.255.0/24".to_string()],
        tenant_host_asn: Some(65100),
        dhcp_servers: vec!["127.0.0.1".to_string()],
        ntp_servers: vec![],
        dhcpv6_server_preference: Some(255),
        vni_device: "".to_string(),

        managed_host_config: Some(rpc::forge::ManagedHostNetworkConfig {
            loopback_ip: loopback_ip_override.unwrap_or("127.0.0.1").to_string(),
            loopback_ip_v6: None,
            quarantine_state: quarantine_host.then(|| rpc::forge::ManagedHostQuarantineState {
                mode: rpc::forge::ManagedHostQuarantineMode::BlockAllTraffic.into(),
                reason: Some("quarantine must not wait for metadata readiness".to_string()),
            }),
        }),
        managed_host_config_version: config_version.clone(),
        use_admin_network: !quarantine_host,
        admin_interface: Some(admin_interface),
        tenant_interfaces: vec![tenant_interface],
        network_security_policy_overrides,
        instance_network_config_version: config_version,
        instance_id: None,
        network_virtualization_type: Some(
            rpc::forge::VpcVirtualizationType::from(virtualization_type).into(),
        ),
        vpc_vni: None,
        route_servers: vec![],
        remote_id: loopback_ip_override
            .map(|_| "test-dpu")
            .unwrap_or_default()
            .to_string(),
        deny_prefixes: vec!["1.1.1.1/32".to_string()],
        site_fabric_prefixes: vec!["2.2.2.2/32".to_string()],
        site_fabric_null_routes: None,
        vpc_peer_vnis_authoritative: true,
        vpc_isolation_behavior: rpc::forge::VpcIsolationBehaviorType::VpcIsolationMutual.into(),
        deprecated_deny_prefixes: vec![],
        enable_dhcp: true,
        host_interface_id: loopback_ip_override
            .map(|_| "11111111-1111-1111-1111-111111111111".to_string()),
        min_dpu_functioning_links: None,
        is_primary_dpu: true,
        dpu_network_pinger_type: Some("HbnExec".to_string()),
        internet_l3_vni: Some(1337),
        stateful_acls_enabled: true,
        instance: Some(instance),
        dpu_extension_services: vec![],
        astra_config,
        // The FMDS scenario also exercises the required OVS side effect while
        // entering the quarantined tenant network; retries must not repeat it.
        use_admin_network_changed: quarantine_host.then_some(true),
    };
    common::respond(netconf)
}

async fn handle_record_netstat(
    AxumState(state): AxumState<Arc<Mutex<State>>>,
    body: Bytes,
) -> Result<axum::response::Response, StatusCode> {
    let payload = body.get(5..).ok_or(StatusCode::BAD_REQUEST)?;
    let status =
        rpc::forge::DpuNetworkStatus::decode(payload).map_err(|_| StatusCode::BAD_REQUEST)?;
    let mut state = state.lock().await;
    state.num_health_reports.fetch_add(1, Ordering::SeqCst);
    state.post_config_wait_seen |= status.dpu_health.as_ref().is_some_and(|health| {
        health
            .alerts
            .iter()
            .any(|alert| alert.id == "PostConfigCheckWait")
    });
    let rejected = std::mem::take(&mut state.reject_next_network_status);
    state
        .network_status_reports
        .push((Instant::now(), rejected));
    if rejected {
        return Ok((
            [
                ("content-type", "application/grpc"),
                ("grpc-status", "14"),
                ("grpc-message", "injected%20status%20rejection"),
            ],
            "",
        )
            .into_response());
    }
    state.last_network_status = Some(status);
    Ok(common::respond(()).into_response())
}

async fn handle_dpu_agent_upgrade_check(
    AxumState(state): AxumState<Arc<Mutex<State>>>,
) -> impl axum::response::IntoResponse {
    state.lock().await.has_checked_for_upgrade = true;
    common::respond(rpc::forge::DpuAgentUpgradeCheckResponse {
        should_upgrade: false,
        package_version: carbide_version::v!(build_version)[1..].to_string(),
        server_version: carbide_version::v!(build_version).to_string(),
    })
}

async fn handle_update_agent_reported_inventory() -> impl axum::response::IntoResponse {
    common::respond(())
}

async fn handle_get_dpu_info_list(
    AxumState(state): AxumState<Arc<Mutex<State>>>,
) -> impl axum::response::IntoResponse {
    {
        state
            .lock()
            .await
            .num_get_dpu_ips
            .fetch_add(1, Ordering::SeqCst);
    }
    common::respond(rpc::forge::GetDpuInfoListResponse {
        dpu_list: vec![
            DpuInfo {
                id: "fm100dsvstfujf6mis0gpsoi81tadmllicv7rqo4s7gc16gi0t2478672vg".to_string(),
                loopback_ip: "172.20.0.119".to_string(),
                observed_status: None,
            },
            DpuInfo {
                id: "fm100dsjd1vuk6gklgvh0ao8t7r7tk1pt101ub5ck0g3j7lqcm8h3rf1p8g".to_string(),
                loopback_ip: "172.20.0.200".to_string(),
                observed_status: None,
            },
        ],
    })
}

fn timestamp_from_secs_nanos(secs: i64, nanos: i32) -> Timestamp {
    let duration = Duration::from_secs(secs as u64) + Duration::from_nanos(nanos as u64);
    let system_time = UNIX_EPOCH + duration;
    Timestamp::from(system_time)
}

#[allow(deprecated)]
async fn handle_find_interfaces() -> impl axum::response::IntoResponse {
    let interface = rpc::forge::MachineInterface {
        id: Some(
            MachineInterfaceId::from_str("c5ab152e-5ba6-4785-bce0-04e9711f6dc6")
                .expect("valid interface id"),
        ),
        attached_dpu_machine_id: Some(
            DpuMachineId::from_str("fm100ds7f2c7e5i3nlho0cfq4ke3ma8chtpn49qm6j12rv63l6fa527j8c0")
                .expect("valid machine id"),
        ),
        machine_id: Some(
            MachineId::from_str("fm100hthn93o41u6eq8b9ijnjtpce73m8uuh7hd462gtj9p0cvl08oo5r0g")
                .expect("valid machine id"),
        ),
        segment_id: Some(
            NetworkSegmentId::from_str("63ad6dcf-2a60-476b-a2c0-e3a85cd326d0")
                .expect("valid network segment id"),
        ),
        hostname: "10-217-100-219".to_string(),
        domain_id: Some(
            DomainId::from_str("fd37cb4a-cad9-4d50-be07-b54f818dcde3").expect("valid domain id"),
        ),
        primary_interface: false,
        mac_address: "9C:63:C0:E6:9F:50".to_string(),
        address: vec!["10.217.100.219".to_string()],
        vendor: None,
        created: Some(timestamp_from_secs_nanos(1773084037, 3824000)),
        last_dhcp: Some(timestamp_from_secs_nanos(1773097243, 70533000)),
        is_bmc: Some(false),
        interface_type: Some(InterfaceType::Data.into()),
        power_shelf_id: None,
        switch_id: None,
        association_type: Some(InterfaceAssociationType::Machine.into()),
    };

    common::respond(rpc::forge::InterfaceList {
        interfaces: vec![interface],
    })
}

async fn handler(uri: Uri) -> impl IntoResponse {
    tracing::debug!(?uri, "General request handler received request");
    StatusCode::NOT_FOUND
}

// copied from api/src/model/config_version.rs
fn now() -> DateTime<Utc> {
    let mut now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before Unix epoch");
    let round = now.as_nanos() % 1000;
    now -= Duration::from_nanos(round as _);

    let naive = DateTime::from_timestamp(now.as_secs() as i64, now.subsec_nanos())
        .expect("out-of-range number of seconds and/or invalid nanosecond");
    Utc.from_utc_datetime(&naive.naive_utc())
}
