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

// tests/runner_integration_tests.rs
// Integration tests for MlxConfigRunner functionality

use std::time::Duration;

use carbide_test_support::Outcome::*;
use carbide_test_support::scenarios;
use libmlx::runner::error::MlxRunnerError;
use libmlx::runner::exec_options::ExecOptions;
use libmlx::runner::runner::MlxConfigRunner;

use super::common;

// Note: These tests focus on the runner's internal logic and error handling
// rather than actually executing mlxconfig commands, since we can't rely on
// mlxconfig being available or specific hardware being present in test environments.

// A dry-run runner over a fresh test registry, targeting `01:00.0`. Dry run keeps
// these tests off any real mlxconfig binary; the construction was copy-pasted into
// almost every test below.
fn dry_run_runner() -> MlxConfigRunner {
    let registry = common::create_test_registry();
    let options = ExecOptions::new().with_dry_run(true);
    MlxConfigRunner::with_options("01:00.0".to_string(), registry, options)
}

// Runs `set` on a fresh dry-run runner and distills the error to a comparable
// shape: a `VariableNotFound` becomes its offending variable name (the part of the
// error that is the contract), any other failure becomes a generic marker. Lets one
// `check_cases` table both pin the not-found name (`FailsWith`) and assert plain
// rejection (`Fails`) for the validation failures.
fn set_outcome(assignments: &[(&str, &str)]) -> Result<(), String> {
    dry_run_runner().set(assignments).map_err(|err| match err {
        MlxRunnerError::VariableNotFound { variable_name } => variable_name,
        other => format!("other: {other:?}"),
    })
}

// Every invalid `set` assignment is rejected before any mlxconfig command runs.
// The not-found rows pin the offending variable name (it's the contract, and the
// runner reports the *first* invalid variable); the enum / boolean / array-bounds /
// preset-range rows just assert rejection.
#[test]
fn invalid_set_assignments_are_rejected() {
    scenarios!(
        run = set_outcome;
        "unknown variable names itself" {
            &[("SRIOV_EN", "true"), ("NONEXISTENT_VAR", "value")][..] => FailsWith("NONEXISTENT_VAR".to_string()),
        }

        "first invalid variable wins over later invalid values" {
            &[
                ("NONEXISTENT_VAR", "value"),   // Variable not found
                ("POWER_MODE", "INVALID_MODE"), // Invalid enum value
                ("GPIO_ENABLED[100]", "true"),  // Array index out of bounds
            ][..] => FailsWith("NONEXISTENT_VAR".to_string()),
        }

        "enum value outside allowed options (LOW/MEDIUM/HIGH)" {
            &[("POWER_MODE", "INVALID_POWER_MODE")][..] => Fails,
        }

        "non-boolean value for a boolean variable" {
            &[("SRIOV_EN", "maybe")][..] => Fails,
        }

        "array index past the registry size of 4" {
            &[("GPIO_ENABLED[10]", "true")][..] => Fails,
        }

        "preset above the max of 10" {
            &[("PERFORMANCE_PRESET", "20")][..] => Fails,
        }
    );
}

#[test]
fn test_query_nonexistent_variable() {
    let runner = dry_run_runner();
    let result = runner.query(["NONEXISTENT_VAR"]);

    assert!(result.is_err());
    if let Err(MlxRunnerError::VariableNotFound { variable_name }) = result {
        assert_eq!(variable_name, "NONEXISTENT_VAR");
    } else {
        panic!("Expected VariableNotFound error, got: {result:?}");
    }
}

#[test]
fn test_empty_assignments() {
    let runner = dry_run_runner();

    // Empty assignments array should be handled gracefully
    let empty_assignments: &[(&str, &str)] = &[];

    let result = runner.set(empty_assignments);
    // Should succeed (no operations to perform)
    assert!(result.is_ok());
}

// Everything below drives `dry_run_runner`, which short-circuits before mlxconfig is ever
// invoked: `query` returns an empty result rather than shelling out, `set` returns `Ok(())`,
// and `sync`/`compare` take their counts from the assignment list. That makes the results
// fully determined and host-independent.
//
// The comment that used to sit here claimed the opposite -- that no outcome could be pinned
// without a mockable mlxconfig -- and every test below discarded its result with `let _ =`
// on the strength of it. Since a dry-run query reports no current values, `sync` and
// `compare` can never find a variable to change, which is the interesting half of the
// contract and is now asserted.

#[test]
fn test_sync_with_no_changes_needed() {
    let runner = dry_run_runner();

    let assignments = &[("SRIOV_EN", "true"), ("NUM_OF_VFS", "16")];

    let result = runner
        .sync(assignments)
        .expect("dry-run sync should succeed");
    assert_eq!(result.variables_checked, 2);
    assert_eq!(
        result.variables_changed, 0,
        "a dry-run query returns no current values, so nothing can be found to change"
    );
    assert!(result.changes_applied.is_empty());
}

#[test]
fn test_compare_operation() {
    let runner = dry_run_runner();

    let assignments = &[
        ("SRIOV_EN", "false"),
        ("NUM_OF_VFS", "32"),
        ("POWER_MODE", "LOW"),
    ];

    let result = runner
        .compare(assignments)
        .expect("dry-run compare should succeed");
    assert_eq!(result.variables_checked, 3);
    assert_eq!(
        result.variables_needing_change, 0,
        "nothing to compare against without a real query"
    );
    assert!(result.planned_changes.is_empty());
}

#[test]
fn test_set_with_array_variables() {
    let runner = dry_run_runner();

    // Sparse indices, deliberately non-contiguous.
    let assignments = &[
        ("GPIO_ENABLED[0]", "true"),
        ("GPIO_ENABLED[2]", "false"),
        ("GPIO_MODES[1]", "output"),
        ("GPIO_MODES[3]", "bidirectional"),
    ];

    assert!(
        runner.set(assignments).is_ok(),
        "sparse array assignments should resolve against the registry and build a command"
    );
}

#[test]
fn test_query_all_variables() {
    let runner = dry_run_runner();

    let result = runner
        .query_all()
        .expect("dry-run query_all should succeed");
    assert!(
        result.variables.is_empty(),
        "a dry run reports nothing back from the card"
    );
}

#[test]
fn test_query_specific_variables() {
    let runner = dry_run_runner();

    let result = runner
        .query(["SRIOV_EN", "NUM_OF_VFS"])
        .expect("dry-run query should succeed");
    assert!(result.variables.is_empty());
}

#[test]
fn test_sync_vs_set_vs_compare_consistency() {
    let runner = dry_run_runner();

    let assignments = &[("SRIOV_EN", "true"), ("NUM_OF_VFS", "32")];

    // The point of the test is that all three agree on the same assignment list, so pin
    // that rather than just running them: each accepts the two variables, and neither
    // sync nor compare finds anything to do without a real query behind it.
    assert!(runner.set(assignments).is_ok());

    let synced = runner.sync(assignments).expect("sync should succeed");
    assert_eq!(synced.variables_checked, 2);
    assert_eq!(synced.variables_changed, 0);

    let compared = runner.compare(assignments).expect("compare should succeed");
    assert_eq!(compared.variables_checked, 2);
    assert_eq!(compared.variables_needing_change, 0);
}

#[test]
fn test_different_device_identifiers() {
    let registry = common::create_test_registry();

    let devices = [
        "01:00.0",
        "02:00.0",
        "03:00.1",
        "0000:01:00.0",
        "0000:0a:00.0",
    ];

    for device in &devices {
        let options = ExecOptions::new().with_dry_run(true);
        let runner = MlxConfigRunner::with_options(device.to_string(), registry.clone(), options);
        assert!(
            runner.set([("SRIOV_EN", "true")]).is_ok(),
            "device identifier {device} should be accepted"
        );
    }
}

#[test]
fn test_execution_options_propagation() {
    let registry = common::create_test_registry();

    // Test various option combinations
    let test_cases = vec![
        ExecOptions::new().with_verbose(true),
        ExecOptions::new().with_dry_run(true),
        ExecOptions::new().with_retries(5),
        ExecOptions::new().with_timeout(Some(Duration::from_secs(60))),
        ExecOptions::new()
            .with_verbose(true)
            .with_dry_run(true)
            .with_retries(3)
            .with_confirm_destructive(true),
    ];

    for options in test_cases {
        // Only the dry-run cases can be asserted here -- the others would shell out to a
        // real mlxconfig. What this pins is that no option combination makes the runner
        // reject an assignment it would otherwise accept.
        let dry_run = options.clone().with_dry_run(true);
        let runner =
            MlxConfigRunner::with_options("01:00.0".to_string(), registry.clone(), dry_run);
        assert!(runner.set([("SRIOV_EN", "true")]).is_ok());
    }
}
