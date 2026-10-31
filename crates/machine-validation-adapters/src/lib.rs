/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

//! Container contract adapters independent of Machine Validation orchestration.
//!
//! Adapters prepare typed invocations and normalize output. The framework owns
//! site approval, image pulls, container execution, deadlines, cleanup and logs.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::Value;

mod dsx_health_validation;
mod standard;

struct Registration {
    name: &'static str,
    adapter: &'static dyn ContainerPluginAdapter,
}

static ADAPTERS: &[Registration] = &[Registration {
    name: "DSXHealthValidation",
    adapter: &dsx_health_validation::DsxHealthValidation,
}];

/// Names accepted for explicit per-revision adapter selection.
pub fn adapter_names() -> impl Iterator<Item = &'static str> {
    ADAPTERS.iter().map(|registration| registration.name)
}

/// Resolve an adapter. Omission uses the standard NICo contract; empty names fail.
pub fn resolve(name: Option<&str>) -> Result<&'static dyn ContainerPluginAdapter, String> {
    let Some(name) = name else {
        return Ok(&standard::StandardAdapter);
    };
    ADAPTERS
        .iter()
        .find(|registration| registration.name == name)
        .map(|registration| registration.adapter)
        .ok_or_else(|| format!("unsupported container plugin adapter {name:?}"))
}

/// Static host access declared by an approved adapter, independent of user parameters.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeRequirements {
    /// Expose all GPUs through the NVIDIA container runtime.
    pub all_gpus: bool,
    /// Run as container root to read root-owned dependencies, with capabilities dropped.
    pub run_as_root: bool,
}

/// Immutable request and filesystem locations supplied for one attempt.
#[derive(Clone, Copy)]
pub struct AdapterContext<'a> {
    /// NICo input metadata, including machine identity and plugin parameters.
    pub input: &'a Value,
    /// Host directory for prepared input files.
    pub input_dir: &'a Path,
    /// Host directory containing container-written output files.
    pub output_dir: &'a Path,
    /// Container-visible contract directory.
    pub contract_dir: &'a Path,
}

/// Bounded output from a completed container process.
pub struct AdapterOutput<'a> {
    /// Exit code, or None if terminated by a signal.
    pub exit_code: Option<i32>,
    /// Captured stdout bytes without text conversion.
    pub stdout: &'a [u8],
    /// Whether stdout exceeded the capture limit.
    pub stdout_truncated: bool,
}

/// Read-only host dependency requested by a trusted adapter.
#[derive(Debug, PartialEq, Eq)]
pub struct ReadOnlyMount {
    /// Absolute source path on Scout.
    pub source: PathBuf,
    /// Absolute destination path inside the container.
    pub target: PathBuf,
}

/// Prepared command and dependencies; image and privilege settings stay with the framework.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct AdapterInvocation {
    /// Optional override of the image's baked-in entrypoint.
    pub entrypoint: Option<String>,
    /// Arguments supplied after the image reference.
    pub arguments: Vec<String>,
    /// Additional container environment variables.
    pub environment: BTreeMap<String, String>,
    /// Host dependencies mounted read-only.
    pub mounts: Vec<ReadOnlyMount>,
}

/// Container-specific behavior behind the generic execution boundary.
pub trait ContainerPluginAdapter: Send + Sync {
    /// Declare static runtime requirements; these do not grant privileged or full-host access.
    fn requirements(&self) -> RuntimeRequirements {
        RuntimeRequirements::default()
    }
    /// Validate command and parameters before accepting an immutable revision.
    fn validate_registration(
        &self,
        entrypoint: &[String],
        parameters: &Value,
    ) -> Result<(), String>;
    /// Prepare invocation data within the framework's attempt deadline.
    fn prepare<'a>(
        &'a self,
        context: AdapterContext<'a>,
        entrypoint: &'a [String],
    ) -> BoxFuture<'a, Result<AdapterInvocation, String>>;
    /// Convert this container's output into a normalized health result.
    fn interpret(
        &self,
        context: AdapterContext<'_>,
        output: AdapterOutput<'_>,
    ) -> Result<PluginResult, String>;
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// Normalized health assessment consumed by the framework.
pub struct PluginResult {
    #[serde(rename = "contractVersion")]
    /// Normalized result contract version.
    pub contract_version: String,
    /// Normalized result document kind.
    pub kind: String,
    /// Health outcome: pass, fail, or error.
    pub outcome: String,
    /// Bounded human-readable assessment summary.
    pub summary: String,
    #[serde(default)]
    /// Optional normalized severity.
    pub severity: Option<String>,
    #[serde(default)]
    /// Bounded diagnostic findings.
    pub findings: Vec<PluginFinding>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
/// One diagnostic finding in a normalized assessment.
pub struct PluginFinding {
    /// Diagnostic identifier.
    pub name: String,
    /// Diagnostic explanation.
    pub message: String,
}
