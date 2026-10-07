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
use std::borrow::Cow;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

use eyre::{Context, ContextCompat};
use toml_edit::{DocumentMut, Formatted, InlineTable, Item, Value};
use version_compare::Version;
use walkdir::WalkDir;

static REPO_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../.."); // crates/xtask/../..

pub(super) fn check(fix: bool) -> eyre::Result<CheckOutcome> {
    let repo_root = PathBuf::from(REPO_ROOT).canonicalize()?;
    let mut workspace = Workspace::load(repo_root).context("error reading cargo.toml files")?;

    workspace.move_deps_to_workspace()?;
    let diffs = workspace.diffs()?;
    if diffs.is_empty() {
        Ok(CheckOutcome::Success { fixed: vec![] })
    } else if fix {
        workspace.write_all()?;
        Ok(CheckOutcome::Success { fixed: diffs })
    } else {
        Ok(CheckOutcome::Failure { diffs })
    }
}

pub(super) enum CheckOutcome {
    Success { fixed: Vec<(PathBuf, String)> },
    Failure { diffs: Vec<(PathBuf, String)> },
}

impl CheckOutcome {
    pub(super) fn report_and_exit(self) -> ! {
        match self {
            CheckOutcome::Success { fixed } if fixed.is_empty() => {
                // All good, no fixes needed
            }
            CheckOutcome::Success { fixed } => {
                // Report what we fixed
                for (path, diff) in fixed {
                    println!("Fixed {} by applying change: \n{}", path.display(), diff);
                }
            }
            CheckOutcome::Failure { diffs } => {
                // Report what needs fixing
                for (path, diff) in diffs {
                    println!("Cargo.toml at {} needs fixes: \n{}", path.display(), diff);
                }
                std::process::exit(1);
            }
        }

        std::process::exit(0);
    }
}

struct Workspace {
    path: PathBuf,
    workspace_cargo_toml: DocumentMut,
    non_workspace_cargo_tomls: HashMap<PathBuf, DocumentMut>,
}

impl Workspace {
    fn load(path: PathBuf) -> eyre::Result<Self> {
        let workspace_cargo_toml_path = path.join("Cargo.toml");
        let workspace_cargo_toml_string = std::fs::read_to_string(&workspace_cargo_toml_path)?;

        // The toplevel workspace cargo.toml
        let workspace_cargo_toml = workspace_cargo_toml_string.parse::<DocumentMut>()?;

        // Find any Cargo.toml file throughout the workspace, excluding the toplevel one
        let non_workspace_cargo_tomls = WalkDir::new(path.join("crates"))
            .into_iter()
            .filter_entry(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|name| !name.starts_with("."))
            })
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|name| name.eq("Cargo.toml"))
            })
            .filter_map(|e| e.path().canonicalize().ok())
            // Skip workspace Cargo.toml
            .filter(|p| p != &workspace_cargo_toml_path)
            .map(|p| {
                let toml = std::fs::read_to_string(&p)?.parse()?;
                Ok((p, toml))
            })
            .collect::<Result<HashMap<_, _>, eyre::Error>>()?;

        Ok(Self {
            path,
            workspace_cargo_toml,
            non_workspace_cargo_tomls,
        })
    }

    /// For all Cargo.toml files in the repository, take any dependencies and move their version
    /// specification to the toplevel (workspace) Cargo.toml, and change them to `{ workspace = true }`.
    /// This is so we don't have multiple versions of the same dependency throughout the workspace.
    fn move_deps_to_workspace(&mut self) -> eyre::Result<()> {
        for (toml_path, document) in &mut self.non_workspace_cargo_tomls {
            let contents = fs::read_to_string(toml_path)?;
            for deptype in ["dependencies", "dev-dependencies"] {
                let Some(deps) = document.get_mut(deptype).and_then(|v| v.as_table_mut()) else {
                    continue;
                };

                for (dep_name, dep) in deps.iter_mut() {
                    let dep_line_prefix = format!("{dep_name} = ");
                    if contents.lines().any(|l| {
                        l.starts_with(&dep_line_prefix) && l.contains("check-workspace-deps:ignore")
                    }) {
                        println!("Ignoring dep {} in {}", dep_name, toml_path.display());
                        continue;
                    }

                    if let Some(dep) = dep.as_inline_table_mut() {
                        // It's a line like dep = { version = "1", ... }

                        // Does it say `default-features = false`? If so, that needs to be moved to
                        // the workspace root (crate-level toml can't set default-features=false if
                        // the workspace-level does not.)
                        let clear_default_features = dep
                            .get("default-features")
                            .as_ref()
                            .map(|v| v.as_bool().is_some_and(|b| !b))
                            .unwrap_or(false);

                        // Does it specify a custom registry? If so, that needs to migrate to the
                        // workspace root.
                        let registry = dep
                            .get("registry")
                            .as_ref()
                            .and_then(|v| v.as_str())
                            .map(ToString::to_string);

                        let version = match dep
                            .get_mut("version")
                            .and_then(|v| v.as_str().and_then(Version::from))
                        {
                            Some(version) => version,
                            None => continue,
                        };

                        specify_version(
                            &mut self.workspace_cargo_toml,
                            dep_name.get(),
                            version,
                            clear_default_features,
                            registry.as_deref(),
                        )
                        .with_context(|| {
                            format!(
                                "error specifying version for {} in toplevel toml",
                                dep_name.get()
                            )
                        })?;

                        dep.remove("version");
                        dep.remove("registry");
                        dep.insert("workspace", Value::Boolean(Formatted::new(true)));
                    } else if let Some(version) = dep.as_str() {
                        let version = Version::from(version).with_context(|| {
                            format!(
                                "error parsing version for dependency `{}` in {}",
                                dep_name,
                                toml_path.display()
                            )
                        })?;
                        // It's a simple `dep = "version"` line, so no registry and no default_features=false are possible
                        specify_version(
                            &mut self.workspace_cargo_toml,
                            dep_name.get(),
                            version,
                            false,
                            None,
                        )
                        .with_context(|| {
                            format!(
                                "error specifying version for {} in toplevel toml",
                                dep_name.get()
                            )
                        })?;

                        let mut table = InlineTable::new();
                        table.insert("workspace", Value::Boolean(Formatted::new(true)));
                        *dep = Item::Value(Value::InlineTable(table));
                    };
                }
            }
        }

        Ok(())
    }

    fn diffs(&self) -> eyre::Result<Vec<(PathBuf, String)>> {
        Ok(self
            .all_tomls()
            .filter_map(
                |(path, toml)| match diff_against_file(&path, toml.to_string().as_str()) {
                    Ok(Some(diff)) => Some(Ok((path.to_path_buf(), diff))),
                    Ok(None) => None,
                    Err(e) => Some(Err(e)),
                },
            )
            .collect::<Result<Vec<_>, _>>()?)
    }

    fn write_all(&self) -> eyre::Result<()> {
        for (path, document) in &self.non_workspace_cargo_tomls {
            std::fs::write(path, document.to_string())
                .with_context(|| format!("error writing to {}", path.display()))?;
        }

        let workspace_cargo_toml_path = self.toplevel_cargo_toml_path();
        std::fs::write(
            workspace_cargo_toml_path,
            self.workspace_cargo_toml.to_string(),
        )?;
        Ok(())
    }

    fn all_tomls(&self) -> impl Iterator<Item = (Cow<'_, Path>, &DocumentMut)> {
        self.non_workspace_cargo_tomls
            .iter()
            .map(|(path, document)| (Cow::Borrowed(path.as_path()), document))
            .chain(Some((
                Cow::Owned(self.toplevel_cargo_toml_path()),
                &self.workspace_cargo_toml,
            )))
    }

    fn toplevel_cargo_toml_path(&self) -> PathBuf {
        self.path.join("Cargo.toml")
    }
}

/// Edits a workspace cargo toml to specify a particular version
fn specify_version(
    workspace_cargo_toml: &mut DocumentMut,
    dep_name: &str,
    new_version: Version,
    clear_default_features: bool,
    registry: Option<&str>,
) -> eyre::Result<()> {
    let Some(deps) = workspace_cargo_toml["workspace"]["dependencies"].as_table_mut() else {
        return Err(eyre::eyre!(
            "no dependencies section in toplevel cargo.toml"
        ));
    };

    let dep = deps
        .entry(dep_name)
        .or_insert_with(|| Item::Value(new_version.as_str().into()));

    let maybe_table = if let Some(table) = dep.as_inline_table() {
        // It's an inline table, e.g. `dep = { version = "..." }`. So we'll modify the version string, and add any fields we want.
        Some(table.clone())
    } else if dep.is_str() && (clear_default_features || registry.is_some()) {
        // It's a simple declaration, e.g. `dep = "1"`, but we have fields we need to add to it, so convert it to a table.
        Some(InlineTable::new())
    } else {
        None
    };

    if let Some(mut table) = maybe_table {
        // The dep is (now) a table, set the fields appropriately.
        if table
            .get("version")
            .and_then(|v| v.as_str())
            .and_then(Version::from)
            .is_none_or(|current| new_version > current)
        {
            // Define the version only if it's newer
            table.insert("version", new_version.as_str().into());
        }

        if clear_default_features {
            // Set `default-features = false`
            table.insert("default-features", Value::Boolean(Formatted::new(false)));
        }

        if let Some(registry) = registry {
            // Set `registry = "..."`
            table.insert("registry", registry.into());
        }

        *dep = Item::Value(Value::InlineTable(table));
    } else if let Some(existing_version) = dep.as_str() {
        // It's now a simple declaration, e.g. `dep = "1.0"`. Per above, we must not have needed to
        // set any other fields, so just change the version string if needed.
        if Version::from(existing_version)
            .is_none_or(|existing_version| new_version > existing_version)
        {
            *dep = Item::Value(new_version.as_str().into());
        }
    } else {
        eprintln!("Invalid dependency in toplevel Cargo.toml: {dep_name}");
    }

    Ok(())
}

/// Compare `desired` (in-memory) to the contents of `path` (on-disk).
/// - Ok(None) -> identical
/// - Ok(Some) -> a unified diff string ready to print
fn diff_against_file(path: impl AsRef<Path>, desired: &str) -> io::Result<Option<String>> {
    let path = path.as_ref();
    let on_disk = fs::read_to_string(path)?;

    if on_disk == desired {
        return Ok(None);
    }

    let path_str = path.display().to_string();
    let diff = similar::TextDiff::from_lines(on_disk.as_str(), desired)
        .unified_diff()
        .context_radius(3)
        .header(&path_str, &path_str)
        .to_string();

    Ok(Some(diff))
}
