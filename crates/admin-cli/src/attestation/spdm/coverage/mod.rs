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

//! `attestation spdm coverage`: which hardware classes the site has, and what
//! would supply the attestation policy for each.

pub(super) mod args;
#[cfg(test)]
mod tests;

use args::Args;
use model::attestation::profile::ANY_HARDWARE_CLASS;
use prettytable::{Table, row};
use rpc::admin_cli::OutputFormat;
use rpc::forge::{AttestationCoverage, GetAttestationCoverageResponse};
use serde::Serialize;
use tonic::Status;

use super::profile::mode_name;
use super::write_output;
use crate::cfg::run::Run;
use crate::cfg::runtime::RuntimeContext;
use crate::errors::CarbideCliResult;
use crate::rpc::ApiClient;

/// Stands in for a count that has no meaning in the `any` row, which is not
/// keyed to hardware that endpoints are explored as.
const NOT_APPLICABLE: &str = "—";

/// Stands in for the class of the endpoints exploration has recorded none for.
/// Parenthesised because every other value in the column is a class name a
/// profile can be keyed to.
const NO_CLASS_RECORDED: &str = "(no class recorded)";

/// The coverage read, so the command can be exercised without a server.
trait CoverageClient {
    async fn coverage(&self) -> Result<GetAttestationCoverageResponse, Status>;
}

impl CoverageClient for ApiClient {
    async fn coverage(&self) -> Result<GetAttestationCoverageResponse, Status> {
        self.0.get_attestation_coverage().await
    }
}

impl Run for Args {
    async fn run(self, ctx: &mut RuntimeContext) -> CarbideCliResult<()> {
        self.execute(&ctx.api_client, ctx.config.format, &mut ctx.output_file)
            .await
    }
}

impl Args {
    async fn execute(
        self,
        client: &impl CoverageClient,
        format: OutputFormat,
        output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    ) -> CarbideCliResult<()> {
        let coverage = client.coverage().await?;
        write_coverage(&coverage, format, output).await
    }
}

/// What would supply the policy for a class, and the mode it would apply.
/// Spelled by the server, so this does not restate the resolution rule.
fn would_use(coverage: i32, mode: Option<i32>) -> String {
    let mode = mode.map(mode_name).unwrap_or_default();
    match AttestationCoverage::try_from(coverage) {
        Ok(AttestationCoverage::OwnProfile) => format!("its own profile ({mode})"),
        Ok(AttestationCoverage::AnyFallback) => format!("any ({mode})"),
        Ok(AttestationCoverage::NoProfile) => {
            "nothing: no profile for this class and no any fallback".to_string()
        }
        Ok(AttestationCoverage::ClassUnrecognized) => {
            "nothing: exploration did not recognise this hardware".to_string()
        }
        Ok(AttestationCoverage::ClassNotRecorded) => {
            "nothing: no class recorded; explore these endpoints again".to_string()
        }
        Ok(AttestationCoverage::Unspecified) | Err(_) => {
            "unknown to this client; the server is newer".to_string()
        }
    }
}

/// Whether a profile keyed to this class supplies the policy. `n/a` where no
/// profile may be keyed to it at all, which is the `unrecognized` marker and
/// the endpoints carrying no class.
fn own_profile(coverage: i32) -> &'static str {
    match AttestationCoverage::try_from(coverage) {
        Ok(AttestationCoverage::OwnProfile) => "yes",
        Ok(AttestationCoverage::AnyFallback | AttestationCoverage::NoProfile) => "no",
        Ok(AttestationCoverage::ClassUnrecognized | AttestationCoverage::ClassNotRecorded) => "n/a",
        Ok(AttestationCoverage::Unspecified) | Err(_) => "",
    }
}

/// One row of coverage as this CLI reports it.
#[derive(Serialize)]
struct CoverageView {
    /// Absent for the endpoints exploration has recorded no class for, which
    /// arrive with the class empty because absence is not a class name.
    hardware_class: Option<String>,
    /// Absent for the `any` row, which is never recorded on an endpoint.
    explored_endpoints: Option<i32>,
    own_profile: &'static str,
    would_use: String,
}

/// Every class the site has, then `any`. The `any` row is listed even when no
/// fallback is stored, so the posture for everything unprofiled is visible
/// rather than inferred.
fn coverage_views(coverage: &GetAttestationCoverageResponse) -> Vec<CoverageView> {
    let mut views: Vec<_> = coverage
        .entries
        .iter()
        .map(|entry| CoverageView {
            hardware_class: (!entry.hardware_class.is_empty())
                .then(|| entry.hardware_class.clone()),
            explored_endpoints: Some(entry.endpoints),
            own_profile: own_profile(entry.coverage),
            would_use: would_use(entry.coverage, entry.mode),
        })
        .collect();

    let (own_profile, would_use) = match coverage.any_profile_mode {
        Some(mode) => ("yes", format!("its own profile ({})", mode_name(mode))),
        None => ("no", "nothing: no any fallback is stored".to_string()),
    };
    views.push(CoverageView {
        hardware_class: Some(ANY_HARDWARE_CLASS.to_string()),
        explored_endpoints: None,
        own_profile,
        would_use,
    });
    views
}

async fn write_coverage(
    coverage: &GetAttestationCoverageResponse,
    format: OutputFormat,
    output: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
) -> CarbideCliResult<()> {
    let views = coverage_views(coverage);
    let mut table = Table::new();
    table.set_titles(row![
        "HARDWARE CLASS",
        "EXPLORED ENDPOINTS",
        "OWN PROFILE",
        "WOULD USE"
    ]);
    for view in &views {
        table.add_row(row![
            view.hardware_class.as_deref().unwrap_or(NO_CLASS_RECORDED),
            view.explored_endpoints
                .map(|count| count.to_string())
                .unwrap_or(NOT_APPLICABLE.to_string()),
            view.own_profile,
            view.would_use,
        ]);
    }
    write_output(&views, table, "attestation coverage", format, output).await
}
