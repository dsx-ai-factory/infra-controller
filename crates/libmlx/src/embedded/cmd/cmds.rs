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

use std::fs;
use std::time::Duration;

use carbide_libmlx_model::device::info::MlxDeviceInfo;
use prettytable::{Cell, Row, Table};
use regex::Regex;
use serde_json;
use tracing;

use crate::device::cmd::device::args::DeviceArgs;
use crate::device::cmd::device::cmds::handle as handle_device;
use crate::embedded::cmd::args::{
    Cli, Commands, FirmwareAction, OutputFormat, ProfileCommands, RegistryAction, RunnerCommands,
};
use crate::firmware::config::{FirmwareFlasherProfile, FirmwareSpec, FlashSpec};
use crate::firmware::credentials::Credentials;
use crate::firmware::flasher::FirmwareFlasher;
use crate::lockdown::cmd::cmds::handle_lockdown;
use crate::profile::profile::MlxConfigProfile;
use crate::registry::registries;
use crate::runner::error::MlxRunnerError;
use crate::runner::exec_options::ExecOptions;
use crate::runner::result_types::QueryResult;
use crate::runner::runner::MlxConfigRunner;
use crate::variables::registry::MlxVariableRegistry;
use crate::variables::spec::MlxVariableSpec;
use crate::variables::variable::MlxConfigVariable;

pub async fn run_cli(cli: Cli) -> Result<(), Box<dyn std::error::Error>> {
    match cli.command {
        Some(Commands::Version) => {
            cmd_version();
        }
        Some(Commands::Registry { action }) => match action {
            RegistryAction::Generate {
                input_file,
                out_file,
            } => {
                cmd_registry_generate(input_file, out_file)?;
            }
            RegistryAction::Validate { yaml_file } => {
                cmd_registry_validate(yaml_file)?;
            }
            RegistryAction::List => {
                cmd_registry_list();
            }
            RegistryAction::Show {
                registry_name,
                output,
            } => {
                cmd_registry_show(&registry_name, output)?;
            }
            RegistryAction::Check {
                registry_name,
                device_type,
                part_number,
                fw_version,
            } => {
                cmd_registry_check(&registry_name, device_type, part_number, fw_version)?;
            }
        },
        Some(Commands::Runner {
            device,
            verbose,
            dry_run,
            retries,
            timeout,
            confirm,
            runner_command,
        }) => {
            // Leverage our ExecOptions builder to build some arguments
            // using what we got from CLI arguments.
            let options = ExecOptions::default()
                .with_verbose(verbose)
                .with_dry_run(dry_run)
                .with_retries(retries)
                .with_timeout(Some(Duration::from_secs(timeout)))
                .with_confirm_destructive(confirm);
            run_runner_command(&device, runner_command, options)?;
        }
        Some(Commands::Profile {
            device,
            verbose,
            dry_run,
            retries,
            timeout,
            confirm,
            profile_command,
        }) => {
            let options = ExecOptions::default()
                .with_verbose(verbose)
                .with_dry_run(dry_run)
                .with_retries(retries)
                .with_timeout(Some(Duration::from_secs(timeout)))
                .with_confirm_destructive(confirm);
            run_profile_command(&device, profile_command, options)?;
        }
        Some(Commands::Device { action }) => {
            let device_args = DeviceArgs { action };
            handle_device(device_args)?;
        }
        Some(Commands::Lockdown { action }) => {
            handle_lockdown(action)?;
        }
        Some(Commands::Firmware {
            dry_run,
            work_dir,
            firmware_action,
        }) => {
            run_firmware_command(dry_run, work_dir, *firmware_action).await?;
        }
        None => {
            cmd_show_default_info();
        }
    }
    Ok(())
}

fn cmd_version() {
    println!("mlxconfig-embedded version 0.0.1");
    println!(
        "A reference example showcasing how to work with mlxconfig-runner, mlxconfig-registry, and mlxconfig-variables."
    );
}

fn cmd_registry_generate(
    input_file: std::path::PathBuf,
    out_file: Option<std::path::PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let content = fs::read_to_string(&input_file).map_err(|e| {
        format!(
            "Failed to read input file '{}': {}",
            input_file.display(),
            e
        )
    })?;

    let registry = parse_mlx_show_confs(&content);
    let yaml = registry_to_yaml(&registry);

    match out_file {
        Some(path) => {
            fs::write(&path, &yaml)
                .map_err(|e| format!("Failed to write output file '{}': {}", path.display(), e))?;
            println!("Generated registry YAML: {}", path.display());
        }
        None => {
            print!("{yaml}");
        }
    }

    Ok(())
}

fn cmd_registry_validate(yaml_file: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let content = fs::read_to_string(&yaml_file)
        .map_err(|e| format!("Failed to read YAML file '{}': {}", yaml_file.display(), e))?;

    match serde_yaml::from_str::<MlxVariableRegistry>(&content) {
        Ok(registry) => {
            println!("YAML file is valid!");
            println!("Registry: '{}'", registry.name);
            println!("Variables: {}", registry.variables.len());
        }
        Err(e) => {
            return Err(format!("Invalid YAML: {e}").into());
        }
    }

    Ok(())
}

fn cmd_registry_list() {
    let mut table = Table::new();

    // Set up table headers
    table.add_row(Row::new(vec![Cell::new("Name"), Cell::new("Variables")]));

    for registry in registries::get_all() {
        table.add_row(Row::new(vec![
            Cell::new(&registry.name),
            Cell::new(&registry.variables.len().to_string()),
        ]));
    }

    table.printstd();
}

fn cmd_registry_show(
    registry_name: &str,
    output_format: OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = registries::get(registry_name).ok_or_else(|| {
        format!(
            "Registry '{registry_name}' not found. Use 'registry list' to see available registries.",
        )
    })?;

    match output_format {
        OutputFormat::AsciiTable => show_registry_table(registry),
        OutputFormat::Json => show_registry_json(registry)?,
        OutputFormat::Yaml => show_registry_yaml(registry)?,
    }

    Ok(())
}

fn cmd_registry_check(
    registry_name: &str,
    device_type: Option<String>,
    part_number: Option<String>,
    fw_version_current: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = registries::get(registry_name).ok_or_else(|| {
        format!(
            "Registry '{registry_name}' not found. Use 'registry list' to see available registries.",
        )
    })?;
    let device_info = MlxDeviceInfo {
        pci_name: "00:00.0".to_string(),
        device_type: device_type.unwrap_or_else(|| "Unknown".to_string()),
        psid: Some("Unknown".to_string()),
        device_description: Some("Test device".to_string()),
        pxe_version_current: None,
        uefi_version_current: None,
        uefi_version_virtio_blk_current: None,
        uefi_version_virtio_net_current: None,
        base_mac: None,
        base_guid: None,
        status: None,
        part_number,
        fw_version_current,
    };
    if let Some(filter_set) = &registry.filters {
        if filter_set.matches(&device_info) {
            println!("Device info matches registry filters ({filter_set}!)");
        } else {
            println!("Device info doesn't match registry filters ({filter_set})!");
        }
    } else {
        println!("No filters configured on registry, device allowed.")
    }
    Ok(())
}

fn cmd_show_default_info() {
    println!("Try running with --help.");
}

pub fn parse_mlx_show_confs(content: &str) -> MlxVariableRegistry {
    let mut variables = Vec::new();

    let section_re = Regex::new(r"^\s*([A-Z][A-Z0-9 _]+):$").unwrap();
    let var_re = Regex::new(r"^\s+([A-Z][A-Z0-9_]+)=<([^>]+)>\s*(.*)$").unwrap();

    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i];

        // Skip header lines
        if line.starts_with("List of configurations") || line.trim().is_empty() {
            i += 1;
            continue;
        }

        // Check for section header (we track this for potential future use).
        if let Some(_caps) = section_re.captures(line) {
            i += 1;
            continue;
        }

        // Check for variable definition.
        if let Some(caps) = var_re.captures(line) {
            let var_name = caps[1].to_string();
            let var_type_str = &caps[2];
            let mut description = caps[3].to_string();

            // Look ahead for continuation lines (additional description).
            let mut j = i + 1;
            while j < lines.len() {
                let next_line = lines[j];
                // If next line starts with whitespace and doesn't match variable
                // pattern, it's continuation.
                if next_line.starts_with(' ')
                    && !var_re.is_match(lines[j])
                    && !section_re.is_match(lines[j])
                    && !next_line.trim().is_empty()
                {
                    description.push(' ');
                    description.push_str(next_line.trim());
                    j += 1;
                } else {
                    break;
                }
            }

            // Clean up description -- remove extra whitespace and truncate if too long.
            description = description.split_whitespace().collect::<Vec<_>>().join(" ");

            // More aggressive cleaning for problematic characters.
            description = clean_description(&description);

            if description.len() > 120 {
                description.truncate(117);
                description.push_str("...");
            }

            let spec = parse_variable_type(var_type_str);

            variables.push(MlxConfigVariable {
                name: var_name,
                description,
                // TODO(chet): Needs query integration.
                read_only: false,
                spec,
            });

            i = j;
        } else {
            i += 1;
        }
    }

    MlxVariableRegistry {
        // TODO(chet): Make it so you can provide a name.
        name: "generated_registry".to_string(),
        filters: None,
        variables,
    }
}

fn clean_description(desc: &str) -> String {
    desc.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn parse_variable_type(type_str: &str) -> MlxVariableSpec {
    if type_str == "False|True" {
        return MlxVariableSpec::Enum {
            options: vec!["False".to_string(), "True".to_string()],
        };
    }

    if type_str == "NUM" {
        return MlxVariableSpec::Integer;
    }

    if type_str == "BYTES" {
        return MlxVariableSpec::Bytes;
    }

    if type_str == "BINARY" {
        return MlxVariableSpec::Binary;
    }

    // Check if it contains pipe-separated options, which means
    // it's an enum.
    if type_str.contains('|') {
        let options: Vec<String> = type_str.split('|').map(|s| s.trim().to_string()).collect();
        return MlxVariableSpec::Enum { options };
    }

    // Default to String for anything else.
    MlxVariableSpec::String
}

pub fn registry_to_yaml(registry: &MlxVariableRegistry) -> String {
    let mut yaml = String::new();

    yaml.push_str(&format!(
        "name: \"{}\"\n",
        escape_yaml_string(&registry.name)
    ));

    yaml.push_str("variables:\n");

    for var in &registry.variables {
        yaml.push_str(&format!(
            "  - name: \"{}\"\n",
            escape_yaml_string(&var.name)
        ));
        yaml.push_str(&format!(
            "    description: \"{}\"\n",
            escape_yaml_string(&var.description)
        ));
        yaml.push_str(&format!("    read_only: {}\n", var.read_only));
        yaml.push_str("    spec:\n");

        match &var.spec {
            MlxVariableSpec::Boolean => {
                yaml.push_str("      type: \"Boolean\"\n");
            }
            MlxVariableSpec::Integer => {
                yaml.push_str("      type: \"Integer\"\n");
            }
            MlxVariableSpec::String => {
                yaml.push_str("      type: \"String\"\n");
            }
            MlxVariableSpec::Binary => {
                yaml.push_str("      type: \"Binary\"\n");
            }
            MlxVariableSpec::Bytes => {
                yaml.push_str("      type: \"Bytes\"\n");
            }
            MlxVariableSpec::Array => {
                yaml.push_str("      type: \"Array\"\n");
            }
            MlxVariableSpec::Enum { options } => {
                yaml.push_str("      type: \"Enum\"\n");
                yaml.push_str("      config:\n");
                yaml.push_str("        options: [");
                for (i, option) in options.iter().enumerate() {
                    if i > 0 {
                        yaml.push_str(", ");
                    }
                    yaml.push_str(&format!("\"{}\"", escape_yaml_string(option)));
                }
                yaml.push_str("]\n");
            }
            MlxVariableSpec::Preset { max_preset } => {
                yaml.push_str("      type: \"Preset\"\n");
                yaml.push_str("      config:\n");
                yaml.push_str(&format!("        max_preset: {max_preset}\n"));
            }
            MlxVariableSpec::BooleanArray { size } => {
                yaml.push_str("      type: \"BooleanArray\"\n");
                yaml.push_str("      config:\n");
                yaml.push_str(&format!("        size: {size}\n"));
            }
            MlxVariableSpec::IntegerArray { size } => {
                yaml.push_str("      type: \"IntegerArray\"\n");
                yaml.push_str("      config:\n");
                yaml.push_str(&format!("        size: {size}\n"));
            }
            MlxVariableSpec::EnumArray { options, size } => {
                yaml.push_str("      type: \"EnumArray\"\n");
                yaml.push_str("      config:\n");
                yaml.push_str("        options: [");
                for (i, option) in options.iter().enumerate() {
                    if i > 0 {
                        yaml.push_str(", ");
                    }
                    yaml.push_str(&format!("\"{}\"", escape_yaml_string(option)));
                }
                yaml.push_str("]\n");
                yaml.push_str(&format!("        size: {size}\n"));
            }
            MlxVariableSpec::BinaryArray { size } => {
                yaml.push_str("      type: \"BinaryArray\"\n");
                yaml.push_str("      config:\n");
                yaml.push_str(&format!("        size: {size}\n"));
            }
            MlxVariableSpec::Opaque => {
                yaml.push_str("      type: \"Opaque\"\n");
            }
        }
        yaml.push('\n');
    }

    yaml
}

fn escape_yaml_string(s: &str) -> String {
    // For YAML double-quoted strings, we only need to escape double quotes
    // and backslashes.
    s.replace("\\", "\\\\") // Escape backslashes first,
        .replace("\"", "\\\"") // and then escape double quotes.
}

fn format_spec(spec: &MlxVariableSpec) -> String {
    match spec {
        MlxVariableSpec::Boolean => "boolean".to_string(),
        MlxVariableSpec::Integer => "integer".to_string(),
        MlxVariableSpec::String => "string".to_string(),
        MlxVariableSpec::Binary => "binary".to_string(),
        MlxVariableSpec::Bytes => "bytes".to_string(),
        MlxVariableSpec::Array => "array".to_string(),
        MlxVariableSpec::Enum { options } => {
            format!("enum [{}]", options.join(", "))
        }
        MlxVariableSpec::Preset { max_preset } => {
            format!("preset (max: {max_preset})")
        }
        MlxVariableSpec::BooleanArray { size } => {
            format!("boolean_array[{size}]")
        }
        MlxVariableSpec::IntegerArray { size } => {
            format!("integer_array[{size}]")
        }
        MlxVariableSpec::EnumArray { options, size } => {
            format!("enum_array[{size}] [{}]", options.join(", "))
        }
        MlxVariableSpec::BinaryArray { size } => {
            format!("binary_array[{size}]")
        }
        MlxVariableSpec::Opaque => "opaque".to_string(),
    }
}

fn show_registry_table(registry: &MlxVariableRegistry) {
    let mut table = Table::new();

    // Registry name row (spans all 4 columns).
    table.add_row(Row::new(vec![
        Cell::new("Registry Name"),
        // And this one spans across 3 columns.
        Cell::new(&registry.name).with_hspan(3),
    ]));

    let device_filters = match &registry.filters {
        Some(filters) => format!("{filters}"),
        None => "None".to_string(),
    };

    table.add_row(Row::new(vec![
        Cell::new("Device Filters"),
        // And this one spans across 3 columns.
        Cell::new(&device_filters).with_hspan(3),
    ]));

    // Variables count row (spans all 4 columns).
    table.add_row(Row::new(vec![
        Cell::new("Variables"),
        Cell::new(&registry.variables.len().to_string()).with_hspan(3),
    ]));

    // Variables header row.
    table.add_row(Row::new(vec![
        Cell::new("Variable Name").style_spec("Fb"),
        Cell::new("Read-Write").style_spec("Fb"),
        Cell::new("Type").style_spec("Fb"),
        Cell::new("Description").style_spec("Fb"),
    ]));

    // ...and then add all variables.
    for variable in &registry.variables {
        let read_write = if variable.read_only { "RO" } else { "RW" };
        let var_type = format_spec(&variable.spec);

        // Wrap description at 60 characters. Could probably
        // make this configurable but since it's just a CLI
        // reference example I don't think it's really needed
        // for now.
        let wrapped_description = wrap_text(&variable.description, 60);

        table.add_row(Row::new(vec![
            Cell::new(&variable.name),
            Cell::new(read_write),
            Cell::new(&var_type),
            Cell::new(&wrapped_description),
        ]));
    }

    table.printstd();
}

fn show_registry_json(registry: &MlxVariableRegistry) -> Result<(), Box<dyn std::error::Error>> {
    let json = serde_json::to_string_pretty(registry)
        .map_err(|e| format!("Failed to serialize registry to JSON: {e}"))?;
    println!("{json}");
    Ok(())
}

fn show_registry_yaml(registry: &MlxVariableRegistry) -> Result<(), Box<dyn std::error::Error>> {
    let yaml = serde_yaml::to_string(registry)
        .map_err(|e| format!("Failed to serialize registry to YAML: {e}"))?;
    print!("{yaml}");
    Ok(())
}

// Basic helper function to wrap text at specified width.
fn wrap_text(text: &str, width: usize) -> String {
    if text.len() <= width {
        return text.to_string();
    }

    let mut result = String::new();
    let mut current_line = String::new();

    for word in text.split_whitespace() {
        if current_line.len() + word.len() + 1 > width {
            if !result.is_empty() {
                result.push('\n');
            }
            result.push_str(&current_line);
            current_line = word.to_string();
        } else {
            if !current_line.is_empty() {
                current_line.push(' ');
            }
            current_line.push_str(word);
        }
    }

    if !current_line.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&current_line);
    }

    result
}

// run_runner_command runs the specific runner subcommands.
fn run_runner_command(
    device: &str,
    runner_command: RunnerCommands,
    options: ExecOptions,
) -> Result<(), MlxRunnerError> {
    match runner_command {
        RunnerCommands::Query {
            registry,
            variables,
            format,
        } => query_command(device, &registry, variables.as_ref(), format, options)?,
        RunnerCommands::Set {
            registry,
            assignments,
        } => set_command(device, &registry, &assignments, options)?,
        RunnerCommands::Sync {
            registry,
            assignments,
        } => sync_command(device, &registry, &assignments, options)?,
        RunnerCommands::Compare {
            registry,
            assignments,
        } => compare_command(device, &registry, &assignments, options)?,
    }
    Ok(())
}

// query_command executes a query, with an optional
// list of variables to get. If unset, it will query
// all variables configured in the provided registry.
fn query_command(
    device: &str,
    registry_name: &str,
    variables_filter: Option<&Vec<String>>,
    format: OutputFormat,
    options: ExecOptions,
) -> Result<(), MlxRunnerError> {
    let registry = get_registry(registry_name)?;
    let runner = MlxConfigRunner::with_options(device.to_string(), registry, options.clone());

    // Either query specific variables, or all.
    let result = if let Some(var_list) = variables_filter {
        runner.query(var_list.as_slice())?
    } else {
        runner.query_all()?
    };

    // ...and then output results in the specified format.
    match format {
        OutputFormat::AsciiTable => {
            display_query_results_table(&result, options.verbose);
        }
        OutputFormat::Json => {
            display_query_results_json(&result)?;
        }
        OutputFormat::Yaml => {
            display_query_results_yaml(&result)?;
        }
    }

    Ok(())
}

// set_command executes a `set` command to set one or
// more key=val variable values, where the variables must
// exist in the provided registry.
fn set_command(
    device: &str,
    registry_name: &str,
    assignments: &[String],
    options: ExecOptions,
) -> Result<(), MlxRunnerError> {
    let registry = get_registry(registry_name)?;
    let runner = MlxConfigRunner::with_options(device.to_string(), registry, options);

    // Take the vec of key=val and make sure each key=val is a key=val.
    let parsed_assignments =
        parse_assignments(assignments).map_err(|e| MlxRunnerError::GenericError(e.to_string()))?;

    let total = parsed_assignments.len();
    println!("Setting {total} variables on device {device} using registry '{registry_name}':");
    for (var, val) in &parsed_assignments {
        println!(" - {var} = {val}");
    }
    println!();

    runner.set(parsed_assignments)?;
    println!("Set operation complete: {total} variables configured",);
    Ok(())
}

// sync executes a sync (query + set) flow.
fn sync_command(
    device: &str,
    registry_name: &str,
    assignments: &[String],
    options: ExecOptions,
) -> Result<(), MlxRunnerError> {
    let registry = get_registry(registry_name)?;
    let runner = MlxConfigRunner::with_options(device.to_string(), registry, options);

    let parsed_assignments =
        parse_assignments(assignments).map_err(|e| MlxRunnerError::GenericError(e.to_string()))?;
    let total = parsed_assignments.len();

    println!("Syncing {total} variables on device {device} using registry '{registry_name}':");
    for (var, val) in &parsed_assignments {
        println!(" - {var} = {val}");
    }
    println!();

    let sync_result = runner.sync(parsed_assignments)?;
    println!("Sync Results:");
    println!("{}", sync_result.summary());
    println!();

    if sync_result.changes_applied.is_empty() {
        println!("Already in sync.");
    } else {
        println!("Changes applied:");
        for change in &sync_result.changes_applied {
            println!("  - {}", change.description());
        }
    }

    Ok(())
}

// compare executes a comparison operation.
fn compare_command(
    device: &str,
    registry_name: &str,
    assignments: &[String],
    options: ExecOptions,
) -> Result<(), MlxRunnerError> {
    let registry = get_registry(registry_name)?;
    let runner = MlxConfigRunner::with_options(device.to_string(), registry, options);

    // Parse assignments
    let parsed_assignments =
        parse_assignments(assignments).map_err(|e| MlxRunnerError::GenericError(e.to_string()))?;
    let total = parsed_assignments.len();

    println!("Comparing {total} variables on device {device} using registry '{registry_name}':");
    for (var, val) in &parsed_assignments {
        println!(" - {var} = {val}");
    }
    println!();

    let comparison_result = runner.compare(parsed_assignments)?;
    println!("Comparison Results:");
    println!("{}", comparison_result.summary());
    println!();

    if comparison_result.planned_changes.is_empty() {
        println!("Already in sync.");
    } else {
        println!("PlannedChange:");
        for change in &comparison_result.planned_changes {
            println!(" - {}", change.description());
        }
        println!();
    }

    Ok(())
}

// display_query_results_table displays query results
// as a pretty ASCII table.
fn display_query_results_table(result: &QueryResult, verbose: bool) {
    if verbose {
        println!("Query Results");
        println!(
            " Device: {} ({})",
            result
                .device_info
                .device_type
                .as_deref()
                .unwrap_or("Unknown"),
            result
                .device_info
                .part_number
                .as_deref()
                .unwrap_or("Unknown part")
        );
        println!(" Variables: {}", result.variable_count());
        println!();
    }

    let mut table = Table::new();
    table.add_row(Row::new(vec![
        Cell::new("Variable Name"),
        Cell::new("Modified"),
        Cell::new("Next Value"),
        Cell::new("Current Value"),
        Cell::new("Default Value"),
    ]));

    for var in &result.variables {
        // Use fun emojis because Andrew likes them.
        let modified_icon = if var.modified { "🔄" } else { "✅" };

        table.add_row(Row::new(vec![
            Cell::new(var.name()),
            Cell::new(modified_icon),
            Cell::new(&var.next_value.to_display_string()),
            Cell::new(&var.current_value.to_display_string()),
            Cell::new(&var.default_value.to_display_string()),
        ]));
    }

    table.printstd();

    if verbose {
        println!();
        println!(
            "Query complete: {} variables retrieved",
            result.variable_count()
        );
    }
}

// display_query_results_json prints query results as JSON.
fn display_query_results_json(result: &QueryResult) -> Result<(), MlxRunnerError> {
    let json_output = serde_json::to_string_pretty(&result.variables).map_err(|e| {
        MlxRunnerError::JsonParsing {
            content: "query result".to_string(),
            error: e,
        }
    })?;

    println!("{json_output}");
    Ok(())
}

// display_query_results_yaml prints query results as YAML.
fn display_query_results_yaml(result: &QueryResult) -> Result<(), MlxRunnerError> {
    let yaml_output = serde_yaml::to_string(&result.variables)
        .map_err(|e| MlxRunnerError::GenericError(format!("{e}")))?;
    println!("{yaml_output}");
    Ok(())
}

// get_registry gets a registry by name.
fn get_registry(registry_name: &str) -> Result<MlxVariableRegistry, MlxRunnerError> {
    registries::get(registry_name)
        .cloned()
        .ok_or_else(|| MlxRunnerError::VariableNotFound {
            variable_name: format!("registry '{registry_name}'"),
        })
}

// parse_assignments is used for parsing the comma-separated list
// of key=val variable assignments for set, sync, and compare.
pub fn parse_assignments(assignments: &[String]) -> Result<Vec<(String, String)>, String> {
    let mut parsed = Vec::new();

    for assignment in assignments {
        let assignment = assignment.trim();
        if assignment.is_empty() {
            continue;
        }

        // Split on the first '=' sign.
        let parts: Vec<&str> = assignment.splitn(2, '=').collect();
        if parts.len() != 2 {
            return Err(format!(
                "Invalid assignment format: '{assignment}'. Expected 'variable=value'"
            ));
        }

        let variable = parts[0].to_string();
        let value = parts[1].to_string();

        if variable.is_empty() {
            return Err("Variable name cannot be empty".to_string());
        }

        parsed.push((variable, value));
    }

    if parsed.is_empty() {
        return Err("No valid assignments found".to_string());
    }

    Ok(parsed)
}

// run_profile_command runs the specific profile subcommands.
fn run_profile_command(
    device: &str,
    profile_command: ProfileCommands,
    options: ExecOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    match profile_command {
        ProfileCommands::Sync { yaml_path } => profile_sync_command(device, &yaml_path, options)?,
        ProfileCommands::Compare { yaml_path } => {
            profile_compare_command(device, &yaml_path, options)?
        }
    }
    Ok(())
}

// profile_sync_command loads a profile from YAML and syncs it to the device.
fn profile_sync_command(
    device: &str,
    yaml_path: &std::path::Path,
    options: ExecOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Loading profile from: {}", yaml_path.display());
    let profile = MlxConfigProfile::from_yaml_file(yaml_path)
        .map_err(|e| format!("Failed to load profile: {e}"))?;

    println!("{}", profile.summary());
    println!();

    let sync_result = profile
        .sync(device, Some(options))
        .map_err(|e| format!("Profile sync failed: {e}"))?;

    println!("Profile Sync Results:");
    println!("{}", sync_result.summary());
    println!();

    if sync_result.changes_applied.is_empty() {
        println!("Device is already in sync with profile.");
    } else {
        println!("Changes applied:");
        for change in &sync_result.changes_applied {
            println!("  • {}", change.description());
        }
    }

    Ok(())
}

// profile_compare_command loads a profile from YAML and compares it against the device.
fn profile_compare_command(
    device: &str,
    yaml_path: &std::path::Path,
    options: ExecOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    println!("Loading profile from: {}", yaml_path.display());
    let profile = MlxConfigProfile::from_yaml_file(yaml_path)
        .map_err(|e| format!("Failed to load profile: {e}"))?;

    println!("{}", profile.summary());
    println!();

    let comparison_result = profile
        .compare(device, Some(options))
        .map_err(|e| format!("Profile comparison failed: {e}"))?;

    println!("Profile Comparison Results:");
    println!("{}", comparison_result.summary());
    println!();

    if comparison_result.planned_changes.is_empty() {
        println!("Device is already in sync with profile.");
    } else {
        println!("Changes needed:");
        for change in &comparison_result.planned_changes {
            println!("  • {}", change.description());
        }
    }

    Ok(())
}

// run_firmware_command dispatches firmware subcommands.
async fn run_firmware_command(
    dry_run: bool,
    work_dir: Option<std::path::PathBuf>,
    action: FirmwareAction,
) -> Result<(), Box<dyn std::error::Error>> {
    match action {
        FirmwareAction::Flash {
            device,
            part_number,
            psid,
            version,
            firmware_url,
            device_conf_url,
            firmware_bearer_token,
            firmware_basic_auth,
            firmware_ssh_key,
            firmware_ssh_agent,
            device_conf_bearer_token,
            device_conf_basic_auth,
            device_conf_ssh_key,
            device_conf_ssh_agent,
        } => {
            let spec = FirmwareSpec {
                part_number,
                psid,
                version,
            };
            let flasher = FirmwareFlasher::new(&device, &spec)?.with_dry_run(dry_run);

            let firmware_credentials = build_credentials(
                firmware_bearer_token.as_deref(),
                firmware_basic_auth.as_deref(),
                firmware_ssh_key.as_ref(),
                firmware_ssh_agent,
            )?;

            let device_conf_credentials = if device_conf_url.is_some() {
                build_credentials(
                    device_conf_bearer_token.as_deref(),
                    device_conf_basic_auth.as_deref(),
                    device_conf_ssh_key.as_ref(),
                    device_conf_ssh_agent,
                )?
            } else {
                None
            };

            let flash_spec = FlashSpec {
                firmware_url,
                firmware_credentials,
                device_conf_url,
                device_conf_credentials,
                verify_from_cache: false,
                cache_dir: work_dir,
            };

            flasher.flash(&flash_spec).await?;
        }

        FirmwareAction::FlashConfig {
            device,
            config_file,
        } => {
            let profile = FirmwareFlasherProfile::from_file(&config_file)?;
            let flasher =
                FirmwareFlasher::new(&device, &profile.firmware_spec)?.with_dry_run(dry_run);

            flasher.apply(&profile).await?;
        }

        FirmwareAction::VerifyImage {
            device,
            part_number,
            psid,
            version,
            image_url,
            bearer_token,
            basic_auth,
            ssh_key,
            ssh_agent,
        } => {
            let spec = FirmwareSpec {
                part_number,
                psid,
                version,
            };
            let flasher = FirmwareFlasher::new(&device, &spec)?.with_dry_run(dry_run);

            let firmware_credentials = build_credentials(
                bearer_token.as_deref(),
                basic_auth.as_deref(),
                ssh_key.as_ref(),
                ssh_agent,
            )?;

            let flash_spec = FlashSpec {
                firmware_url: image_url,
                firmware_credentials,
                device_conf_url: None,
                device_conf_credentials: None,
                verify_from_cache: false,
                cache_dir: work_dir,
            };

            flasher.verify_image(&flash_spec).await?;
        }

        FirmwareAction::VerifyVersion {
            device,
            part_number,
            psid,
            version,
        } => {
            let spec = FirmwareSpec {
                part_number,
                psid,
                version,
            };
            let flasher = FirmwareFlasher::new(&device, &spec)?.with_dry_run(dry_run);

            flasher.verify_version()?;
        }

        FirmwareAction::Reset {
            device,
            part_number,
            psid,
            version,
            level,
        } => {
            let spec = FirmwareSpec {
                part_number,
                psid,
                version,
            };
            let flasher = FirmwareFlasher::new(&device, &spec)?.with_dry_run(dry_run);

            flasher.reset_with_level(level)?;
        }

        FirmwareAction::ConfigReset { device } => {
            let exec_options = ExecOptions::new().with_dry_run(dry_run);
            let applier =
                crate::runner::applier::MlxConfigApplier::with_options(&device, exec_options);
            applier.reset_config()?;

            tracing::info!("Configuration reset complete");
        }
    }

    Ok(())
}

// build_credentials constructs an optional Credentials from CLI flags.
// At most one credential type should be set.
fn build_credentials(
    bearer_token: Option<&str>,
    basic_auth: Option<&str>,
    ssh_key: Option<&std::path::PathBuf>,
    ssh_agent: bool,
) -> Result<Option<Credentials>, Box<dyn std::error::Error>> {
    if let Some(token) = bearer_token {
        Ok(Some(Credentials::bearer_token(token)))
    } else if let Some(auth) = basic_auth {
        let (username, password) = auth
            .split_once(':')
            .ok_or("Basic auth must be in user:password format")?;
        Ok(Some(Credentials::basic_auth(username, password)))
    } else if ssh_agent {
        Ok(Some(Credentials::ssh_agent()))
    } else if let Some(key_path) = ssh_key {
        Ok(Some(Credentials::ssh_key(key_path.to_string_lossy())))
    } else {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::*;
    use carbide_test_support::{scenarios, value_scenarios};

    use super::*;

    // parse_assignments splits each "VAR=value" on the first '=', trims and skips
    // blank entries, and reports precise errors for malformed input.
    #[test]
    fn parse_assignments_cases() {
        fn run(args: &[&str]) -> Result<Vec<(String, String)>, String> {
            parse_assignments(&args.iter().map(|s| s.to_string()).collect::<Vec<_>>())
        }
        scenarios!(
            run = run;
            "single assignment" {
                &["SRIOV_EN=1"][..] => Yields(vec![("SRIOV_EN".to_string(), "1".to_string())]),
            }

            "value with '=' splits on the first only" {
                &["KEY=a=b"][..] => Yields(vec![("KEY".to_string(), "a=b".to_string())]),
            }

            "whitespace trimmed and blank entries skipped" {
                &["  X=1  ", "", "   ", "Y=2"][..] => Yields(vec![
                    ("X".to_string(), "1".to_string()),
                    ("Y".to_string(), "2".to_string()),
                ]),
            }

            "missing '=' is rejected" {
                &["NOEQUALS"][..] => FailsWith(
                    "Invalid assignment format: 'NOEQUALS'. Expected 'variable=value'"
                        .to_string(),
                ),
            }

            "empty variable name is rejected" {
                &["=value"][..] => FailsWith("Variable name cannot be empty".to_string()),
            }

            "all-blank input yields no assignments" {
                &["", "   "][..] => FailsWith("No valid assignments found".to_string()),
            }
        );
    }

    // parse_mlx_show_confs pulls `VAR=<TYPE> description` definitions out of mlxconfig
    // show-confs text, skipping the header and section lines.
    #[test]
    fn parse_mlx_show_confs_extracts_variable_names() {
        value_scenarios!(
            run = |content| {
                parse_mlx_show_confs(content)
                    .variable_names()
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<String>>()
            };
            "two variables under a section" {
                "List of configurations:\n   POWER SETTINGS:\n       SRIOV_EN=<BOOLEAN> Enable SR-IOV\n       NUM_OF_VFS=<INTEGER> Number of virtual functions\n" => vec!["SRIOV_EN".to_string(), "NUM_OF_VFS".to_string()],
            }

            "header only yields no variables" {
                "List of configurations:\n" => Vec::<String>::new(),
            }
        );
    }
}
