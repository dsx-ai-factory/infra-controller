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
use serde::Deserialize;
use tokio::process::Command as TokioCommand;
use tokio::sync::OnceCell;
use tokio::time::timeout;

use crate::containerd::container;

/// How long to wait for `crictl ps`
const TIMEOUT_GET_CONTAINER_ID: Duration = Duration::from_secs(5);

/// How long to wait for an arbitrary command run in the HBN container.
/// `nv config apply` can take > 10s
const TIMEOUT_CONTAINER_CMD: Duration = Duration::from_secs(45);
/// The running version of the HBN container
static HBN_VERSION: OnceCell<String> = OnceCell::const_new();
// Containerd is started in the mgmt VRF.  Processes started in the MGMT VRF are not reachable
// from the default VRF.  This means that we need to run crictl commands in the MGMT VRF in order to
// execute commands in containers.  `RunCommandPredicate` provides abstraction over the `Command`
// and `args` needed to execute health checks.
// We need to move the dpu-agent out of the mgmt VRF because we want to expose services to instances
// running on an X86 machine, eg FMDS, while still being able to use the mgmt VRF to connect to our control plane
#[derive(Debug)]
struct RunCommandPredicate<'a> {
    command: TokioCommand,
    args: Vec<&'a str>,
}
impl<'a> RunCommandPredicate<'a> {
    /// Create a new `RunCommandPredicate` with the appropriate `Command` and `args`
    /// ENV::VAR `IGNORE_MGMT_VRF` dictates which command predicate to use
    fn new(container_id: &'a str) -> Self {
        let ignore_mgmt_vrf = std::env::var("IGNORE_MGMT_VRF").is_ok();
        tracing::trace!(ignore_mgmt_vrf, "RunCommandPredicate: IGNORE_MGMT_VRF");

        match ignore_mgmt_vrf {
            true => Self {
                command: TokioCommand::new("crictl"),
                args: vec!["exec", container_id],
            },
            false => Self {
                command: TokioCommand::new("ip"),
                args: vec!["vrf", "exec", "mgmt", "crictl", "exec", container_id],
            },
        }
    }
}

pub(super) async fn get_hbn_container_id() -> eyre::Result<String> {
    let mut crictl = TokioCommand::new("crictl");
    crictl.kill_on_drop(true);
    let cmd = crictl.args(["ps", "--name=doca-hbn", "-o=json"]);
    let cmd_str = super::pretty_cmd(cmd.as_std());
    let cmd_res = timeout(TIMEOUT_GET_CONTAINER_ID, cmd.output())
        .await
        .wrap_err_with(|| format!("timeout calling {cmd_str}"))?;
    let out = cmd_res.wrap_err(cmd_str.to_string())?;
    if !out.status.success() {
        return Err(eyre::eyre!("{} for cmd '{cmd_str}'", out.status,));
    }

    parse_container_id(&String::from_utf8_lossy(&out.stdout))
}

fn parse_container_id(json: &str) -> eyre::Result<String> {
    let o: CrictlOut = serde_json::from_str(json)?;
    if o.containers.is_empty() {
        return Err(eyre::eyre!(
            "crictl JSON output has empty 'containers' array. is doca-hbn running?"
        ));
    }
    Ok(o.containers[0].id.clone())
}

// Run the given command inside HBN container in a shell. Ignore the output.
pub(super) async fn run_in_container_shell(cmd: &str) -> Result<(), eyre::Report> {
    let container_id = get_hbn_container_id().await?;
    let check_result = true;

    run_in_container(&container_id, &["bash", "-c", cmd], check_result)
        .await
        .wrap_err_with(|| {
            format!("failed executing '{cmd}' in container. check logs in /var/log/doca/hbn/")
        })?;
    Ok(())
}

// Run the given command inside HBN container directly. Return stdout.
pub(super) async fn run_in_container(
    container_id: &str,
    command: &[&str],
    need_success: bool,
) -> eyre::Result<String> {
    let pred = RunCommandPredicate::new(container_id);
    let mut crictl = pred.command;
    let mut args = pred.args;
    args.extend_from_slice(command);

    let cmd = crictl.args(args);
    cmd.kill_on_drop(true);
    let cmd_str = super::pretty_cmd(cmd.as_std());
    tracing::trace!(command = cmd_str.as_str(), "run_in_container");

    let cmd_res = timeout(TIMEOUT_CONTAINER_CMD, cmd.output())
        .await
        .wrap_err_with(|| format!("timeout calling {cmd_str}"))?;
    let out = cmd_res.wrap_err(cmd_str.to_string())?;

    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();

    if need_success && !out.status.success() {
        tracing::debug!(
            command = cmd_str.as_str(),
            stderr = stderr.as_str(),
            "Container command failed"
        );
        return Err(eyre::eyre!(
            "cmd '{cmd_str}' failed with status: {}, stderr: {}, stdout: {}",
            out.status, // includes the string "exit status"
            stderr,
            stdout
        ));
    }
    if !stdout.is_empty() {
        tracing::trace!("{stdout}");
    }
    Ok(stdout)
}

async fn fetch_hbn_version() -> eyre::Result<String> {
    let containers = container::Containers::list().await?;
    let hbn_container = containers.find_by_name("doca-hbn")?;

    let hbn_version = hbn_container
        .image_ref
        .into_iter()
        .map(|x| x.version())
        .next()
        .unwrap_or_default();
    tracing::info!(hbn_version, "HBN version from doca-hbn container");

    Ok(hbn_version)
}

pub(super) async fn read_version() -> eyre::Result<String> {
    Ok(HBN_VERSION
        .get_or_try_init(fetch_hbn_version)
        .await?
        .to_string())
}

#[derive(Deserialize, Debug)]
struct CrictlOut {
    containers: Vec<Container>,
}

#[derive(Deserialize, Debug)]
struct Container {
    id: String,
}

/// This is used to track the state of some one-time changes that need to be made
/// inside the HBN container.
#[derive(Default)]
pub(super) struct HBNContainerFileConfigs {
    // A Some(container_id) indicates we've previously seen and modified that
    // container, and no further action is needed unless we see a different
    // container ID appear.
    last_fixed_container_id: Option<String>,
}

impl HBNContainerFileConfigs {
    /// This takes care of a couple of file-based configurations that are
    /// currently not supported in NVUE (like ifupdown2 policy and neighmgrd
    /// configuration).
    pub(super) async fn ensure_configs(&mut self) -> eyre::Result<()> {
        let current_container_id = get_hbn_container_id().await?;

        match self.last_fixed_container_id.as_ref() {
            // We've seen and fixed this container before, nothing to do.
            Some(last_container_id) if last_container_id.as_str() == current_container_id => Ok(()),

            // We haven't seen the container before, either because we (the
            // agent process) just started, or because the HBN container was
            // replaced.
            l => {
                tracing::info!(
                    container_id = current_container_id.as_str(),
                    previous_container_id = l.map_or("None", |s| s.as_str()),
                    "HBN container ID is new to us, updating its neighbor learning config"
                );
                set_strict_neighbor_learning(current_container_id.as_str())
                    .await
                    .inspect(|_| {
                        self.last_fixed_container_id.replace(current_container_id);
                    })
            }
        }
    }
}

async fn set_strict_neighbor_learning(container_id: &str) -> eyre::Result<()> {
    // let container_id = get_hbn_container_id().await?;

    write_hbn_ifupdown2_arp_policy(container_id).await?;
    set_neighmgr_subnet_checks(container_id).await?;

    Ok(())
}

async fn write_hbn_ifupdown2_arp_policy(container_id: &str) -> eyre::Result<()> {
    const IFUPDOWN2_POLICY_DIR: &str = "/etc/network/ifupdown2/policy.d";
    const POLICY_FILE_NAME: &str = "forge-arp-accept.json";
    const POLICY_FILE_CONTENTS: &str =
        r#"{"address":{"module_globals":{"l3_intf_arp_accept":"0"}}}"#;
    // If there are currently tenant vlan interfaces present, we'll need to
    // reload their config to apply the new policy.
    const RELOAD_COMMAND: &str = "ifreload -a";

    let bash_command = format!(
        "mkdir -p {IFUPDOWN2_POLICY_DIR} && echo '{POLICY_FILE_CONTENTS}' > {IFUPDOWN2_POLICY_DIR}/{POLICY_FILE_NAME} && {RELOAD_COMMAND}"
    );
    run_in_container(container_id, &["bash", "-c", bash_command.as_str()], true)
        .await
        .map(|_stdout| ())
}

async fn set_neighmgr_subnet_checks(container_id: &str) -> eyre::Result<()> {
    const NEIGHMGR_CONFIG_PATH: &str = "/etc/cumulus/neighmgr.conf";
    // These contents must be rendered through the shell's printf builtin
    const NEIGHMGR_CONFIG_CONTENTS: &str = "[snooper]\nsubnet_checks=1\n";
    const NEIGHMGR_SERVICE_RESTART_COMMAND: &str = "supervisorctl restart neighmgr";

    let bash_command = format!(
        "printf '{NEIGHMGR_CONFIG_CONTENTS}' > {NEIGHMGR_CONFIG_PATH} && {NEIGHMGR_SERVICE_RESTART_COMMAND}"
    );

    run_in_container(container_id, &["bash", "-c", bash_command.as_str()], true)
        .await
        .map(|_stdout| ())
}

/// Try to parse the HBN version out of the value of the system build reported
/// by NVUE.
pub(super) fn parse_nvue_build_as_hbn_version(build_value: &str) -> eyre::Result<String> {
    // We expect build_value to look like this: "HBN 3.2.0"
    build_value
        .strip_prefix("HBN ")
        .ok_or_else(|| eyre::eyre!("couldn't find \"HBN \" prefix in build_value"))
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::parse_container_id;
    const CRICTL_OUT: &str = r#"
{
  "containers": [
    {
      "id": "f11d4746b230d51598bac048331072597a87303fede8c1812e01612c496bbc43",
      "podSandboxId": "b5703f93d448f305b391c2583384b7d1a4e2266c35d12b0e0f2f01fe5083f93d",
      "metadata": {
        "name": "doca-hbn",
        "attempt": 0
      },
      "image": {
        "image": "sha256:05f1047133f9852bd739590fa53071cc6f6eb7cce0a695ce981ddba81317c368",
        "annotations": {
        }
      },
      "imageRef": "sha256:05f1047133f9852bd739590fa53071cc6f6eb7cce0a695ce981ddba81317c368",
      "state": "CONTAINER_RUNNING",
      "createdAt": "1678127057518777146",
      "labels": {
        "io.kubernetes.container.name": "doca-hbn",
        "io.kubernetes.pod.name": "doca-hbn-service-idaho-hamper.forge.local",
        "io.kubernetes.pod.namespace": "default",
        "io.kubernetes.pod.uid": "949491dc6d16952d446a7d0e80da5b18"
      },
      "annotations": {
        "io.kubernetes.container.hash": "ee4ee15b",
        "io.kubernetes.container.restartCount": "0",
        "io.kubernetes.container.terminationMessagePath": "/dev/termination-log",
        "io.kubernetes.container.terminationMessagePolicy": "File",
        "io.kubernetes.pod.terminationGracePeriod": "30"
      }
    }
  ]
}
"#;

    #[test]
    fn test_parse_container_id() -> eyre::Result<()> {
        assert_eq!(
            parse_container_id(CRICTL_OUT)?,
            "f11d4746b230d51598bac048331072597a87303fede8c1812e01612c496bbc43"
        );
        Ok(())
    }
}
