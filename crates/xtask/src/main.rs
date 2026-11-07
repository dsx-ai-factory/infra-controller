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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

mod error_message_case;
mod event_names;
mod isolated_package_builds;
mod metric_docs;
mod squash_migrations;
mod workspace_deps;

use clap::Parser;

#[derive(Parser)]
#[clap(name = "xtask")]
enum Xtask {
    #[clap(
        name = "check-event-names",
        about = "Check that production instrumented Events have unique event_name identities"
    )]
    CheckEventNames,
    #[clap(
        name = "check-workspace-deps",
        about = "Check for any dependency versions defined in crate-level Cargo.toml's instead of the workspace root"
    )]
    CheckWorkspaceDeps(CheckWorkspaceDeps),
    #[clap(
        name = "check-isolated-package-builds",
        about = "Check that each workspace package builds independently with its default features"
    )]
    IsolatedPackageBuilds,
    #[clap(
        name = "check-metric-docs",
        about = "Check that every #[derive(Event)] counter/histogram has a row in docs/observability/core_metrics.md"
    )]
    CheckMetricDocs(CheckMetricDocs),
    #[clap(
        name = "squash-migrations",
        about = "Create a single squashed migration from all existing migrations in crates/api-db/migrations"
    )]
    SquashMigrations(squash_migrations::Args),
    #[clap(
        name = "lint-error-messages",
        about = "Check that error messages follow C-GOOD-ERR (lowercase, no trailing period)"
    )]
    LintErrorMessages(LintErrorMessages),
}

#[derive(Parser, Debug)]
struct LintErrorMessages {
    #[clap(
        short,
        long,
        help = "Rewrite offending error messages in place instead of just reporting them"
    )]
    fix: bool,
}

#[derive(Parser, Debug)]
struct CheckMetricDocs {
    #[clap(
        short,
        long,
        help = "Add any missing rows to docs/observability/core_metrics.md instead of just reporting them"
    )]
    fix: bool,
}

#[derive(Parser, Debug)]
struct CheckWorkspaceDeps {
    #[clap(
        short,
        long,
        help = "Fix any dependencies defined in crate-level Cargo.toml's by moving them to the workspace root"
    )]
    fix: bool,
}

#[tokio::main]
async fn main() -> eyre::Result<()> {
    match Xtask::parse() {
        Xtask::CheckEventNames => event_names::check()?,
        Xtask::CheckWorkspaceDeps(CheckWorkspaceDeps { fix }) => {
            workspace_deps::check(fix)?.report_and_exit()
        }
        Xtask::IsolatedPackageBuilds => isolated_package_builds::check()?,
        Xtask::CheckMetricDocs(CheckMetricDocs { fix }) => metric_docs::check(fix)?,
        Xtask::SquashMigrations(args) => squash_migrations::run(args).await?,
        Xtask::LintErrorMessages(LintErrorMessages { fix }) => {
            error_message_case::check(fix)?.report_and_exit()
        }
    }
    Ok(())
}
