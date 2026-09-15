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
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use eyre::Context;
use futures_util::FutureExt;
use lazy_static::lazy_static;
use russh::ChannelMsg;
use russh::keys::PrivateKeyWithHashAlg;
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

mod util;

use ::ssh_console::shutdown_handle::ShutdownHandle;
use api_test_helper::utils::REPO_ROOT;
use util::ssh_console_test_helper;

use crate::util::ssh_client::{ConnectionConfig, PermissiveSshClient};
use crate::util::{BaselineTestAssertion, MockBmcType, run_baseline_test_environment};

static TENANT_SSH_PUBKEY: &str = include_str!("fixtures/tenant_ssh_key.pub");

lazy_static! {
    static ref TENANT_SSH_KEY_PATH: PathBuf =
        REPO_ROOT.join("crates/ssh-console/tests/fixtures/tenant_ssh_key");
    static ref ADMIN_SSH_KEY_PATH: PathBuf =
        REPO_ROOT.join("crates/ssh-console/tests/fixtures/admin_ssh_key");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ssh_console() -> eyre::Result<()> {
    if std::env::var("REPO_ROOT").is_err() {
        tracing::info!("Skipping running ssh-console integration tests, as REPO_ROOT is not set");
        return Ok(());
    }
    let Some(env) = run_baseline_test_environment(vec![
        MockBmcType::Ipmi,
        MockBmcType::Ssh,
        MockBmcType::LenovoAmiSsh,
        MockBmcType::DpuSsh,
    ])
    .await?
    else {
        return Ok(());
    };

    // Run new ssh-console
    let handle = ssh_console_test_helper::spawn(env.mock_api_server.addr.port(), None).await?;

    // Run the same assertions we do with legacy ssh-console
    env.run_baseline_assertions(
        handle.addr,
        "new-ssh-console",
        &[
            BaselineTestAssertion::ConnectAsInstanceId,
            BaselineTestAssertion::ConnectAsMachineId,
        ],
        || Some(ssh_console_test_helper::get_metrics(handle.metrics_address).boxed()),
        true,
    )
    .await?;

    // Now test things specific to the new ssh-console

    // Shut down ssh-console now so we can assert on final log lines (and make sure it shuts down
    // properly.)
    handle.spawn_handle.shutdown_and_wait().await;

    let logs_path = handle.logs_dir.path();

    assert!(
        logs_path.exists(),
        "logs were not created for new ssh-console"
    );

    for mock_host in env.mock_hosts.iter() {
        let log_path = logs_path.join(format!("{}_{}.log", mock_host.machine_id, mock_host.bmc_ip));
        assert!(
            log_path.exists(),
            "did not see any logs at {}",
            log_path.display()
        );

        let logs = std::fs::read_to_string(&log_path)
            .with_context(|| format!("error reading log file at {}", log_path.display()))?;

        // Find "ssh-console started at" lines
        let (started_lines, other_lines) = logs
            .lines()
            .partition::<Vec<_>, _>(|l| l.starts_with("--- ssh-console started at "));

        // Find the "ssh-console shutting down at" line
        let (shutting_down_lines, other_lines) = other_lines
            .into_iter()
            .partition::<Vec<_>, _>(|l| l.starts_with("--- ssh-console shutting down at "));

        // Find the "Console connected!" line
        let (console_connected_lines, other_lines) = other_lines
            .into_iter()
            .partition::<Vec<_>, _>(|l| l.starts_with("--- Console connected! ---"));

        assert_eq!(
            started_lines.len(),
            1,
            "{} does not contain at least one line saying `--- ssh-console started at`:\n{}",
            log_path.display(),
            logs
        );

        assert_eq!(
            console_connected_lines.len(),
            1,
            "{} does not contain at least one line saying `--- Console connected! ---`:\n{}",
            log_path.display(),
            logs
        );

        assert!(
            !other_lines.is_empty(),
            "{} does not contain any lines except connection status lines:\n{}",
            log_path.display(),
            logs
        );

        assert_eq!(
            shutting_down_lines.len(),
            1,
            "{} does not contain expected disconnected line:\n{}",
            log_path.display(),
            logs
        );

        assert!(
            logs.lines()
                .last()
                .is_some_and(|l| l == shutting_down_lines[0]),
            "{} did not have the shutdown line as its last line:\n{}",
            log_path.display(),
            logs
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ipmi_sol_conflict_does_not_evict_existing_session_by_default() -> eyre::Result<()> {
    if std::env::var("REPO_ROOT").is_err() {
        tracing::info!("Skipping running ssh-console integration tests, as REPO_ROOT is not set");
        return Ok(());
    }
    let Some(env) = run_baseline_test_environment(vec![MockBmcType::Ipmi]).await? else {
        return Ok(());
    };
    let mock_host = &env.mock_hosts[0];
    let active_sol_session = util::ipmi_sim::activate_sol(
        mock_host
            .ipmi_port
            .expect("IPMI mock should provide an IPMI port"),
    )
    .await?;

    let handle = ssh_console_test_helper::spawn(
        env.mock_api_server.addr.port(),
        Some(ssh_console_test_helper::ConfigOverrides {
            reconnect_interval_base: Some(Duration::from_secs(1)),
            reconnect_interval_max: Some(Duration::from_secs(1)),
            successful_connection_minimum_duration: Some(Duration::from_secs(60)),
            ..Default::default()
        }),
    )
    .await?;

    let machine_id = mock_host.machine_id.to_string();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Ok(metrics) = ssh_console_test_helper::get_metrics(handle.metrics_address).await
                && metrics.lines().any(|line| {
                    line.starts_with("ssh_console_bmc_status{")
                        && line.contains(&machine_id)
                        && line.contains("ConnectionError")
                })
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .context("ssh-console did not report the expected conflicting SOL connection error")?;

    let expected_prompt = format!("root@{} # ", mock_host.machine_id).into_bytes();
    active_sol_session
        .assert_console_works(&expected_prompt)
        .await?;

    handle.spawn_handle.shutdown_and_wait().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ipmi_sol_conflict_recovery_when_enabled() -> eyre::Result<()> {
    if std::env::var("REPO_ROOT").is_err() {
        tracing::info!("Skipping running ssh-console integration tests, as REPO_ROOT is not set");
        return Ok(());
    }
    let Some(env) = run_baseline_test_environment(vec![MockBmcType::Ipmi]).await? else {
        return Ok(());
    };
    let mock_host = &env.mock_hosts[0];
    let active_sol_session = util::ipmi_sim::activate_sol(
        mock_host
            .ipmi_port
            .expect("IPMI mock should provide an IPMI port"),
    )
    .await?;

    let handle = ssh_console_test_helper::spawn(
        env.mock_api_server.addr.port(),
        Some(ssh_console_test_helper::ConfigOverrides {
            reconnect_interval_base: Some(Duration::from_secs(30)),
            reconnect_interval_max: Some(Duration::from_secs(30)),
            successful_connection_minimum_duration: Some(Duration::from_secs(60)),
            force_deactivate_conflicting_ipmi_sol_sessions: Some(true),
        }),
    )
    .await?;

    let user = mock_host.machine_id.to_string();
    let expected_prompt = format!("root@{} # ", mock_host.machine_id).into_bytes();
    // Connecting within twenty seconds proves the stale holder was evicted and retried immediately,
    // rather than waiting for the configured 30-second normal reconnect delay. Keep the holder
    // process alive until then so dropping its local PTY cannot make the conflict disappear.
    tokio::time::timeout(
        Duration::from_secs(20),
        util::ssh_client::assert_connection_works_with_retries_and_timeout(
            &ConnectionConfig {
                connection_name: "ssh-console after conflicting IPMI SOL session recovery",
                user: &user,
                private_key_path: &ADMIN_SSH_KEY_PATH,
                addr: handle.addr,
                expected_prompt: &expected_prompt,
            },
            5,
            Duration::from_secs(10),
        ),
    )
    .await
    .context("ssh-console did not recover the conflicting SOL session before normal backoff")??;

    drop(active_sol_session);

    handle.spawn_handle.shutdown_and_wait().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ssh_console_lenovo_sr650_sol_fallback() -> eyre::Result<()> {
    if std::env::var("REPO_ROOT").is_err() {
        tracing::info!("Skipping running ssh-console integration tests, as REPO_ROOT is not set");
        return Ok(());
    }
    let Some(env) = run_baseline_test_environment(vec![MockBmcType::LenovoSr650Ssh]).await? else {
        return Ok(());
    };

    let handle = ssh_console_test_helper::spawn(env.mock_api_server.addr.port(), None).await?;

    env.run_baseline_assertions(
        handle.addr,
        "new-ssh-console Lenovo SR650",
        &[BaselineTestAssertion::ConnectAsMachineId],
        || None,
        false,
    )
    .await?;

    handle.spawn_handle.shutdown_and_wait().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ssh_console_reconnect() -> eyre::Result<()> {
    if std::env::var("REPO_ROOT").is_err() {
        tracing::info!("Skipping running ssh-console integration tests, as REPO_ROOT is not set");
        return Ok(());
    }
    let Some(env) = run_baseline_test_environment(vec![MockBmcType::Ssh]).await? else {
        return Ok(());
    };
    let reconnect_interval_max = Duration::from_secs(5);

    // Run new ssh-console
    let handle = ssh_console_test_helper::spawn(
        env.mock_api_server.addr.port(),
        // Exercise reconnect backoff without letting randomized exponential delays exceed the
        // prompt wait below.
        Some(ssh_console_test_helper::ConfigOverrides {
            reconnect_interval_base: Some(Duration::from_secs(3)),
            reconnect_interval_max: Some(reconnect_interval_max),
            successful_connection_minimum_duration: Some(Duration::from_secs(60)),
            ..Default::default()
        }),
    )
    .await?;

    // Connect to the server and authenticate
    let session = {
        let mut session = russh::client::connect(
            Arc::new(russh::client::Config {
                ..Default::default()
            }),
            handle.addr,
            PermissiveSshClient,
        )
        .await?;

        session
            .authenticate_publickey(
                env.mock_hosts[0].instance_id.to_string(),
                PrivateKeyWithHashAlg::new(
                    Arc::new(
                        russh::keys::load_secret_key(TENANT_SSH_KEY_PATH.as_path(), None)
                            .context("error loading ssh private key")?,
                    ),
                    None,
                ),
            )
            .await
            .context("error authenticating with public key")?;

        Ok::<_, eyre::Error>(session)
    }?;

    // Open a session channel
    let channel = session
        .channel_open_session()
        .await
        .context("error opening session")?;

    // Request PTY
    channel
        .request_pty(false, "xterm", 80, 24, 0, 0, &[])
        .await
        .context("error requesting PTY")?;

    // Request Shell
    channel.request_shell(false).await?;

    let (mut channel_rx, channel_tx) = channel.split();

    // Read from the BMC output in the background, sending a message to prompt_seen_tx every time we
    // see a prompt, until we're done.
    let prompt = format!("root@{} # ", env.mock_hosts[0].machine_id).into_bytes();
    let (prompt_seen_tx, mut prompt_seen_rx) = mpsc::channel(1);
    let (done_tx, mut done_rx) = oneshot::channel::<()>();
    let prompt_reader_handle = tokio::spawn(async move {
        let mut buf = Vec::new();
        loop {
            tokio::select! {
                _ = &mut done_rx => {
                    break;
                }
                res = channel_rx.wait() => match res {
                    Some(ChannelMsg::Data { data }) => {
                        buf.extend_from_slice(&data);
                        if buf.windows(prompt.len()).any(|w| w == prompt) {
                            buf.clear();
                            // Coalesce prompt notifications so repeated newlines cannot make the
                            // reader task block or exit before the test consumes the current prompt.
                            match prompt_seen_tx.try_send(()) {
                                Ok(()) | Err(TrySendError::Full(())) => {}
                                Err(TrySendError::Closed(())) => break,
                            }
                        }
                    }
                    Some(_) => {},
                    None => {
                        break;
                    }
                },
            }
        }
        Ok::<(), eyre::Error>(())
    });

    // Send ctrl+c (break) multiple times, waiting for reconnection after each time
    for _ in 0..5 {
        while prompt_seen_rx.try_recv().is_ok() {}

        let mut newline_interval = tokio::time::interval(Duration::from_secs(1));
        let wait_for_prompt_timeout =
            Instant::now() + reconnect_interval_max + Duration::from_secs(5);
        // Send newlines to wait for prompt to appear
        'wait_for_prompt: loop {
            tokio::select! {
                _ = tokio::time::sleep_until(wait_for_prompt_timeout) => {
                    panic!("Timed out waiting for prompt during reconnect");
                }
                _ = newline_interval.tick() => {
                    tokio::time::timeout(Duration::from_secs(10), channel_tx.data(b"\n".as_slice()))
                        .await
                        .expect("Timed out writing newline while waiting for reconnect")?;
                }
                res = prompt_seen_rx.recv() => match res {
                    Some(()) => break 'wait_for_prompt,
                    None => panic!("Did not see prompt after sending ctrl+C"),
                }
            }
        }

        // Send ctrl+C to cause a disconnect
        tokio::time::timeout(Duration::from_secs(10), channel_tx.data([3u8].as_slice()))
            .await
            .expect("Timed out writing ctrl+C to trigger reconnect")?;
    }

    done_tx.send(()).ok();
    prompt_reader_handle
        .await
        .expect("prompt reader task panicked")?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn test_ssh_console_log_rotation() -> eyre::Result<()> {
    if std::env::var("REPO_ROOT").is_err() {
        tracing::info!("Skipping running ssh-console integration tests, as REPO_ROOT is not set");
        return Ok(());
    }
    let Some(env) =
        run_baseline_test_environment(vec![MockBmcType::Ipmi, MockBmcType::Ssh]).await?
    else {
        return Ok(());
    };

    // Run new ssh-console
    let handle = ssh_console_test_helper::spawn(env.mock_api_server.addr.port(), None).await?;

    // Run the same assertions we do with legacy ssh-console
    env.run_baseline_assertions(
        handle.addr,
        "new-ssh-console",
        // 50 kilobytes should be enough to show log rotation behavior
        &[BaselineTestAssertion::FillLogsAsMachineId(50 * 1024)],
        || None,
        false,
    )
    .await?;

    // Shut down ssh-console now so we can assert on log rotation
    handle.spawn_handle.shutdown_and_wait().await;

    let logs_path = handle.logs_dir.path();

    assert!(
        logs_path.exists(),
        "logs were not created for new ssh-console"
    );

    for mock_host in env.mock_hosts.iter() {
        let log_path = logs_path.join(format!("{}_{}.log", mock_host.machine_id, mock_host.bmc_ip));
        let log_path_0 = logs_path.join(format!(
            "{}_{}.log.0",
            mock_host.machine_id, mock_host.bmc_ip
        ));
        let log_path_1 = logs_path.join(format!(
            "{}_{}.log.1",
            mock_host.machine_id, mock_host.bmc_ip
        ));
        let log_path_2 = logs_path.join(format!(
            "{}_{}.log.2",
            mock_host.machine_id, mock_host.bmc_ip
        ));
        // This one shouldn't exist
        let log_path_3 = logs_path.join(format!(
            "{}_{}.log.3",
            mock_host.machine_id, mock_host.bmc_ip
        ));

        for path in &[&log_path, &log_path_0, &log_path_1, &log_path_2] {
            assert!(path.exists(), "did not see any logs at {}", path.display());
            let size = path.metadata()?.len();
            assert!(
                size <= 1024 * 10,
                "logs at {} exceeded configured size: {} > 10 KiB",
                path.display(),
                size
            );
        }

        assert!(
            !log_path_3.exists(),
            "logs exist at {}, but that's more than configured log rotation",
            log_path_3.display()
        );

        for path in &[&log_path_0, &log_path_1, &log_path_2] {
            let size = path.metadata()?.len();
            assert!(
                size > 1024 * 10 - 100,
                "rotated log at {} was rotated while too-small, {} < (10KiB - 100 bytes)",
                path.display(),
                size
            );
        }
    }

    Ok(())
}

#[ctor::ctor(unsafe)]
fn setup_test_logging() {
    api_test_helper::setup_logging()
}
