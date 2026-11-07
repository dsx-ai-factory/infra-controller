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

mod args;
#[cfg(test)]
mod tests;

use std::future::Future;
use std::io::{BufRead, Write};
use std::time::Duration;

pub(crate) use args::{ChangeProfile, ReleaseInactiveVni, Show};
use carbide_uuid::vpc::VpcId;
use config_version::ConfigVersion;
use eyre::{Context, ensure};
use model::resource_pool::common::{EXTERNAL_VPC_VNI, VPC_VNI};
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{
    VpcChangeRoutingProfileRequest, VpcReleaseInactiveVniRequest, VpcReleaseInactiveVniResult,
    VpcRoutingState, VpcRoutingStateRequest,
};
use serde::Serialize;
use tonic::{Code, Status};

use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;
use crate::{async_write, async_writeln};

// Match the client's default when no override is configured. The whole-call
// deadline also bounds connection retries and body reads; expiry cannot undo a commit.
const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(300);

trait RoutingClient {
    async fn routing_state(&self, id: VpcId) -> Result<VpcRoutingState, Status>;
    async fn change_profile(
        &self,
        request: VpcChangeRoutingProfileRequest,
    ) -> Result<VpcRoutingState, Status>;
    async fn release(
        &self,
        request: VpcReleaseInactiveVniRequest,
    ) -> Result<VpcReleaseInactiveVniResult, Status>;
}

impl RoutingClient for ApiClient {
    async fn routing_state(&self, id: VpcId) -> Result<VpcRoutingState, Status> {
        self.0
            .get_vpc_routing_state(VpcRoutingStateRequest { id: Some(id) })
            .await
    }

    async fn change_profile(
        &self,
        request: VpcChangeRoutingProfileRequest,
    ) -> Result<VpcRoutingState, Status> {
        self.0.change_vpc_routing_profile(request).await
    }

    async fn release(
        &self,
        request: VpcReleaseInactiveVniRequest,
    ) -> Result<VpcReleaseInactiveVniResult, Status> {
        self.0.release_vpc_inactive_vni(request).await
    }
}

impl Run for Show {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        Ok(self
            .execute(
                &ctx.api_client,
                ctx.config.request_timeout,
                ctx.config.format,
                &mut ctx.output_file,
            )
            .await?)
    }
}

impl Run for ChangeProfile {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        ctx.assert_cloud_unsafe_op_message()?;
        Ok(self
            .execute(
                &ctx.api_client,
                ctx.config.request_timeout,
                ctx.config.format,
                &mut ctx.output_file,
                confirm_interactively,
            )
            .await?)
    }
}

impl Run for ReleaseInactiveVni {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        ctx.assert_cloud_unsafe_op_message()?;
        Ok(self
            .execute(
                &ctx.api_client,
                ctx.config.request_timeout,
                ctx.config.format,
                &mut ctx.output_file,
                confirm_interactively,
            )
            .await?)
    }
}

impl Show {
    async fn execute(
        self,
        client: &impl RoutingClient,
        request_timeout: Option<Duration>,
        format: OutputFormat,
        output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    ) -> eyre::Result<()> {
        let state = read_state(client, self.id, request_timeout).await?;
        write_state(&state, None, format, output)
            .await
            .wrap_err_with(|| format!("could not write routing state for VPC {}", self.id))
    }
}

impl ChangeProfile {
    async fn execute(
        self,
        client: &impl RoutingClient,
        request_timeout: Option<Duration>,
        format: OutputFormat,
        output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
        confirm: impl AsyncFnOnce(&str) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        let before = read_state(client, self.id, request_timeout).await?;
        let selection = match self.vni {
            Some(vni) => format!("exact destination VNI {vni}"),
            None => "automatic VNI selection (reuse the retained destination or allocate)".into(),
        };
        let version = approve_version(
            &before,
            self.if_version_match,
            &format!(
                "Change VPC {} to configured profile {:?} with {selection}. The previous active VNI remains allocated; this does not verify DPU or fabric convergence.",
                self.id, self.routing_profile_type
            ),
            confirm,
        )
        .await?;

        let request = VpcChangeRoutingProfileRequest {
            id: Some(self.id),
            if_version_match: Some(version.to_string()),
            routing_profile_type: self.routing_profile_type.clone(),
            vni: self.vni,
        };
        let after = rpc_attempt(client.change_profile(request), request_timeout)
            .await
            .map_err(|status| mutation_error(self.id, status))?;
        self.validate_result(&before, &after, version)
            .wrap_err_with(|| uncertain_result(self.id))?;
        write_state(&after, None, format, output)
            .await
            .wrap_err_with(|| {
                format!(
                    "core committed the routing change but its result could not be written; {}",
                    inspection(self.id)
                )
            })?;
        eprintln!("Core configuration updated; DPU and fabric convergence has not been verified");
        Ok(())
    }

    fn validate_result(
        &self,
        before: &VpcRoutingState,
        after: &VpcRoutingState,
        observed_version: ConfigVersion,
    ) -> eyre::Result<()> {
        let version = validate_state(after, self.id)?;
        check_advanced_version(observed_version, version)?;
        ensure!(
            after.routing_profile_type.as_deref() == Some(self.routing_profile_type.as_str()),
            "core returned a different routing profile"
        );
        let retained = after
            .retained_allocation
            .as_ref()
            .ok_or_else(|| eyre::eyre!("core did not acknowledge the retained allocation"))?;
        ensure!(
            retained.vni == before.active_vni,
            "core did not retain the previous active VNI"
        );
        ensure!(
            (1..=16_777_215).contains(&after.active_vni)
                && (1..=16_777_215).contains(&retained.vni),
            "core returned a VNI outside the routing-profile transition range"
        );
        if let Some(vni) = self.vni {
            ensure!(
                after.active_vni == vni,
                "core did not select the requested VNI {vni}"
            );
        }
        if let Some(previously_retained) = &before.retained_allocation {
            ensure!(
                after.active_vni == previously_retained.vni
                    && retained.pool_name != previously_retained.pool_name,
                "core did not reuse the retained allocation from the other pool"
            );
        }
        Ok(())
    }
}

impl ReleaseInactiveVni {
    async fn execute(
        self,
        client: &impl RoutingClient,
        request_timeout: Option<Duration>,
        format: OutputFormat,
        output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
        confirm: impl AsyncFnOnce(&str) -> eyre::Result<()>,
    ) -> eyre::Result<()> {
        let before = read_state(client, self.id, request_timeout).await?;
        ensure!(
            before
                .retained_allocation
                .as_ref()
                .map(|allocation| allocation.vni)
                == Some(self.expected_inactive_vni),
            "the observed inactive allocation does not match VNI {}; {}",
            self.expected_inactive_vni,
            inspection(self.id)
        );
        let version = approve_version(
            &before,
            self.if_version_match,
            &format!(
                "Release inactive VNI {} from VPC {} at the displayed version. Confirm that target and direct-peer DPUs and fabric routes no longer use this observed allocation, and concurrent attachment, peering, and routing changes are held. An earlier --confirm-convergence does not verify this allocation.",
                self.expected_inactive_vni, self.id
            ),
            confirm,
        )
        .await?;
        let request = VpcReleaseInactiveVniRequest {
            id: Some(self.id),
            if_version_match: Some(version.to_string()),
            expected_inactive_vni: Some(self.expected_inactive_vni),
        };
        let result = rpc_attempt(client.release(request), request_timeout)
            .await
            .map_err(|status| mutation_error(self.id, status))?;
        let version = self
            .validate_result(&before, &result, version)
            .wrap_err_with(|| uncertain_result(self.id))?;
        // Cleanup acknowledges the unchanged active configuration and the
        // exact released VNI; it does not return another allocation snapshot.
        let after = VpcRoutingState {
            version: version.to_string(),
            retained_allocation: None,
            ..before
        };
        write_state(&after, Some(result.released_inactive_vni), format, output)
            .await
            .wrap_err_with(|| {
                format!(
                    "core released the inactive VNI but its result could not be written; {}",
                    inspection(self.id)
                )
            })
    }

    fn validate_result(
        &self,
        before: &VpcRoutingState,
        result: &VpcReleaseInactiveVniResult,
        observed_version: ConfigVersion,
    ) -> eyre::Result<ConfigVersion> {
        let vpc = result
            .vpc
            .as_ref()
            .ok_or_else(|| eyre::eyre!("core did not acknowledge the VPC after release"))?;
        ensure!(vpc.id == Some(self.id), "core acknowledged a different VPC");
        let version = vpc
            .version
            .parse()
            .wrap_err("invalid VPC version in release response")?;
        check_advanced_version(observed_version, version)?;
        ensure!(
            result.released_inactive_vni == self.expected_inactive_vni,
            "core acknowledged a different released VNI"
        );
        let status = vpc
            .status
            .as_ref()
            .ok_or_else(|| eyre::eyre!("missing VPC status in release response"))?;
        let config = vpc
            .config
            .as_ref()
            .ok_or_else(|| eyre::eyre!("missing VPC config in release response"))?;
        ensure!(
            status.vni == Some(before.active_vni)
                && config.routing_profile_type == before.routing_profile_type,
            "core changed the active routing configuration during release"
        );
        Ok(version)
    }
}

async fn approve_version(
    state: &VpcRoutingState,
    requested: Option<ConfigVersion>,
    action: &str,
    confirm: impl AsyncFnOnce(&str) -> eyre::Result<()>,
) -> eyre::Result<ConfigVersion> {
    let observation = format!(
        "Observed routing state:\n{}",
        serde_json::to_string_pretty(state)?
    );
    if let Some(version) = requested {
        check_observation(state, version)?;
        eprintln!("{observation}");
        return Ok(version);
    }

    let version = state.version.parse().wrap_err("invalid observed version")?;
    confirm(&format!(
        "{observation}\n\n{action}\n\nThis approves a new action, not a retry of an earlier attempt. If repeating this request, add --if-version-match {version} and keep the same VPC, profile, VNI selection (including omission), and site connection options. After an ambiguous error, inspect state first."
    ))
    .await?;
    Ok(version)
}

async fn confirm_interactively(message: &str) -> eyre::Result<()> {
    let message = message.to_owned();
    tokio::task::spawn_blocking(move || {
        read_confirmation(
            &message,
            &mut std::io::stdin().lock(),
            &mut std::io::stderr().lock(),
        )
    })
    .await
    .wrap_err("confirmation task failed")?
}

fn read_confirmation(
    message: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> eyre::Result<()> {
    write!(output, "{message}\n\nType yes to proceed: ")
        .wrap_err("could not write confirmation prompt")?;
    output
        .flush()
        .wrap_err("could not flush confirmation prompt")?;
    let mut answer = String::new();
    input
        .read_line(&mut answer)
        .wrap_err("could not read confirmation")?;
    ensure!(
        answer.trim() == "yes",
        "operation cancelled; no mutation was sent"
    );
    Ok(())
}

async fn rpc_attempt<T>(
    call: impl Future<Output = Result<T, Status>>,
    request_timeout: Option<Duration>,
) -> Result<T, Status> {
    let timeout = request_timeout.unwrap_or(DEFAULT_RPC_TIMEOUT);
    tokio::time::timeout(timeout, call).await.map_err(|_| {
        Status::deadline_exceeded(format!(
            "VPC routing RPC exceeded its {timeout:?} attempt deadline"
        ))
    })?
}

async fn read_state(
    client: &impl RoutingClient,
    id: VpcId,
    request_timeout: Option<Duration>,
) -> eyre::Result<VpcRoutingState> {
    let state = rpc_attempt(client.routing_state(id), request_timeout)
        .await
        .map_err(|status| {
            let context = if status.code() == Code::Unimplemented {
                "core does not support GetVpcRoutingState"
            } else {
                "could not inspect VPC routing state"
            };
            eyre::Report::new(status).wrap_err(format!("{context} for {id}"))
        })?;
    validate_state(&state, id)?;
    Ok(state)
}

fn validate_state(state: &VpcRoutingState, id: VpcId) -> eyre::Result<ConfigVersion> {
    ensure!(
        state.id == Some(id),
        "core returned a missing or different VPC identity"
    );
    let version: ConfigVersion = state
        .version
        .parse()
        .wrap_err("invalid routing-state version")?;
    ensure!(
        version.version_nr() != 0,
        "core returned an invalid zero configuration version"
    );
    if let Some(retained) = &state.retained_allocation {
        ensure!(
            matches!(retained.pool_name.as_str(), VPC_VNI | EXTERNAL_VPC_VNI)
                && retained.vni != state.active_vni,
            "core returned an invalid retained allocation"
        );
    }
    Ok(version)
}

fn check_observation(state: &VpcRoutingState, expected: ConfigVersion) -> eyre::Result<()> {
    ensure!(
        state
            .version
            .parse::<ConfigVersion>()
            .wrap_err("invalid observed version")?
            == expected,
        "the observed VPC version no longer matches --if-version-match; this does not prove whether an earlier attempt committed; keep the original request unchanged and inspect current state"
    );
    Ok(())
}

fn check_advanced_version(before: ConfigVersion, after: ConfigVersion) -> eyre::Result<()> {
    ensure!(
        after.version_nr() == before.increment().version_nr(),
        "core did not acknowledge the next configuration version"
    );
    Ok(())
}

fn inspection(id: VpcId) -> String {
    format!(
        "inspect with nico-admin-cli vpc routing-state {id} using the same site connection options"
    )
}

fn uncertain_result(id: VpcId) -> String {
    format!(
        "core returned an invalid acknowledgement and may have committed; do not retry with a fresh version; {}",
        inspection(id)
    )
}

fn mutation_error(id: VpcId, status: Status) -> eyre::Report {
    let advice = match status.code() {
        Code::Unimplemented => "core does not support this VPC routing operation",
        Code::FailedPrecondition => {
            "core rejected the request; a stale version does not prove whether an earlier attempt committed"
        }
        Code::InvalidArgument
        | Code::NotFound
        | Code::PermissionDenied
        | Code::Unauthenticated
        | Code::ResourceExhausted => "core rejected the VPC routing request",
        _ => "core may have committed the VPC routing request; do not retry with a fresh version",
    };
    eyre::Report::new(status).wrap_err(format!("{advice}; {}", inspection(id)))
}

#[derive(Serialize)]
struct RoutingOutput<'a> {
    #[serde(flatten)]
    state: &'a VpcRoutingState,
    #[serde(skip_serializing_if = "Option::is_none")]
    released_inactive_vni: Option<u32>,
}

async fn write_state(
    state: &VpcRoutingState,
    released_inactive_vni: Option<u32>,
    format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> eyre::Result<()> {
    let view = RoutingOutput {
        state,
        released_inactive_vni,
    };
    match format {
        OutputFormat::Json => async_writeln!(output, "{}", serde_json::to_string_pretty(&view)?)?,
        OutputFormat::Yaml => async_writeln!(output, "{}", serde_yaml::to_string(&view)?)?,
        OutputFormat::AsciiTable => {
            let mut table = Table::new();
            table.set_titles(row!["Field", "Value"]);
            table.add_row(row![
                "VPC ID",
                state.id.map(|id| id.to_string()).unwrap_or_default()
            ]);
            table.add_row(row!["Version", state.version]);
            table.add_row(row![
                "Routing Profile",
                state.routing_profile_type.as_deref().unwrap_or_default()
            ]);
            table.add_row(row!["Active VNI", state.active_vni]);
            table.add_row(row![
                "Retained Pool",
                state
                    .retained_allocation
                    .as_ref()
                    .map(|allocation| allocation.pool_name.as_str())
                    .unwrap_or_default()
            ]);
            table.add_row(row![
                "Retained VNI",
                state
                    .retained_allocation
                    .as_ref()
                    .map(|allocation| allocation.vni.to_string())
                    .unwrap_or_default()
            ]);
            if let Some(vni) = released_inactive_vni {
                table.add_row(row!["Released VNI", vni]);
            }
            async_write!(output, "{table}")?;
        }
        OutputFormat::Csv => eyre::bail!("CSV output is not supported for VPC routing commands"),
    }
    Ok(())
}
