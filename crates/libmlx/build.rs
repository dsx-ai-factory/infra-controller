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

// build.rs
// Registry build script. This takes the YAML registry schema files
// from the databases/ directory, and uses them to generate the
// OUT_DIR/registries.rs file, which will now contain ALL compile-time
// embedded registries for use by any components which need to
// work with mlxconfig variable management and assignment.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use serde::Deserialize;

// Local deserialization types that mirror the YAML schema.
// These are self-contained so build.rs doesn't need to depend
// on the crate's own types (which aren't compiled yet).

#[derive(Deserialize)]
struct Registry {
    name: String,
    variables: Vec<Variable>,
    #[serde(default)]
    filters: Option<FilterSet>,
}

#[derive(Deserialize)]
struct Variable {
    name: String,
    description: String,
    read_only: bool,
    spec: Spec,
}

#[derive(Deserialize)]
#[serde(tag = "type", content = "config", rename_all = "snake_case")]
enum Spec {
    Boolean,
    Integer,
    String,
    Binary,
    Bytes,
    Array,
    Enum {
        options: Vec<std::string::String>,
    },
    Preset {
        max_preset: u8,
    },
    BooleanArray {
        size: usize,
    },
    IntegerArray {
        size: usize,
    },
    EnumArray {
        options: Vec<std::string::String>,
        size: usize,
    },
    BinaryArray {
        size: usize,
    },
    Opaque,
}

#[derive(Deserialize)]
#[serde(transparent)]
struct FilterSet {
    filters: Vec<Filter>,
}

#[derive(Deserialize)]
struct Filter {
    field: Field,
    values: Vec<std::string::String>,
    #[serde(default)]
    match_mode: MatchModeLocal,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum MatchModeLocal {
    #[default]
    Regex,
    Exact,
    Prefix,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Field {
    DeviceType,
    PartNumber,
    FirmwareVersion,
    MacAddress,
    Description,
    PciName,
    Status,
}

static DEBUG_BUILD_SCRIPT: LazyLock<bool> = LazyLock::new(|| {
    // Note that this variable is only evaluated when the build script is run,
    // but the build script output (which this controls) is persisted for later
    // `cargo` commands.
    matches!(std::env::var("DEBUG_MLXCONFIG"), Ok(value) if value != "0")
});

fn main() {
    println!("cargo:rerun-if-changed=databases/");

    let registries = load_registries();
    let generated_code = generate_registries_code(&registries);
    write_generated_code(&generated_code);

    if *DEBUG_BUILD_SCRIPT {
        println!(
            "cargo:warning=Generated {} registries with {} total variables",
            registries.len(),
            registries.iter().map(|r| r.variables.len()).sum::<usize>()
        );
    }
}

// load_registries loads and validates all registry
// YAML files from the databases/ directory.
fn load_registries() -> Vec<Registry> {
    let databases_dir = Path::new("databases");
    if !databases_dir.exists() {
        panic!("databases/ directory not found!");
    }

    let mut registries = Vec::new();

    // Process all .yaml files in databases/
    for entry in fs::read_dir(databases_dir).expect("Failed to read databases directory") {
        let entry = entry.expect("Failed to read directory entry");
        let path = entry.path();

        if path.extension().and_then(|s| s.to_str()) != Some("yaml") {
            continue;
        }

        let file_name = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("Invalid filename");

        println!("cargo:rerun-if-changed={}", path.display());

        let registry = load_registry_file(&path, file_name);
        registries.push(registry);
    }

    if registries.is_empty() {
        panic!("No YAML files found in databases/ directory!");
    }

    // Sort by registry name for consistent output.
    registries.sort_by(|a, b| a.name.cmp(&b.name));
    registries
}

// load_registry_file loads and validates a single registry
// file discovered in the databases/ directory.
fn load_registry_file(path: &Path, file_name: &str) -> Registry {
    let yaml_content = fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("Failed to read {}: {}", path.display(), e));

    let registry: Registry = serde_yaml::from_str(&yaml_content)
        .unwrap_or_else(|e| panic!("Failed to parse {} as Registry: {}", path.display(), e));

    if *DEBUG_BUILD_SCRIPT {
        println!(
            "cargo:warning=[INFO] Parsed registry '{}' with {} variables from {}",
            registry.name,
            registry.variables.len(),
            file_name
        );

        // Show filter info during build.
        if registry
            .filters
            .as_ref()
            .is_some_and(|f| !f.filters.is_empty())
        {
            let filter_count = registry.filters.as_ref().map_or(0, |f| f.filters.len());
            println!(
                "cargo:warning=[INFO]   Filters: {} filter(s) configured",
                filter_count
            );
        } else {
            println!("cargo:warning=[INFO]   No device filters configured for this registry");
        }
    }

    registry
}

// generate_registries_code generates the complete registries.rs
// Rust code file for all registries parsed from the databases/
// directory.
fn generate_registries_code(registries: &[Registry]) -> String {
    let mut code = String::new();

    // First, dump out the header for registries.rs.
    code.push_str("// This file was auto-generated by build.rs\n");
    code.push_str("use once_cell::sync::Lazy;\n\n");
    code.push_str(
        "pub static REGISTRIES: Lazy<Vec<crate::variables::registry::MlxVariableRegistry>> = Lazy::new(\n    || {\n        vec![\n",
    );

    // Now generate the code for each registry.
    for registry in registries {
        code.push_str(&generate_registry_code(registry));
    }

    // And finally, dump out the accessor functions
    // for working with the defined registries.
    code.push_str("]\n    },\n);\n\n");
    code.push_str(&generate_accessor_functions());
    code
}

// generate_registry_code generates code for a
// single registry to put into registries.rs,
// taking whitespace and such into consideration
// so it looks pretty when you actually read the
// code itself.
fn generate_registry_code(registry: &Registry) -> String {
    let mut code = String::new();

    code.push_str(&format!(
        "        crate::variables::registry::MlxVariableRegistry::new({:?})\n",
        registry.name
    ));

    // Generate filters if they exist.
    if let Some(ref filters) = registry.filters
        && !filters.filters.is_empty()
    {
        code.push_str(&generate_filters_code(filters));
    }

    code.push_str("            .variables(vec![\n");

    // And now generate all variables for the registry.
    for variable in &registry.variables {
        code.push_str(&generate_variable_code(variable));
        code.push('\n');
    }

    code.push_str("            ]),\n");
    code
}

// generate_filters_code generates code for
// any configured registry device filters.
fn generate_filters_code(filter_set: &FilterSet) -> String {
    let mut code = String::new();

    code.push_str("            .with_filters(\n");
    code.push_str("                crate::device::filters::DeviceFilterSet::new()");

    // Generate code for each filter in the set.
    for filter in &filter_set.filters {
        code.push_str("\n                    .with_filter(");
        code.push_str(&generate_single_filter_code(filter));
        code.push(')');
    }

    code.push_str("\n            )\n");
    code
}

// generate_single_filter_code generates code for a single device filter.
fn generate_single_filter_code(filter: &Filter) -> String {
    let field_code = match filter.field {
        Field::DeviceType => "crate::device::filters::DeviceField::DeviceType",
        Field::PartNumber => "crate::device::filters::DeviceField::PartNumber",
        Field::FirmwareVersion => "crate::device::filters::DeviceField::FirmwareVersion",
        Field::MacAddress => "crate::device::filters::DeviceField::MacAddress",
        Field::Description => "crate::device::filters::DeviceField::Description",
        Field::PciName => "crate::device::filters::DeviceField::PciName",
        Field::Status => "crate::device::filters::DeviceField::Status",
    };

    let values_code = format!(
        "vec![{}]",
        filter
            .values
            .iter()
            .map(|v| format!("{v:?}.to_string()"))
            .collect::<Vec<_>>()
            .join(", ")
    );

    let match_mode_code = match filter.match_mode {
        MatchModeLocal::Regex => "crate::device::filters::MatchMode::Regex",
        MatchModeLocal::Exact => "crate::device::filters::MatchMode::Exact",
        MatchModeLocal::Prefix => "crate::device::filters::MatchMode::Prefix",
    };

    format!(
        "crate::device::filters::DeviceFilter {{ field: {field_code}, values: {values_code}, match_mode: {match_mode_code} }}",
    )
}

// generate_variable_code generates code for a single
// variable in the registry, with MlxVariableSpec code
// generation coming at the tail end.
fn generate_variable_code(variable: &Variable) -> String {
    format!(
        r#"                crate::variables::variable::MlxConfigVariable::builder()
                    .name({:?}.to_string())
                    .description({:?}.to_string())
                    .read_only({})
                    .spec({})
                    .build(),"#,
        variable.name,
        variable.description,
        variable.read_only,
        generate_spec_code(&variable.spec)
    )
}

// generate_accessor_functions generates the accessor functions
// for actually working with the REGISTRIES constant.
fn generate_accessor_functions() -> String {
    r#"/// get_all returns all hardware configuration registries.
pub fn get_all() -> &'static [crate::variables::registry::MlxVariableRegistry] {
    &REGISTRIES
}

/// get will return a registry by name.
pub fn get(name: &str) -> Option<&'static crate::variables::registry::MlxVariableRegistry> {
    REGISTRIES.iter().find(|r| r.name == name)
}

/// list will return a list of all registry names.
pub fn list() -> Vec<&'static str> {
    REGISTRIES.iter().map(|r| r.name.as_str()).collect()
}

/// get_registries_for_device returns all registries that match the given device.
/// If a registry has no filters configured, it matches all devices.
pub fn get_registries_for_device(
    device_info: &carbide_libmlx_model::device::info::MlxDeviceInfo,
) -> Vec<&'static crate::variables::registry::MlxVariableRegistry> {
    REGISTRIES
        .iter()
        .filter(|r| r.matches_device(device_info))
        .collect()
}
"#
    .to_string()
}

// write_generated_code writes the generated code to the
// output registries.rs file.
fn write_generated_code(code: &str) {
    // Generated sources belong in Cargo's output directory. Keeping the source
    // tree immutable is required by Bazel sandboxes and also prevents parallel
    // Cargo builds from racing over a checked-in file.
    let dest_path = Path::new(
        &std::env::var("OUT_DIR").expect("Cargo always defines OUT_DIR for build scripts"),
    )
    .join("registries.rs");

    if let Ok(existing) = fs::read_to_string(&dest_path)
        && existing == code
    {
        // Avoid rewriting it if it hasn't changed, so that we don't bump the timestamp and cause rebuilds
        return;
    }
    fs::write(dest_path, code).expect("Failed to write generated code");
}

// generate_spec_code generates the code to define the spec for a given variable.
fn generate_spec_code(spec: &Spec) -> String {
    match spec {
        Spec::Boolean => {
            "crate::variables::spec::MlxVariableSpec::builder().boolean().build()".to_string()
        }
        Spec::Integer => {
            "crate::variables::spec::MlxVariableSpec::builder().integer().build()".to_string()
        }
        Spec::String => {
            "crate::variables::spec::MlxVariableSpec::builder().string().build()".to_string()
        }
        Spec::Binary => {
            "crate::variables::spec::MlxVariableSpec::builder().binary().build()".to_string()
        }
        Spec::Bytes => {
            "crate::variables::spec::MlxVariableSpec::builder().bytes().build()".to_string()
        }
        Spec::Array => {
            "crate::variables::spec::MlxVariableSpec::builder().array().build()".to_string()
        }
        Spec::Enum { options } => {
            format!(
                "crate::variables::spec::MlxVariableSpec::builder().enum_type().with_options(vec![{}]).build()",
                options
                    .iter()
                    .map(|opt| format!("{opt:?}.to_string()"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Spec::Preset { max_preset } => {
            format!(
                "crate::variables::spec::MlxVariableSpec::builder().preset().with_max_preset({max_preset}).build()",
            )
        }
        Spec::BooleanArray { size } => {
            format!(
                "crate::variables::spec::MlxVariableSpec::builder().boolean_array().with_size({size}).build()",
            )
        }
        Spec::IntegerArray { size } => {
            format!(
                "crate::variables::spec::MlxVariableSpec::builder().integer_array().with_size({size}).build()",
            )
        }
        Spec::EnumArray { options, size } => {
            format!(
                "crate::variables::spec::MlxVariableSpec::builder().enum_array().with_options(vec![{}]).with_size({size}).build()",
                options
                    .iter()
                    .map(|opt| format!("{opt:?}.to_string()"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        }
        Spec::BinaryArray { size } => {
            format!(
                "crate::variables::spec::MlxVariableSpec::builder().binary_array().with_size({size}).build()",
            )
        }
        Spec::Opaque => {
            "crate::variables::spec::MlxVariableSpec::builder().opaque().build()".to_string()
        }
    }
}
