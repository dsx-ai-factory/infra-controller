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

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::{env, fs};

use anyhow::{Context, Result, bail};
use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition;

fn main() -> Result<()> {
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").context("CARGO_MANIFEST_DIR is not set")?);
    let crd_dir = manifest_dir.join("crds");
    if !crd_dir.is_dir() {
        bail!("CRD directory does not exist: {}", crd_dir.display());
    }
    // Watch the directory so added/removed/renamed CRD files retrigger generation.
    println!("cargo:rerun-if-changed={}", crd_dir.display());

    let out_dir = PathBuf::from(env::var("OUT_DIR").context("OUT_DIR is not set")?).join("crds");
    fs::create_dir_all(&out_dir).context("failed to create output CRD directory")?;

    let crd_paths = discover_yaml_files(&crd_dir)?;
    if crd_paths.is_empty() {
        bail!("no CRD YAML files found in {}", crd_dir.display());
    }

    let generator = kopium::TypeGenerator::builder()
        .derive(
            "@enum=strum_macros::Display"
                .parse()
                .context("invalid derive rule")?,
        )
        .derive(
            "@enum=strum_macros::IntoStaticStr"
                .parse()
                .context("invalid derive rule")?,
        )
        .build();
    let mut generated_modules = BTreeSet::new();

    for crd_path in crd_paths {
        // Watch each file so in-place content edits also retrigger generation.
        println!("cargo:rerun-if-changed={}", crd_path.display());

        let content = fs::read_to_string(&crd_path)
            .with_context(|| format!("failed to read {}", crd_path.display()))?;
        let crd: CustomResourceDefinition = serde_yaml::from_str(&content)
            .with_context(|| format!("failed to parse CRD YAML {}", crd_path.display()))?;

        let module_name = format!("{}_generated", crd.spec.names.plural.to_lowercase());
        if !generated_modules.insert(module_name.clone()) {
            bail!(
                "duplicate generated module name '{}' while processing {}",
                module_name,
                crd_path.display()
            );
        }

        let generated = generator
            .generate_rust_types_for(&crd, Some(format!("-f {}", crd_path.display())))
            .with_context(|| format!("failed to generate rust types for {}", crd_path.display()))?;

        let output_path = out_dir.join(format!("{module_name}.rs"));
        fs::write(&output_path, generated)
            .with_context(|| format!("failed to write {}", output_path.display()))?;
    }

    write_module_index(&out_dir, &generated_modules)?;

    Ok(())
}

fn discover_yaml_files(crd_dir: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in
        fs::read_dir(crd_dir).with_context(|| format!("failed to read {}", crd_dir.display()))?
    {
        let entry =
            entry.with_context(|| format!("failed to read entry in {}", crd_dir.display()))?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("yaml") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

fn write_module_index(out_dir: &Path, generated_modules: &BTreeSet<String>) -> Result<()> {
    let mut module_file = String::new();
    for module_name in generated_modules {
        writeln!(
            &mut module_file,
            "pub mod {module_name} {{ include!(concat!(env!(\"OUT_DIR\"), \"/crds/{module_name}.rs\")); }}"
        )
        .context("failed to render generated module index")?;
    }

    let mod_path = out_dir.join("mod.rs");
    fs::write(&mod_path, module_file)
        .with_context(|| format!("failed to write {}", mod_path.display()))?;
    Ok(())
}
