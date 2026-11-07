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

// src/runner.rs
// Main mlxconfig command runner, pulling together all of the goodies
// we've defined in this crate, as well as the mlxconfig-variables and
// mlxconfig-registry crates, to have a type-safe, registry-driven
// configuration management suite to safely execute mlxconfig commands.

use std::collections::HashMap;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::runner::command_builder::CommandBuilder;
use crate::runner::error::MlxRunnerError;
use crate::runner::exec_options::{ExecOptions, is_destructive_variable};
use crate::runner::executor::CommandExecutor;
use crate::runner::json_parser::JsonResponseParser;
use crate::runner::result_types::{
    ComparisonResult, PlannedChange, QueriedDeviceInfo, QueryResult, SyncResult, VariableChange,
};
use crate::runner::traits::{MlxConfigQueryable, MlxConfigSettable};
use crate::variables::registry::MlxVariableRegistry;
use crate::variables::value::MlxConfigValue;

// MlxConfigRunner is the main orchestrator for mlxconfig CLI operations.
#[derive(Debug, Clone)]
pub struct MlxConfigRunner {
    /// device is the device identifier (e.g., "01:00.0").
    device: String,
    // registry is the registry defining available variables
    // and their types. An instance of MlxConfigRunner must
    // be backed by a registry.
    registry: MlxVariableRegistry,
    // temp_file_prefix is an optional temp file prefix
    // to use with the mlxconfig JSON interface
    // (defaults to /tmp).
    temp_file_prefix: Option<String>,
    // options contains the execution options (which
    // includes things like timeout, retries, etc).
    options: ExecOptions,
}

// JsonResponse is the JSON representation of
// an mlxconfig JSON response.
#[derive(Debug, Deserialize, Serialize)]
struct _JsonResponse {
    #[serde(rename = "Device #1")]
    device: _JsonDevice,
}

// JsonDevice is the device information from an
// mlxconfig JSON response, where tlv_configuration
// is the actual set of variables and values.
#[derive(Debug, Deserialize, Serialize)]
struct _JsonDevice {
    description: String,
    device: String,
    device_type: String,
    name: String,
    tlv_configuration: HashMap<String, _JsonVariable>,
}

// JsonVariable is an individual variable + data
// from an mlxconfig JSON response. The default,
// current, and next values will all be parsed
// and converted into MlxConfigValue instances.
#[derive(Debug, Deserialize, Serialize)]
struct _JsonVariable {
    current_value: serde_json::Value,
    default_value: serde_json::Value,
    modified: bool,
    next_value: serde_json::Value,
    read_only: bool,
}

// JsonValueField is used to determine which
// JSON field we're plucking from a value.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum JsonValueField {
    Default,
    Current,
    Next,
}

impl MlxConfigRunner {
    // new creates a new runner for the given device,
    // backed by the given registry, using default
    // ExecOptions.
    pub fn new(device: String, registry: MlxVariableRegistry) -> Self {
        Self {
            device,
            registry,
            temp_file_prefix: None,
            options: ExecOptions::default(),
        }
    }

    // with_options is a builder to set custom ExecOptions
    // on the runner.
    pub fn with_options(
        device: String,
        registry: MlxVariableRegistry,
        options: ExecOptions,
    ) -> Self {
        Self {
            device,
            registry,
            temp_file_prefix: None,
            options,
        }
    }

    // set_temp_file_prefix is a builder to set a custom temp file
    // prefix on the runner, which defaults to /tmp if unset.
    pub fn set_temp_file_prefix<P: Into<String>>(&mut self, prefix: P) {
        self.temp_file_prefix = Some(prefix.into());
    }

    // get_temp_file_prefix returns the effective temp file prefix.
    fn get_temp_file_prefix(&self) -> &str {
        self.temp_file_prefix.as_deref().unwrap_or("/tmp")
    }

    // query_all queries all variables in the registry.
    pub fn query_all(&self) -> Result<QueryResult, MlxRunnerError> {
        let all_variables: Vec<&str> = self.registry.variable_names();
        self.query(all_variables.as_slice())
    }

    // query queries specific variables from the registry
    // using the unified API.
    pub fn query<Q: MlxConfigQueryable>(
        &self,
        queryable: Q,
    ) -> Result<QueryResult, MlxRunnerError> {
        let variable_names = queryable.to_variable_names(&self.registry)?;
        self.query_variables_internal(&variable_names)
    }

    // set is used to set registry-backed variables on
    // the device using the unified API.
    pub fn set<S: MlxConfigSettable>(&self, settable: S) -> Result<(), MlxRunnerError> {
        let config_values = settable.to_config_values(&self.registry)?;
        self.set_values_internal(&config_values)
    }

    // sync will sync registry-backed variables using the unified API
    // (via our query → compare → set workflow, if differences exist).
    pub fn sync<S: MlxConfigSettable>(&self, settable: S) -> Result<SyncResult, MlxRunnerError> {
        let start_time = Instant::now();
        let desired_values = settable.to_config_values(&self.registry)?;

        // Query current state of all target variables.
        //let variable_names: Vec<String> = desired_values
        //    .iter()
        //    .map(|v| v.name().to_string())
        // .collect();
        let variable_names = self.expand_query_variable_names(&desired_values);
        let query_result = self.query_variables_internal(&variable_names)?;

        // Build comparison between desired and current next_value.
        let mut planned_changes = Vec::new();
        let mut values_to_set = Vec::new();

        for desired_value in &desired_values {
            if let Some(queried_var) = query_result
                .variables
                .iter()
                .find(|v| v.name() == desired_value.name())
            {
                // Compare desired value with next_value (not current_value, to handle
                // pending changes). The reason we look at next_value is because that's
                // the next value to be applied to the card, so that's what we actually
                // want to compare against (we're seeing if the next value matches our
                // desired value, or if we need to change the next value to our desired
                // value).
                if &queried_var.next_value != desired_value {
                    planned_changes.push(PlannedChange {
                        variable_name: desired_value.name().to_string(),
                        current_value: queried_var.next_value.clone(),
                        desired_value: desired_value.clone(),
                    });
                    values_to_set.push(desired_value.clone());
                }
            }
        }

        // Log planned changes, if verbose.
        if self.options.verbose && !planned_changes.is_empty() {
            println!(
                "[INFO] Syncing {} variables on device {}",
                desired_values.len(),
                self.device
            );
            println!("[INFO] Comparison complete:");
            for change in &planned_changes {
                println!("  • {}", change.description());
            }
            println!(
                "[INFO] Setting {} variables with divergent values",
                values_to_set.len()
            );
        }

        // Execute set operation for diverging variables (or skip if dry_run).
        let changes_applied = if values_to_set.is_empty() {
            if self.options.verbose {
                println!("[INFO] No changes needed - all variables already at desired values");
            }
            Vec::new()
        } else if self.options.dry_run {
            println!(
                "[DRY RUN] Would set {} variables: {variables:?}",
                values_to_set.len(),
                variables = values_to_set.iter().map(|v| v.name()).collect::<Vec<_>>()
            );
            Vec::new()
        } else {
            // Run the set operation!
            self.set_values_internal(&values_to_set)?;

            // Convert planned changes to applied changes.
            planned_changes
                .into_iter()
                .map(|pc| VariableChange {
                    variable_name: pc.variable_name,
                    old_value: pc.current_value,
                    new_value: pc.desired_value,
                })
                .collect()
        };

        let execution_time = start_time.elapsed();
        if self.options.verbose {
            println!(
                "[INFO] Sync complete: {}/{} variables changed in {execution_time:?}",
                changes_applied.len(),
                desired_values.len(),
            );
        }

        Ok(SyncResult {
            variables_checked: desired_values.len(),
            variables_changed: changes_applied.len(),
            changes_applied,
            execution_time,
            query_result,
        })
    }

    // compare will compare provided values to observed values on the card.
    // It's basically like a dry-run version of sync, but explicitly used
    // for just doing comparisons.
    pub fn compare<S: MlxConfigSettable>(
        &self,
        settable: S,
    ) -> Result<ComparisonResult, MlxRunnerError> {
        let desired_values = settable.to_config_values(&self.registry)?;

        // Query current state of all target variables.
        //let variable_names: Vec<String> = desired_values
        //    .iter()
        //    .map(|v| v.name().to_string())
        //    .collect();
        let variable_names = self.expand_query_variable_names(&desired_values);
        let query_result = self.query_variables_internal(&variable_names)?;

        // Build comparison between desired and current next_value (see the
        // comments in `sync` for more details as to why we look at next_value
        // instead of current_value).
        let mut planned_changes = Vec::new();

        for desired_value in &desired_values {
            if let Some(queried_var) = query_result
                .variables
                .iter()
                .find(|v| v.name() == desired_value.name())
            {
                // Compare desired value with next_value.
                // TODO(chet): PartialEq *should* work here since the variable
                // defs should *also* match. If it ends up being a problem though,
                // this can just become .value for each.
                if &queried_var.next_value != desired_value {
                    planned_changes.push(PlannedChange {
                        variable_name: desired_value.name().to_string(),
                        current_value: queried_var.next_value.clone(),
                        desired_value: desired_value.clone(),
                    });
                }
            }
        }

        Ok(ComparisonResult {
            variables_checked: desired_values.len(),
            variables_needing_change: planned_changes.len(),
            planned_changes,
            query_result,
        })
    }

    // query_variables_internal is an internal method to query
    // specific variables by name.
    fn query_variables_internal(
        &self,
        variable_names: &[String],
    ) -> Result<QueryResult, MlxRunnerError> {
        self.validate_device_matches_registry()?;
        let executor = CommandExecutor {
            options: &self.options,
        };
        let temp_file = executor.create_temp_file(self.get_temp_file_prefix())?;
        let command_builder = CommandBuilder {
            device: &self.device,
            options: &self.options,
        };
        let command_spec = command_builder.build_query_command(variable_names, &temp_file)?;

        if self.options.verbose {
            println!("[runner] {command_spec}");
        }

        if !executor.is_dry_run() {
            executor.execute_with_retry(&command_spec)?;
            let parser = JsonResponseParser {
                registry: &self.registry,
                options: &self.options,
            };
            let query_result = parser.parse_json_response(&temp_file, &self.device)?;
            executor.cleanup_temp_file(&temp_file)?;
            Ok(query_result)
        } else {
            executor.execute_dry_run(&command_spec, "query");
            executor.cleanup_temp_file(&temp_file)?;
            // Return empty result for dry run
            Ok(QueryResult {
                device_info: QueriedDeviceInfo::default(),
                variables: Vec::new(),
            })
        }
    }

    // set_values_internal is the internal method to
    // set configuration values.
    fn set_values_internal(&self, config_values: &[MlxConfigValue]) -> Result<(), MlxRunnerError> {
        self.validate_device_matches_registry()?;

        if config_values.is_empty() {
            return Ok(());
        }

        let executor = CommandExecutor {
            options: &self.options,
        };

        // Check for destructive variables if confirmation is enabled
        if self.options.confirm_destructive {
            let destructive_vars: Vec<String> = config_values
                .iter()
                .filter(|v| is_destructive_variable(v.name()))
                .map(|v| v.name().to_string())
                .collect();

            if !destructive_vars.is_empty()
                && !executor.prompt_for_confirmation(&destructive_vars)?
            {
                return Err(MlxRunnerError::ConfirmationDeclined {
                    variables: destructive_vars,
                });
            }
        }

        let command_builder = CommandBuilder {
            device: &self.device,
            options: &self.options,
        };
        let assignments = command_builder.build_set_assignments(config_values)?;
        let command_spec = command_builder.build_set_command(&assignments)?;

        if self.options.verbose {
            println!("[runner] {command_spec}");
        }

        if !executor.is_dry_run() {
            executor.execute_with_retry(&command_spec)?;
        } else {
            executor.execute_dry_run(&command_spec, "set");
        }

        Ok(())
    }

    fn expand_query_variable_names(&self, desired_values: &[MlxConfigValue]) -> Vec<String> {
        desired_values
            .iter()
            .flat_map(|config_value| {
                if config_value.value.is_array_type() {
                    // For arrays, create indexed variable names for only the set indices
                    if let Some(indices) = config_value.value.get_set_indices() {
                        indices
                            .into_iter()
                            .map(|index| format!("{}[{}]", config_value.name(), index))
                            .collect::<Vec<_>>()
                    } else {
                        // Shouldn't happen for array types, but fallback to base name
                        vec![config_value.name().to_string()]
                    }
                } else {
                    // For non-arrays, use the variable name directly
                    vec![config_value.name().to_string()]
                }
            })
            .collect()
    }

    // validate_device_matches_registry validates that the target device
    // matches any filters configured in the registry. If the registry has
    // no filters, all devices are allowed. If filters are configured,
    // the device must match them to proceed with operations.
    fn validate_device_matches_registry(&self) -> Result<(), MlxRunnerError> {
        // If the registry has no filters configured, allow all devices.
        if !self.registry.has_filters() {
            if self.options.verbose {
                println!(
                    "[DEVICE] Registry '{}' has no device filters - allowing all devices",
                    self.registry.name
                );
            }
            return Ok(());
        }

        if self.options.verbose {
            println!(
                "[DEVICE] Validating device '{}' against registry '{}' filters",
                self.device, self.registry.name
            );
        }

        // Discover the device to get its info.
        let device_info = crate::device::discovery::discover_device_with_settlement(&self.device)
            .map_err(|error| match error {
            crate::device::discovery::DeviceDiscoveryError::DirectQuery(
                error @ MlxRunnerError::ProcessUnsettled { .. },
            ) => error,
            error => MlxRunnerError::GenericError(format!(
                "failed to discover device '{}': {error}",
                self.device
            )),
        })?;

        // Check if the device matches the registry filters.
        let matches = self.registry.matches_device(&device_info);

        if self.options.verbose {
            println!(
                "[DEVICE] Device '{}' (type: {}, part: {}) {} registry filters",
                device_info.pci_name_pretty(),
                device_info.device_type_pretty(),
                device_info.part_number_pretty(),
                if matches { "matches" } else { "does not match" }
            );
        }

        if matches {
            Ok(())
        } else {
            Err(MlxRunnerError::GenericError(format!(
                "Device '{}' (type: {}, part: {}) does not match registry '{}' filters: {}",
                device_info.pci_name_pretty(),
                device_info.device_type_pretty(),
                device_info.part_number_pretty(),
                self.registry.name,
                self.registry.filter_summary()
            )))
        }
    }
}
