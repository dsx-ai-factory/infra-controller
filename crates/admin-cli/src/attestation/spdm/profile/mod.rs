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

//! `attestation spdm profile` subcommands: the operator's view of, and edits
//! to, the policy attestation applies to each hardware class.

mod args;
#[cfg(test)]
mod tests;

pub(crate) use args::Cmd;
use args::{Create, Delete, Get, List, Mode, Patterns, Update};
use model::attestation::profile::{ANY_HARDWARE_CLASS, validate_new_hardware_class};
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{
    AttestationProfile, AttesterSelection, AttesterSelectionMode, ComponentIdMatch,
    CreateAttestationProfileRequest, DeleteAttestationProfileRequest, GetAttestationProfileRequest,
    UpdateAttestationProfileRequest, component_id_match,
};
use serde::Serialize;
use tonic::Status;

use super::write_output;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::rpc::ApiClient;

/// Named in the one error a format can produce, so it says which command.
const WHAT: &str = "attestation profiles";

/// The profile RPCs these commands need, so each one can be exercised without a
/// server.
trait ProfileClient {
    async fn list(&self) -> Result<Vec<AttestationProfile>, Status>;
    async fn get(
        &self,
        request: GetAttestationProfileRequest,
    ) -> Result<AttestationProfile, Status>;
    async fn create(
        &self,
        request: CreateAttestationProfileRequest,
    ) -> Result<AttestationProfile, Status>;
    async fn update(
        &self,
        request: UpdateAttestationProfileRequest,
    ) -> Result<AttestationProfile, Status>;
    async fn delete(&self, request: DeleteAttestationProfileRequest) -> Result<(), Status>;
}

impl ProfileClient for ApiClient {
    async fn list(&self) -> Result<Vec<AttestationProfile>, Status> {
        Ok(self.0.list_attestation_profiles().await?.profiles)
    }

    async fn get(
        &self,
        request: GetAttestationProfileRequest,
    ) -> Result<AttestationProfile, Status> {
        self.0.get_attestation_profile(request).await
    }

    async fn create(
        &self,
        request: CreateAttestationProfileRequest,
    ) -> Result<AttestationProfile, Status> {
        self.0.create_attestation_profile(request).await
    }

    async fn update(
        &self,
        request: UpdateAttestationProfileRequest,
    ) -> Result<AttestationProfile, Status> {
        self.0.update_attestation_profile(request).await
    }

    async fn delete(&self, request: DeleteAttestationProfileRequest) -> Result<(), Status> {
        self.0.delete_attestation_profile(request).await?;
        Ok(())
    }
}

impl Run for List {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        self.execute(&ctx.api_client, ctx.config.format, &mut ctx.output_file)
            .await
    }
}

impl Run for Get {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        self.execute(&ctx.api_client, ctx.config.format, &mut ctx.output_file)
            .await
    }
}

impl Run for Create {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        self.execute(&ctx.api_client, ctx.config.format, &mut ctx.output_file)
            .await
    }
}

impl Run for Update {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        self.execute(&ctx.api_client, ctx.config.format, &mut ctx.output_file)
            .await
    }
}

impl Run for Delete {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        self.execute(&ctx.api_client).await
    }
}

impl List {
    async fn execute(
        self,
        client: &impl ProfileClient,
        format: OutputFormat,
        output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    ) -> CarbideCliResult<()> {
        let profiles = client.list().await?;
        write_profiles(profiles, format, output).await
    }
}

impl Get {
    async fn execute(
        self,
        client: &impl ProfileClient,
        format: OutputFormat,
        output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    ) -> CarbideCliResult<()> {
        let profile = client
            .get(GetAttestationProfileRequest {
                hardware_class: self.hardware_class,
            })
            .await?;
        write_profile(profile, format, output).await
    }
}

impl Create {
    async fn execute(
        self,
        client: &impl ProfileClient,
        format: OutputFormat,
        output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    ) -> CarbideCliResult<()> {
        // The server refuses this too. Checked here so a misspelled class costs
        // a local error naming the accepted ones rather than a round trip.
        validate_new_hardware_class(&self.hardware_class)
            .map_err(|error| CarbideCliError::GenericError(error.to_string()))?;

        let profile = client
            .create(CreateAttestationProfileRequest {
                hardware_class: self.hardware_class,
                selection: Some(selection(self.mode, self.patterns)),
            })
            .await?;
        write_profile(profile, format, output).await
    }
}

impl Update {
    async fn execute(
        self,
        client: &impl ProfileClient,
        format: OutputFormat,
        output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    ) -> CarbideCliResult<()> {
        if self.hardware_class == ANY_HARDWARE_CLASS && self.mode == Mode::None && !self.force {
            return Err(CarbideCliError::GenericError(format!(
                "mode none on the '{ANY_HARDWARE_CLASS}' fallback stops attesting every \
                 hardware class without a profile of its own; re-run with --force to confirm"
            )));
        }

        let profile = client
            .update(UpdateAttestationProfileRequest {
                hardware_class: self.hardware_class,
                selection: Some(selection(self.mode, self.patterns)),
                if_version_match: self.if_version_match,
            })
            .await?;
        write_profile(profile, format, output).await
    }
}

impl Delete {
    async fn execute(self, client: &impl ProfileClient) -> CarbideCliResult<()> {
        if self.hardware_class == ANY_HARDWARE_CLASS && !self.force {
            return Err(CarbideCliError::GenericError(format!(
                "removing the '{ANY_HARDWARE_CLASS}' fallback leaves every hardware class \
                 without a profile of its own attesting nothing; re-run with --force to confirm"
            )));
        }

        let hardware_class = self.hardware_class.clone();
        client
            .delete(DeleteAttestationProfileRequest {
                hardware_class: self.hardware_class,
                if_version_match: self.if_version_match,
            })
            .await?;

        // Reported on stderr because there is no profile left to print, and the
        // requested output format describes stdout.
        eprintln!("Removed the attestation profile for hardware class {hardware_class}");
        Ok(())
    }
}

/// The selection to store. `--exact` values are sent before `--prefix` ones,
/// since a command line does not record the order two flags were interleaved
/// in. The server rejects a mode and pattern count that disagree.
fn selection(mode: Mode, patterns: Patterns) -> AttesterSelection {
    let exact = patterns
        .exact
        .into_iter()
        .map(component_id_match::Pattern::Exact);
    let prefix = patterns
        .prefix
        .into_iter()
        .map(component_id_match::Pattern::Prefix);
    AttesterSelection {
        mode: AttesterSelectionMode::from(mode).into(),
        component_ids: exact
            .chain(prefix)
            .map(|pattern| ComponentIdMatch {
                pattern: Some(pattern),
            })
            .collect(),
    }
}

/// The `--mode` spelling for a mode read back off the wire. A server newer than
/// this build could name one it has no flag for.
pub(super) fn mode_name(mode: i32) -> &'static str {
    match AttesterSelectionMode::try_from(mode) {
        Ok(AttesterSelectionMode::None) => "none",
        Ok(AttesterSelectionMode::All) => "all",
        Ok(AttesterSelectionMode::Allowlist) => "allowlist",
        Ok(AttesterSelectionMode::Denylist) => "denylist",
        Ok(AttesterSelectionMode::Unspecified) | Err(_) => "unknown",
    }
}

/// The patterns of one selection, each prefixed with the kind of match it is.
/// Empty for `all` and `none`, which take none.
fn component_ids(selection: &AttesterSelection) -> Vec<String> {
    selection
        .component_ids
        .iter()
        .filter_map(|id| id.pattern.as_ref())
        .map(|pattern| match pattern {
            component_id_match::Pattern::Exact(id) => format!("exact:{id}"),
            component_id_match::Pattern::Prefix(prefix) => format!("prefix:{prefix}"),
        })
        .collect()
}

/// One profile as this CLI reports it, with the mode spelled the way `--mode`
/// accepts it rather than as the wire integer.
#[derive(Serialize)]
struct ProfileView {
    hardware_class: String,
    mode: &'static str,
    component_ids: Vec<String>,
    /// What `--if-version-match` takes to apply the next edit to this profile.
    version: String,
    updated_at: Option<String>,
    updated_by: String,
}

impl From<AttestationProfile> for ProfileView {
    fn from(profile: AttestationProfile) -> Self {
        // The server always sets a selection; an absent one reports as the
        // unknown mode rather than as a mode it is not.
        let selection = profile.selection.unwrap_or_default();
        Self {
            hardware_class: profile.hardware_class,
            mode: mode_name(selection.mode),
            component_ids: component_ids(&selection),
            version: profile.version,
            updated_at: profile.updated_at.map(|at| at.to_string()),
            updated_by: profile.updated_by,
        }
    }
}

/// One row per profile, whether the command read one or all of them.
fn profile_table(profiles: &[ProfileView]) -> Table {
    let mut table = Table::new();
    table.set_titles(row![
        "HARDWARE CLASS",
        "MODE",
        "COMPONENT IDS",
        "VERSION",
        "UPDATED BY"
    ]);
    for profile in profiles {
        table.add_row(row![
            profile.hardware_class,
            profile.mode,
            profile.component_ids.join(", "),
            profile.version,
            profile.updated_by,
        ]);
    }
    table
}

/// Serialized as one profile, because a command that read one profile
/// returning a list of one would be awkward to script against.
async fn write_profile(
    profile: AttestationProfile,
    format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    let view = ProfileView::from(profile);
    let table = profile_table(std::slice::from_ref(&view));
    write_output(&view, table, WHAT, format, output).await
}

async fn write_profiles(
    profiles: Vec<AttestationProfile>,
    format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    let views: Vec<ProfileView> = profiles.into_iter().map(Into::into).collect();
    let table = profile_table(&views);
    write_output(&views, table, WHAT, format, output).await
}
