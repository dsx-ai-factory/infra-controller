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
use std::path::PathBuf;

use carbide_test_support::Outcome::*;
use carbide_test_support::{Case, check_cases};
use libmlx::runner::applier::MlxConfigApplier;
use libmlx::runner::error::MlxRunnerError;
use libmlx::runner::exec_options::ExecOptions;

enum ApplierOperation {
    ApplyDryRun(PathBuf),
    ResetDryRun,
    ApplyMissing(PathBuf),
}

#[derive(Debug, PartialEq)]
enum ApplierFailure {
    Generic(String),
    Unexpected(String),
}

fn run_operation(operation: ApplierOperation) -> Result<(), ApplierFailure> {
    let result = match operation {
        ApplierOperation::ApplyDryRun(path) => {
            let options = ExecOptions::new().with_dry_run(true).with_verbose(true);
            MlxConfigApplier::with_options("01:00.0", options).apply(&path)
        }
        ApplierOperation::ResetDryRun => {
            let options = ExecOptions::new().with_dry_run(true);
            MlxConfigApplier::with_options("01:00.0", options).reset_config()
        }
        ApplierOperation::ApplyMissing(path) => {
            let options = ExecOptions::new().with_dry_run(true);
            MlxConfigApplier::with_options("01:00.0", options).apply(&path)
        }
    };

    result.map_err(|error| match error {
        MlxRunnerError::GenericError(message) => ApplierFailure::Generic(message),
        error => ApplierFailure::Unexpected(format!("{error:?}")),
    })
}

#[test]
fn dry_run_and_preflight_cases() {
    let temp_dir = tempfile::tempdir().unwrap();
    let present_path = temp_dir.path().join("config.bin");
    let missing_path = temp_dir.path().join("missing.bin");
    fs::write(&present_path, b"config").unwrap();

    check_cases(
        [
            Case {
                scenario: "apply accepts an existing config during dry run",
                input: ApplierOperation::ApplyDryRun(present_path),
                expect: Yields(()),
            },
            Case {
                scenario: "reset completes during dry run",
                input: ApplierOperation::ResetDryRun,
                expect: Yields(()),
            },
            Case {
                scenario: "apply rejects a missing config before execution",
                input: ApplierOperation::ApplyMissing(missing_path.clone()),
                expect: FailsWith(ApplierFailure::Generic(format!(
                    "Configuration file does not exist: {}",
                    missing_path.display()
                ))),
            },
        ],
        run_operation,
    );
}
