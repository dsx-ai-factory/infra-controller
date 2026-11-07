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
use std::process::Command;

use cargo_metadata::MetadataCommand;
use eyre::{Context, ContextCompat, bail};

/// Checks that every workspace package builds independently with default features.
pub(super) fn check() -> eyre::Result<()> {
    let packages = workspace_packages()?;
    let mut failures = Vec::new();

    for package in packages {
        println!("Checking isolated package build for {package}");

        // Keep the check read-only; stale lockfiles should fail in CI.
        let status = Command::new(cargo())
            .args(["check", "--locked", "-p", &package])
            .status()
            .with_context(|| format!("failed to run cargo check for {package}"))?;

        if !status.success() {
            failures.push(package);
        }
    }

    if failures.is_empty() {
        return Ok(());
    }

    eprintln!(
        "Isolated package builds failed for: {}",
        failures.join(", ")
    );

    bail!("one or more isolated package builds failed")
}

fn workspace_packages() -> eyre::Result<Vec<String>> {
    let metadata = MetadataCommand::new()
        .no_deps()
        .exec()
        .context("failed to run cargo metadata")?;

    metadata
        .workspace_members
        .iter()
        .map(|member| {
            metadata
                .packages
                .iter()
                .find(|package| &package.id == member)
                .map(|package| package.name.clone())
                .with_context(|| format!("workspace member {member} missing from cargo metadata"))
        })
        .collect::<eyre::Result<Vec<_>>>()
}

fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string())
}
