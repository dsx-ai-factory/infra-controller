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

// src/error.rs
// Defines errors for the mlxconfig-runner crate.

use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;

use crate::variables::value::MlxValueError;

// MlxRunnerError is the core error type for the
// mlxconfig-runner crate (and underlying mlxconfig
// execution issues).
#[derive(Debug, Error)]
pub enum MlxRunnerError {
    // CommandExecution is returned when mlxconfig command
    // execution failed.
    #[error(
        "command execution failed: {command}\nexit code: {exit_code:?}\nstdout: {stdout}\nstderr: {stderr}"
    )]
    CommandExecution {
        command: String,
        exit_code: Option<i32>,
        stdout: String,
        stderr: String,
    },

    // JsonParsing is returned when mlxconfig JSON response
    // parsing failed.
    #[error("failed to parse JSON response: {error}\ncontent: {content}")]
    JsonParsing {
        content: String,
        error: serde_json::Error,
    },

    // VariableNotFound is returned when a targeted variable
    // is not found in the targeted registry.
    #[error("variable '{variable_name}' not found in registry")]
    VariableNotFound { variable_name: String },

    // ArraySizeMismatch is returned when there is an array size
    // mismatch between what the device expects and what the
    // registry has defined the variable's array size as.
    #[error("array size mismatch for '{variable_name}': expected {expected}, found {found}")]
    ArraySizeMismatch {
        variable_name: String,
        expected: usize,
        found: usize,
    },

    // ValueConversion is returned when there is a value converting
    // the returned value into the value defined for the variable.
    #[error("value conversion failed for '{variable_name}' with value '{value}': {error}")]
    ValueConversion {
        variable_name: String,
        value: String,
        error: MlxValueError,
    },

    // TempFileError is returned when there is an issue working with
    // the JSON temporary file at a given path.
    #[error("temporary file error at '{path}': {error}")]
    TempFileError {
        path: PathBuf,
        error: std::io::Error,
    },

    // Timeout is returned when the mlxconfig command execution
    // has exceeded the configured timeout (in seconds).
    #[error("command timed out after {duration:?}: {command}")]
    Timeout { command: String, duration: Duration },

    // ConfirmationDeclined is returned when an operation is
    // being run in interactive mode (e.g. reference CLI, scout,
    // DPU agent, etc), and the user declined a prompt used to
    // verify a destructive operation.
    #[error("user declined confirmation for destructive variables: {variables:?}")]
    ConfirmationDeclined { variables: Vec<String> },

    // InvalidArrayIndex is returned when the string "API" is
    // being used to configure a given variable array index,
    // and the format being used is invalid -- this is basically
    // just a string parsing error.
    #[error("invalid array index syntax in '{variable_name}': expected format 'VAR[index]'")]
    InvalidArrayIndex { variable_name: String },

    // NoDeviceFound is reutrned when no device info is found
    // in the mlxconfig JSON response.
    #[error("no device found in mlxconfig JSON response")]
    NoDeviceFound,

    // DeviceMismatch is returned when there is a mismatch
    // between the requested device and the device found.
    #[error("device mismatch: expected '{expected}', found '{actual}'")]
    DeviceMismatch { expected: String, actual: String },

    // Io is just a generic I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("generic runner error: {0}")]
    GenericError(String),
}

impl MlxRunnerError {
    // command_execution is a helper to create a command execution
    // error with context.
    pub fn command_execution(command: String, output: std::process::Output) -> Self {
        Self::CommandExecution {
            command,
            exit_code: output.status.code(),
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        }
    }

    // json_parsing is as helper to create a JSON parsing
    // error with context.
    pub fn json_parsing(content: String, error: serde_json::Error) -> Self {
        Self::JsonParsing { content, error }
    }

    // value_conversion is a helper to create a value conversion
    // error with context.
    pub fn value_conversion(variable_name: String, value: String, error: MlxValueError) -> Self {
        Self::ValueConversion {
            variable_name,
            value,
            error,
        }
    }

    // temp_file_error is a helper to create a temp file
    // error with context.
    pub fn temp_file_error(path: PathBuf, error: std::io::Error) -> Self {
        Self::TempFileError { path, error }
    }
}

// Impl to allow conversion from an MlxValueError to
// an MlxRunnerError, which I'm sure will probably
// come in handy.
impl From<MlxValueError> for MlxRunnerError {
    fn from(error: MlxValueError) -> Self {
        Self::ValueConversion {
            variable_name: "unknown".to_string(),
            value: "unknown".to_string(),
            error,
        }
    }
}
