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

use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use std::{env, fs};

use ::rpc::forge as rpc;
use ::rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig};
use carbide_uuid::machine::MachineId;
use data_encoding::BASE64;
use eyre::WrapErr;
use tokio::process::Command as TokioCommand;
use tokio::time::timeout;

fn make_upgrade_cmd(to_package_version: &str) -> String {
    // We do a `dpkg --configure -a` first to give ourselves a better chance of
    // making it through the self-upgrade if the last one was interrupted.
    format!(
        "DEBIAN_FRONTEND=noninteractive dpkg --configure -a && \
         ip vrf exec mgmt apt-get update -o Dir::Etc::sourcelist=sources.list.d/forge.list -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 && \
         apt-get autoclean && \
         DEBIAN_FRONTEND=noninteractive ip vrf exec mgmt apt-get install --yes --allow-downgrades --reinstall \
         forge-dpu={}",
        shell_escape(to_package_version)
    )
}

fn shell_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''"); // close, escape, reopen
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Check if forge-dpu-agent needs upgrading to a new version, and if yes perform the upgrade
/// Returns true if we just updated and hence need to exit, so the new version can start instead.
pub async fn upgrade(
    forge_api: &str,
    client_config: &ForgeClientConfig,
    machine_id: &MachineId,
    // allow integration test to replace UPGRADE_CMD
    override_upgrade_cmd: Option<&str>,
) -> eyre::Result<bool> {
    let resp = match upgrade_check(forge_api, client_config, machine_id).await {
        Ok(r) => r,
        Err(err) => match err.downcast_ref::<tonic::Status>() {
            Some(grpc_status) if grpc_status.code() == tonic::Code::Internal => {
                // If something is wrong on the server wait for that to be fixed
                tracing::error!(
                    error = ?err,
                    "Internal server error, will not upgrade."
                );
                UpgradeCheckResult {
                    should_upgrade: false,
                    ..Default::default()
                }
            }
            _ => {
                // If something is broken in dpu-agent we need to replace it
                tracing::error!(
                    error = ?err,
                    "Failed upgrade check, forcing upgrade"
                );
                UpgradeCheckResult {
                    should_upgrade: true,
                    ..Default::default()
                }
            }
        },
    };
    if !resp.should_upgrade {
        tracing::trace!("forge-dpu-agent is up to date");
        return Ok(false);
    }

    // Upgrading!

    let binary_path = env::current_exe()?;

    // We do this for two reasons:
    // - Move the file back on upgrade failure
    // - Kernel prevents overwriting inode of running binary, we'd get ETXTBSY.
    let mut backup = binary_path.clone();
    backup.set_extension("BAK");

    // If the updates are overridden for unit-test purposes, then don't move
    // the binary. It will not be replaced by an update - and running the
    // unit-test would require it to be rebuilt
    if override_upgrade_cmd.is_none()
        && let Err(err) = fs::rename(&binary_path, &backup)
    {
        tracing::warn!(
            source = %binary_path.display(),
            destination = %backup.display(),
            error = %err,
            "Failed backing up current binary"
        );
        // keep going - if the rename fails we still want the upgrade
    }

    let upgrade_cmd = override_upgrade_cmd
        .map(|s| s.to_string())
        .unwrap_or_else(|| make_upgrade_cmd(&resp.package_version));
    tracing::info!(
        local_build = carbide_version::v!(build_version),
        remote_build = resp.server_version,
        to_package_version = resp.package_version,
        command = upgrade_cmd,
        version = carbide_version::v!(build_version),
        "Upgrading myself, goodbye.",
    );
    if let Err(err) = clear_apt_metadata_cache() {
        tracing::warn!(error = %err, "Failed clearing apt metadata cache");
        // try the upgrade anyway
    }
    match run_upgrade_cmd(&upgrade_cmd).await {
        Ok(()) => {
            // Upgrade succeeded, we need to restart. We do this by exiting and letting
            // systemd restart us.
            Ok(true)
        }
        Err(err) => {
            tracing::error!(command = upgrade_cmd, error = ?err, "Upgrade failed");
            if override_upgrade_cmd.is_none() {
                fs::rename(backup, binary_path)?;
            }
            eyre::bail!("run_upgrade_cmd failed");
        }
    }
}

async fn upgrade_check(
    forge_api: &str,
    client_config: &ForgeClientConfig,
    machine_id: &MachineId,
) -> eyre::Result<UpgradeCheckResult> {
    let binary_path = env::current_exe()?;
    let binary_mtime = mtime(binary_path.as_path())?;
    let binary_hash = hash_file(binary_path.as_path())?;
    network_upgrade_check(
        forge_api,
        client_config,
        machine_id,
        binary_mtime,
        binary_hash,
    )
    .await
}

fn mtime(p: &Path) -> eyre::Result<SystemTime> {
    let stat = fs::metadata(p).wrap_err_with(|| format!("failed stat of '{}'", p.display()))?;
    let Ok(binary_mtime) = stat.modified() else {
        eyre::bail!(
            "failed reading mtime of forge-dpu-agent binary at '{}'",
            p.display()
        );
    };
    Ok(binary_mtime)
}

fn hash_file(p: &Path) -> eyre::Result<String> {
    // blake3 is almost 2x faster than sha2's sha256 in release mode, and 35x faster in debug mode
    let mut hasher = blake3::Hasher::new();
    let mut f = fs::File::open(p).wrap_err_with(|| format!("open {}", p.display()))?;
    let mut buf = vec![0; 32768].into_boxed_slice();
    loop {
        match f.read(&mut buf) {
            Ok(0) => {
                break;
            }
            Ok(n) => {
                hasher.update(&buf[..n]);
            }
            Err(ref e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(err) => {
                return Err(err.into());
            }
        }
    }
    let hash: [u8; 32] = hasher.finalize().into();
    Ok(BASE64.encode(&hash))
}

#[derive(Debug, Default)]
struct UpgradeCheckResult {
    should_upgrade: bool,
    package_version: String,
    server_version: String,
}

async fn network_upgrade_check(
    forge_api: &str,
    client_config: &ForgeClientConfig,
    machine_id: &MachineId,
    binary_mtime: SystemTime,
    binary_hash: String,
) -> eyre::Result<UpgradeCheckResult> {
    let local_build = carbide_version::v!(build_version);
    let req = rpc::DpuAgentUpgradeCheckRequest {
        machine_id: machine_id.to_string(),
        current_agent_version: local_build.to_string(),
        binary_mtime: Some(binary_mtime.into()),
        binary_sha: binary_hash,
    };

    let mut client =
        forge_tls_client::ForgeTlsClient::retry_build(&ApiConfig::new(forge_api, client_config))
            .await?;
    let resp = client
        .dpu_agent_upgrade_check(tonic::Request::new(req))
        .await
        .map(|response| response.into_inner())?;

    Ok(UpgradeCheckResult {
        should_upgrade: resp.should_upgrade,
        package_version: resp.package_version,
        server_version: resp.server_version,
    })
}

async fn run_upgrade_cmd(upgrade_cmd: &str) -> eyre::Result<()> {
    let mut cmd = TokioCommand::new("bash");
    // Do not kill the upgrade command even if it hangs because that risks losing `/usr/bin/forge-dpu-agent`
    cmd.arg("-c").arg(upgrade_cmd).kill_on_drop(false);
    // This can easily take 60 seconds. systemd watchdog gives us 5 mins, so take 3.
    let out = timeout(Duration::from_secs(180), cmd.output())
        .await
        .wrap_err("timeout")?
        .wrap_err("error running command")?;
    if !out.status.success() {
        tracing::error!(
            stdout = %String::from_utf8_lossy(&out.stdout),
            "Upgrade command stdout"
        );
        tracing::error!(
            stderr = %String::from_utf8_lossy(&out.stderr),
            "Upgrade command stderr"
        );
        eyre::bail!("failed running upgrade command. check logs for stdout/stderr");
    }
    Ok(())
}

/// There are rare but real situations, as yet undetermined, where `apt update` will not download
/// the `Packages` file from our repo. This fixes it.
/// It's possibly something about the `Release` file that doesn't match it's expectation.
/// See https://github.com/NVIDIA/infra-controller/issues/5556
fn clear_apt_metadata_cache() -> eyre::Result<()> {
    const MC: [&str; 2] = [
        "/var/lib/apt/lists/carbide-pxe.forge_public_blobs_internal_apt_dists_focal_Release",
        "/var/lib/apt/lists/carbide-pxe.forge_public_blobs_internal_apt_dists_focal_main_binary-arm64_Packages",
    ];
    for filepath in MC {
        let p = PathBuf::from(filepath);
        if p.exists() {
            fs::remove_file(p)?;
        }
    }
    Ok(())
}

#[test]
fn test_make_upgrade_cmd() {
    assert_eq!(
        make_upgrade_cmd("1.0.0"),
        "DEBIAN_FRONTEND=noninteractive dpkg --configure -a && \
         ip vrf exec mgmt apt-get update -o Dir::Etc::sourcelist=sources.list.d/forge.list -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 && \
         apt-get autoclean && \
         DEBIAN_FRONTEND=noninteractive ip vrf exec mgmt apt-get install --yes --allow-downgrades --reinstall \
         forge-dpu='1.0.0'",
    );

    assert_eq!(
        make_upgrade_cmd("2026.01.16-az51trunk-01-4-g3064b81c4"),
        "DEBIAN_FRONTEND=noninteractive dpkg --configure -a && \
         ip vrf exec mgmt apt-get update -o Dir::Etc::sourcelist=sources.list.d/forge.list -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 && \
         apt-get autoclean && \
         DEBIAN_FRONTEND=noninteractive ip vrf exec mgmt apt-get install --yes --allow-downgrades --reinstall \
         forge-dpu='2026.01.16-az51trunk-01-4-g3064b81c4'",
    );

    assert_eq!(
        make_upgrade_cmd("'; echo evil stuff && sh /tmp/evil.sh"),
        "DEBIAN_FRONTEND=noninteractive dpkg --configure -a && \
         ip vrf exec mgmt apt-get update -o Dir::Etc::sourcelist=sources.list.d/forge.list -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 && \
         apt-get autoclean && \
         DEBIAN_FRONTEND=noninteractive ip vrf exec mgmt apt-get install --yes --allow-downgrades --reinstall \
         forge-dpu=''\\''; echo evil stuff && sh /tmp/evil.sh'",
    );

    assert_eq!(
        make_upgrade_cmd(""),
        "DEBIAN_FRONTEND=noninteractive dpkg --configure -a && \
         ip vrf exec mgmt apt-get update -o Dir::Etc::sourcelist=sources.list.d/forge.list -o Dir::Etc::sourceparts=- -o APT::Get::List-Cleanup=0 && \
         apt-get autoclean && \
         DEBIAN_FRONTEND=noninteractive ip vrf exec mgmt apt-get install --yes --allow-downgrades --reinstall \
         forge-dpu=''",
    );
}
