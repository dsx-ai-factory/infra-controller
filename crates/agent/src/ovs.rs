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

use std::time::Duration;

use eyre::WrapErr;

use crate::instrumentation::OvsRestart;

/// ovs-vswitchd is part of HBN. It handles network packets in user-space using DPDK
/// (https://www.dpdk.org/). By default it uses 100% of a CPU core to poll for new packets, never
/// yielding. Here we set it to yield the CPU for up to 100us if it's been idle recently.
/// 100us was recommended by NBU/HBN team.
pub(super) async fn set_vswitchd_yield() -> eyre::Result<()> {
    let mut cmd = tokio::process::Command::new("/usr/bin/ovs-vsctl");
    // table: o
    // record: .
    // column: other_config
    // key: pmd-sleep-max
    // value: 100 nanoseconds
    cmd.arg("set")
        .arg("o")
        .arg(".")
        .arg("other_config:pmd-sleep-max=100")
        .kill_on_drop(true);
    let cmd_str = super::pretty_cmd(cmd.as_std());
    tracing::trace!(command = cmd_str.as_str(), "set_ovs_vswitchd_yield running");

    // It takes less than 1s, so allow up to 5
    let out = tokio::time::timeout(std::time::Duration::from_secs(5), cmd.output())
        .await
        .wrap_err("timeout")?
        .wrap_err("error running command")?;
    if !out.status.success() {
        tracing::error!(
            command = cmd_str.as_str(),
            stdout = %String::from_utf8_lossy(&out.stdout),
            stderr = %String::from_utf8_lossy(&out.stderr),
            "OVS command failed"
        );
        eyre::bail!("failed running ovs-vsctl command. check logs for stdout/stderr");
    }

    Ok(())
}

/// Restart the OVS service (ovs-vswitchd) via systemctl.
pub(super) async fn restart_ovs() -> eyre::Result<()> {
    let restart = tokio::time::timeout(
        Duration::from_secs(180),
        tokio::process::Command::new("systemctl")
            .args(["restart", "ovs-vswitchd.service"])
            .kill_on_drop(true)
            .output(),
    )
    .await;
    let restart = match restart {
        Ok(Ok(output)) => output,
        Ok(Err(e)) => eyre::bail!("failed to execute systemctl restart: {}", e),
        Err(_) => eyre::bail!("timeout (180s) waiting for ovs-vswitchd.service restart"),
    };

    if !restart.status.success() {
        eyre::bail!(
            "systemctl restart ovs-vswitchd.service failed (status: {})",
            restart.status
        );
    }

    carbide_instrument::emit(OvsRestart::Succeeded {});
    Ok(())
}
