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

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use crate::SshError;
use crate::ssh_client::{AuthConfig, HostKeyVerification};

async fn execute_command(
    command: &str,
    ip_address: SocketAddr,
    username: String,
    password: String,
) -> Result<(String, u32), SshError> {
    let host = ip_address.ip().to_string();
    let auth = AuthConfig::Password { password };
    let client = crate::ssh_client::SshClientConfig {
        host: &host,
        port: 22,
        username: &username,
        auth: Some(&auth),
        host_key_verification: HostKeyVerification::Insecure,
    }
    .make_authenticated_client()
    .await?;

    let result = client.execute_ssh_command(command).await?;

    Ok((result.stdout, result.exit_status))
}

pub async fn disable_rshim(
    ip_address: SocketAddr,
    username: String,
    password: String,
) -> Result<(), SshError> {
    let command = "systemctl disable --now rshim";
    let (_stdout, _exit_code) = execute_command(command, ip_address, username, password).await?;
    Ok(())
}

pub async fn enable_rshim(
    ip_address: SocketAddr,
    username: String,
    password: String,
) -> Result<(), SshError> {
    let command = "systemctl enable --now rshim";
    let (_stdout, _exit_code) = execute_command(command, ip_address, username, password).await?;
    Ok(())
}

pub async fn is_rshim_enabled(
    ip_address: SocketAddr,
    username: String,
    password: String,
) -> Result<bool, SshError> {
    let command = "systemctl is-active rshim";
    let (stdout, _exit_code) = execute_command(command, ip_address, username, password).await?;
    Ok(stdout.trim() == "active")
}

/// Copy a BFB file to the BMC rshim device using the system `scp` binary.
/// The password is passed via `SSH_ASKPASS` with a temp script.
pub async fn copy_bfb_to_bmc_rshim(
    ip_address: SocketAddr,
    username: String,
    password: String,
    bfb_path: String,
) -> Result<(), SshError> {
    let timeout_secs: u64 = 30 * 60;

    scp_cmd_write(
        &bfb_path,
        "/dev/rshim0/boot",
        ip_address,
        &username,
        &password,
        timeout_secs,
    )
    .await
}

/// Run a file copy using the system `scp` binary.
/// Password auth is handled via `SSH_ASKPASS` backed by a temp script.
async fn scp_cmd_write(
    local_path: &str,
    remote_path: &str,
    ip_address: SocketAddr,
    username: &str,
    password: &str,
    timeout_secs: u64,
) -> Result<(), SshError> {
    tracing::info!(
        local_path,
        remote_path,
        %ip_address,
        "starting system scp copy to BMC rshim",
    );

    let askpass_dir = tempfile::tempdir().map_err(io_ssh_error)?;
    let askpass_path = askpass_dir.path().join("askpass.sh");
    {
        use std::os::unix::fs::PermissionsExt;
        let escaped = password.replace('\'', "'\\''");
        std::fs::write(
            &askpass_path,
            format!("#!/bin/sh\nprintf '%s' '{escaped}'\n"),
        )
        .map_err(io_ssh_error)?;
        std::fs::set_permissions(&askpass_path, std::fs::Permissions::from_mode(0o700))
            .map_err(io_ssh_error)?;
    }

    let mut child = scp_command(local_path, remote_path, ip_address, username)
        .env("SSH_ASKPASS", &askpass_path)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", "dummy")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(io_ssh_error)?;

    let stderr_pipe = child.stderr.take();
    let stderr_handle = tokio::spawn(async move {
        let mut buf = String::new();
        if let Some(mut stderr) = stderr_pipe {
            use tokio::io::AsyncReadExt;
            let _ = stderr.read_to_string(&mut buf).await;
        }
        buf
    });

    let status = tokio::time::timeout(Duration::from_secs(timeout_secs), child.wait())
        .await
        .map_err(|_| {
            io_ssh_error(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!("scp timed out after {} minutes", timeout_secs / 60),
            ))
        })?
        .map_err(io_ssh_error)?;

    let stderr_output = stderr_handle.await.unwrap_or_default();

    if status.success() {
        tracing::info!(%ip_address, "scp copy to rshim completed");
        return Ok(());
    }

    Err(io_ssh_error(std::io::Error::other(format!(
        "scp failed with {status}: {}",
        stderr_output.trim()
    ))))
}

fn scp_command(
    local_path: &str,
    remote_path: &str,
    ip_address: SocketAddr,
    username: &str,
) -> tokio::process::Command {
    // SCP separates the host and path with ':', so IPv6 needs brackets.
    let host = match ip_address.ip() {
        IpAddr::V4(ip) => ip.to_string(),
        IpAddr::V6(ip) => format!("[{ip}]"),
    };
    let port = ip_address.port().to_string();
    let mut command = tokio::process::Command::new("scp");
    command.args([
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "PubkeyAuthentication=no",
        "-P",
        &port,
        local_path,
        &format!("{username}@{host}:{remote_path}"),
    ]);
    command
}

fn io_ssh_error(e: std::io::Error) -> SshError {
    SshError::Io(e)
}

pub async fn read_obmc_console_log(
    ip_address: SocketAddr,
    username: String,
    password: String,
) -> Result<String, SshError> {
    let command = "cat /var/log/obmc-console.log";
    let (stdout, _exit_code) = execute_command(command, ip_address, username, password).await?;
    Ok(stdout)
}

/// Connect to the DPU serial console via `obmc-console-client` and check
/// whether any line matches the provided patterns. Returns `true` if at
/// least one pattern is found in the console output.
pub async fn check_console_for_markers(
    ip_address: SocketAddr,
    username: String,
    password: String,
    markers: &[&str],
) -> Result<bool, SshError> {
    let command = r#"(printf '\n'; sleep 2) | timeout 5 obmc-console-client 2>/dev/null; true"#;
    let (stdout, _exit_code) = execute_command(command, ip_address, username, password).await?;

    let found = stdout
        .lines()
        .any(|line| markers.iter().any(|marker| line.contains(marker)));
    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use super::scp_command;

    #[test]
    fn scp_arguments_preserve_paths_and_port_for_both_address_families() {
        struct Case {
            name: &'static str,
            address: &'static str,
            port: &'static str,
            destination: &'static str,
        }

        let cases = [
            Case {
                name: "IPv4",
                address: "192.0.2.1:22",
                port: "22",
                destination: "root@192.0.2.1:/dev/rshim0/boot",
            },
            Case {
                name: "IPv6 with custom port",
                address: "[2001:db8::1]:2222",
                port: "2222",
                destination: "root@[2001:db8::1]:/dev/rshim0/boot",
            },
        ];

        for case in cases {
            // Keep a space in the filename to check that it stays one argument.
            let local_path = "/tmp/pre ingestion.bfb";
            let command = scp_command(
                local_path,
                "/dev/rshim0/boot",
                case.address.parse().expect("valid BMC socket address"),
                "root",
            );
            let command = command.as_std();
            assert_eq!(command.get_program(), OsStr::new("scp"), "{}", case.name);
            let expected_args = [
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "PubkeyAuthentication=no",
                "-P",
                case.port,
                local_path,
                case.destination,
            ]
            .map(OsStr::new);
            assert_eq!(
                command.get_args().collect::<Vec<_>>(),
                expected_args,
                "{}",
                case.name,
            );
        }
    }
}
