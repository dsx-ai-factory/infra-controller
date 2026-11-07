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
use std::collections::HashSet;
use std::fmt::Write;

use prettytable::{Row, Table};
use rpc::Metadata;
use rpc::admin_cli::OutputFormat;

use crate::errors::{CarbideCliError, CarbideCliResult};
use crate::{async_write, async_writeln};

/// Display metadata (name, description, labels) in the
/// requested output format. Shared across machine, rack,
/// switch, and power shelf metadata show commands.
pub(crate) async fn display_metadata(
    output_file: &mut Box<dyn tokio::io::AsyncWrite + Unpin>,
    output_format: &OutputFormat,
    metadata: &Metadata,
) -> CarbideCliResult<()> {
    match output_format {
        OutputFormat::AsciiTable => {
            async_writeln!(output_file, "Name        : {}", metadata.name)?;
            async_writeln!(output_file, "Description : {}", metadata.description)?;
            let mut table = Table::new();
            table.set_titles(Row::from(vec!["Key", "Value"]));
            for l in &metadata.labels {
                table.add_row(Row::from(vec![&l.key, l.value.as_deref().unwrap_or("")]));
            }
            async_write!(output_file, "{}", table)?;
        }
        OutputFormat::Csv => {
            return Err(CarbideCliError::NotImplemented(
                "CSV formatted output".to_string(),
            ));
        }
        OutputFormat::Json => {
            async_writeln!(output_file, "{}", serde_json::to_string_pretty(&metadata)?)?
        }
        OutputFormat::Yaml => {
            return Err(CarbideCliError::NotImplemented(
                "YAML formatted output".to_string(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn write_metadata_in_nice_format(
    output: &mut String,
    width: usize,
    metadata: Option<&Metadata>,
) -> std::fmt::Result {
    if let Some(metadata) = metadata {
        writeln!(output, "METADATA: ")?;
        writeln!(output, "\tNAME: {}", metadata.name)?;
        writeln!(output, "\tDESCRIPTION: {}", metadata.description)?;
        writeln!(output, "\tLABELS:")?;
        for label in metadata.labels.iter() {
            writeln!(
                output,
                "\t\t{}:{}",
                label.key,
                label.value.as_deref().unwrap_or_default()
            )?;
        }
    } else {
        writeln!(output, "{:<width$}: None", "METADATA")?;
    }

    Ok(())
}

/// Shared metadata mutation helpers for the `metadata` subcommands.
///
/// These are used by machine, rack, switch, and power shelf to
/// implement `metadata set`, `metadata add-label`, and
/// `metadata remove-labels`. Each caller fetches its entity,
/// passes `entity.metadata` to one of these, then sends the
/// result to the entity's `update_*_metadata` RPC.
///
/// Things were so boilerplate it was either do a series of
/// macros, or make some helper functions, and since most of
/// the macro-able stuff was related to metadata mutation,
/// these were created.
///
/// Apply a name and/or description update to an entity's metadata.
/// Fields that are `None` are left unchanged.
pub(crate) fn apply_set(
    metadata: Option<Metadata>,
    name: Option<String>,
    description: Option<String>,
) -> CarbideCliResult<Metadata> {
    let mut metadata = metadata.ok_or_else(|| {
        CarbideCliError::GenericError("Entity does not carry Metadata that can be patched".into())
    })?;
    if let Some(name) = name {
        metadata.name = name;
    }
    if let Some(description) = description {
        metadata.description = description;
    }
    Ok(metadata)
}

/// Add a label to an entity's metadata. If a label
/// with the same key already exists, it is replaced
/// with the new value.
pub(crate) fn apply_add_label(
    metadata: Option<Metadata>,
    key: String,
    value: Option<String>,
) -> CarbideCliResult<Metadata> {
    let mut metadata = metadata.ok_or_else(|| {
        CarbideCliError::GenericError("Entity does not carry Metadata that can be patched".into())
    })?;
    metadata.labels.retain_mut(|l| l.key != key);
    metadata.labels.push(rpc::forge::Label { key, value });
    Ok(metadata)
}

/// Remove one or more labels from an entity's
/// metadata by key. Keys that don't exist are
/// silently ignored.
pub(crate) fn apply_remove_labels(
    metadata: Option<Metadata>,
    keys: Vec<String>,
) -> CarbideCliResult<Metadata> {
    let mut metadata = metadata.ok_or_else(|| {
        CarbideCliError::GenericError("Entity does not carry Metadata that can be patched".into())
    })?;
    let removed: HashSet<String> = keys.into_iter().collect();
    metadata.labels.retain(|l| !removed.contains(&l.key));
    Ok(metadata)
}

/// Format an entity's labels as quoted `"key:value"`
/// strings for display in list views (e.g. the machine
/// list table). Returns an empty vec if metadata is
/// None or has no labels.
pub(crate) fn fmt_labels_as_kv_pairs(metadata: Option<&Metadata>) -> Vec<String> {
    metadata
        .map(|m| {
            m.labels
                .iter()
                .map(|label| {
                    let key = &label.key;
                    let value = label.value.as_deref().unwrap_or_default();
                    format!("\"{key}:{value}\"")
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Parse user-provided label strings (in `key:value`
/// format) into RPC Label structs. A label without
/// a `:` separator is treated as a key-only label
/// with no value.
pub(crate) fn parse_rpc_labels(labels: Vec<String>) -> Vec<rpc::forge::Label> {
    labels
        .into_iter()
        .map(|label| match label.split_once(':') {
            Some((k, v)) => rpc::forge::Label {
                key: k.trim().to_string(),
                value: Some(v.trim().to_string()),
            },
            None => rpc::forge::Label {
                key: if label.contains(char::is_whitespace) {
                    label.trim().to_string()
                } else {
                    // avoid allocations on the happy path
                    label
                },
                value: None,
            },
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;

    use super::*;

    fn label(key: &str, value: Option<&str>) -> rpc::forge::Label {
        rpc::forge::Label {
            key: key.to_string(),
            value: value.map(str::to_string),
        }
    }

    fn metadata_with(name: &str, desc: &str, labels: Vec<rpc::forge::Label>) -> Metadata {
        Metadata {
            name: name.to_string(),
            description: desc.to_string(),
            labels,
        }
    }

    // apply_set: name/description overrides are applied (None leaves the field
    // untouched), and a missing Metadata is rejected. Each row yields the
    // resulting (name, description) pair.
    #[test]
    fn apply_set_applies_overrides() {
        scenarios!(
            run = |(metadata, name, description)| {
                apply_set(metadata, name, description)
                    .map(|m| (m.name, m.description))
                    .map_err(drop)
            };
            "name override updates name, leaves description" {
                (
                    Some(metadata_with("old", "desc", vec![])),
                    Some("new".to_string()),
                    None,
                ) => Yields(("new".to_string(), "desc".to_string())),
            }

            "description override updates description, leaves name" {
                (
                    Some(metadata_with("name", "old", vec![])),
                    None,
                    Some("new".to_string()),
                ) => Yields(("name".to_string(), "new".to_string())),
            }

            "no overrides leaves both unchanged" {
                (Some(metadata_with("name", "desc", vec![])), None, None) => Yields(("name".to_string(), "desc".to_string())),
            }

            "missing metadata errors" {
                (None, Some("x".to_string()), None) => Fails,
            }
        );
    }

    // apply_add_label: a new key is appended, an existing key is replaced in
    // place (and other labels preserved), and a missing Metadata is rejected.
    // Each row yields the resulting label list.
    #[test]
    fn apply_add_label_adds_or_replaces() {
        scenarios!(
            run = |(metadata, key, value)| {
                apply_add_label(metadata, key, value)
                    .map(|m| m.labels)
                    .map_err(drop)
            };
            "adds a new label" {
                (
                    Some(metadata_with("n", "", vec![])),
                    "env".to_string(),
                    Some("prod".to_string()),
                ) => Yields(vec![label("env", Some("prod"))]),
            }

            "replaces an existing key" {
                (
                    Some(metadata_with("n", "", vec![label("env", Some("staging"))])),
                    "env".to_string(),
                    Some("prod".to_string()),
                ) => Yields(vec![label("env", Some("prod"))]),
            }

            "preserves other labels" {
                (
                    Some(metadata_with("n", "", vec![label("team", Some("infra"))])),
                    "env".to_string(),
                    Some("prod".to_string()),
                ) => Yields(vec![
                    label("team", Some("infra")),
                    label("env", Some("prod")),
                ]),
            }

            "missing metadata errors" {
                (None, "k".to_string(), None) => Fails,
            }
        );
    }

    // apply_remove_labels: matching keys are dropped, missing keys are ignored,
    // and a missing Metadata is rejected. Each row yields the surviving labels.
    #[test]
    fn apply_remove_labels_drops_matching() {
        scenarios!(
            run = |(metadata, keys)| {
                apply_remove_labels(metadata, keys)
                    .map(|m| m.labels)
                    .map_err(drop)
            };
            "removes the matching key" {
                (
                    Some(metadata_with(
                        "n",
                        "",
                        vec![label("a", None), label("b", None)],
                    )),
                    vec!["a".to_string()],
                ) => Yields(vec![label("b", None)]),
            }

            "ignores keys that don't exist" {
                (
                    Some(metadata_with("n", "", vec![label("a", None)])),
                    vec!["nonexistent".to_string()],
                ) => Yields(vec![label("a", None)]),
            }

            "missing metadata errors" {
                (None, vec!["k".to_string()]) => Fails,
            }
        );
    }
}
