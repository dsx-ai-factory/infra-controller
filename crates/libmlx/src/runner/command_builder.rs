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

// src/command_builder.rs
// Command builder for constructing mlxconfig CLI commands with proper
// argument formatting and assignment generation.

use std::path::Path;
use std::process::Command;

use crate::runner::error::MlxRunnerError;
use crate::runner::exec_options::ExecOptions;
use crate::variables::value::{MlxConfigValue, MlxValueType};

// CommandSpec represents the parameters needed to build a Command.
// This allows us to recreate Command instances for retry logic since
// Command doesn't implement Clone.
#[derive(Debug, Clone)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
}

impl CommandSpec {
    // Creates a new CommandSpec with the given program and arguments.
    pub fn new<P: Into<String>>(program: P) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
        }
    }

    // Adds a single argument to the command.
    pub fn arg<A: Into<String>>(mut self, arg: A) -> Self {
        self.args.push(arg.into());
        self
    }

    // Adds multiple arguments to the command.
    pub fn args<I, A>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = A>,
        A: Into<String>,
    {
        self.args.extend(args.into_iter().map(|a| a.into()));
        self
    }

    // Creates a Command from this CommandSpec.
    pub fn to_command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        command
    }
}

impl std::fmt::Display for CommandSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.args.is_empty() {
            write!(f, "{}", self.program)
        } else {
            write!(f, "{} {}", self.program, self.args.join(" "))
        }
    }
}

// CommandBuilder handles the construction of mlxconfig CLI commands
// for different operations (query, set) with proper argument formatting.
pub struct CommandBuilder<'a> {
    // device is the device identifier (e.g., "01:00.0")
    pub device: &'a str,
    // options contains the execution options for running
    // mlxconfig, including specific behaviors, as well
    // as logging.
    pub options: &'a ExecOptions,
}

impl<'a> CommandBuilder<'a> {
    // build_query_command builds an mlxconfig query command
    // spec, with JSON output configured to specified temp file.
    pub fn build_query_command(
        &self,
        variables: &[String],
        temp_file: &Path,
    ) -> Result<CommandSpec, MlxRunnerError> {
        let mut spec = CommandSpec::new("mlxconfig")
            .arg("-d")
            .arg(self.device)
            .arg("-e")
            .arg("-j")
            .arg(temp_file.to_string_lossy().to_string())
            .arg("q");

        // Add all variable names as individual arguments
        for var in variables {
            spec = spec.arg(var);
        }

        if self.options.verbose {
            println!("[command_builder] built query command spec: {spec}");
        }

        Ok(spec)
    }

    // build_set_command builds an mlxconfig `set` command spec,
    // with all space-separated key=val variable assignments.
    pub fn build_set_command(&self, assignments: &[String]) -> Result<CommandSpec, MlxRunnerError> {
        let mut spec = CommandSpec::new("mlxconfig")
            .arg("-d")
            .arg(self.device)
            .arg("--yes")
            .arg("set");

        // Add each assignment as an argument
        for assignment in assignments {
            spec = spec.arg(assignment);
        }

        if self.options.verbose {
            println!("[command_builder] built set command spec: {spec}");
        }

        Ok(spec)
    }

    // build_set_assignments converts MlxConfigValues to CLI assignment
    // strings, which are in the VAR=value format, including support
    // for arrays, which are in the VAR[index]=value format, and will
    // only set them for indices which are Some(val), and not None --
    // aka sparse array support.
    pub fn build_set_assignments(
        &self,
        config_values: &[MlxConfigValue],
    ) -> Result<Vec<String>, MlxRunnerError> {
        let mut assignments = Vec::new();

        for config_value in config_values {
            match &config_value.value {
                MlxValueType::BooleanArray(values) => {
                    self.build_sparse_array_assignments(
                        &mut assignments,
                        config_value,
                        values,
                        |value| {
                            if *value {
                                "true".to_string()
                            } else {
                                "false".to_string()
                            }
                        },
                    );
                }
                MlxValueType::IntegerArray(values) => {
                    self.build_sparse_array_assignments(
                        &mut assignments,
                        config_value,
                        values,
                        |value| value.to_string(),
                    );
                }
                MlxValueType::EnumArray(values) => {
                    self.build_sparse_array_assignments(
                        &mut assignments,
                        config_value,
                        values,
                        |value| value.clone(),
                    );
                }
                MlxValueType::BinaryArray(values) => {
                    self.build_sparse_array_assignments(
                        &mut assignments,
                        config_value,
                        values,
                        |value| format!("0x{}", hex::encode(value)),
                    );
                }
                MlxValueType::Boolean(value) => {
                    assignments.push(self.build_single_assignment(
                        config_value,
                        if *value { "true" } else { "false" },
                    ));
                }
                MlxValueType::Integer(value) => {
                    assignments
                        .push(self.build_single_assignment(config_value, &value.to_string()));
                }
                MlxValueType::String(value) => {
                    assignments.push(self.build_single_assignment(config_value, value));
                }
                MlxValueType::Enum(value) => {
                    assignments.push(self.build_single_assignment(config_value, value));
                }
                MlxValueType::Preset(value) => {
                    assignments
                        .push(self.build_single_assignment(config_value, &value.to_string()));
                }
                MlxValueType::Binary(value)
                | MlxValueType::Bytes(value)
                | MlxValueType::Opaque(value) => {
                    let hex_str = format!("0x{}", hex::encode(value));
                    assignments.push(self.build_single_assignment(config_value, &hex_str));
                }
                MlxValueType::Array(values) => {
                    // TODO(chet): I actually think I can get rid of generic arrays
                    // now. I had this here at one point, but as things evolved I
                    // went for typed arrays, and now I actually don't think this
                    // would ever be used.
                    assignments.push(self.build_single_assignment(config_value, &values.join(",")));
                }
            }
        }

        if self.options.verbose {
            println!(
                "[CMD] Built {} assignments: {assignments:?}",
                assignments.len()
            );
        }

        Ok(assignments)
    }

    // build_sparse_array_assignments builds assignments for sparse arrays,
    // where only Some() values are set (which of course *could* be all of the
    // indices in the array, but it may not be). Takes a formatting function from
    // the caller to convert the given value to its correct string representation,
    // since we need string representations for the CLI arguments.
    fn build_sparse_array_assignments<T, F>(
        &self,
        assignments: &mut Vec<String>,
        config_value: &MlxConfigValue,
        values: &[Option<T>],
        format_fn: F,
    ) where
        F: Fn(&T) -> String,
    {
        for (index, opt_value) in values.iter().enumerate() {
            if let Some(value) = opt_value {
                assignments.push(format!(
                    "{}[{}]={}",
                    config_value.name(),
                    index,
                    format_fn(value)
                ));
            }
            // And notice here if it's a None value, we skip
            // adding the assignment -- this is because we
            // allow certain indices to be updated, and others
            // to be left alone (aka sparse arrays).
        }
    }

    // build_single_assignment builds a single variable assignment
    // in the most easiest bestest tastiest VAR=value format.
    fn build_single_assignment(&self, config_value: &MlxConfigValue, value_str: &str) -> String {
        format!("{}={}", config_value.name(), value_str)
    }
}
