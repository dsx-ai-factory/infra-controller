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

use carbide_test_support::value_scenarios;
use libmlx::lockdown::error::MlxError;
use libmlx::lockdown::lockdown::{LockStatus, LockdownManager, StatusReport};
use libmlx::lockdown::runner::FlintRunner;

// MlxError is not PartialEq, so we can't pin it with Outcome::FailsWith. Map a
// result to a stable token naming its error variant (or "ok") so a table can
// assert the variant that IS the contract via plain string equality.
fn error_kind<T>(result: Result<T, MlxError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(MlxError::CommandFailed(_)) => "CommandFailed",
        Err(MlxError::DryRun(_)) => "DryRun",
        Err(MlxError::AlreadyLocked) => "AlreadyLocked",
        Err(MlxError::AlreadyUnlocked) => "AlreadyUnlocked",
        Err(_) => "other",
    }
}

#[test]
fn lock_status_displays_lowercase_name() {
    value_scenarios!(
        run = |status| status.to_string();
        "locked" {
            LockStatus::Locked => "locked".to_string(),
        }

        "unlocked" {
            LockStatus::Unlocked => "unlocked".to_string(),
        }

        "unknown" {
            LockStatus::Unknown => "unknown".to_string(),
        }
    );
}

#[test]
fn test_status_report_json() {
    let report = StatusReport::new("test_device".to_string(), LockStatus::Unlocked);
    let json = report.to_json().unwrap();

    // Parse back to ensure it's valid JSON
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed["device_id"], "test_device");
    assert_eq!(parsed["status"], "unlocked");
    assert!(parsed["timestamp"].is_string());
}

#[test]
fn test_status_report_yaml() {
    let report = StatusReport::new("test_device".to_string(), LockStatus::Unknown);
    let yaml = report.to_yaml().unwrap();

    // Basic validation that it contains expected content
    assert!(yaml.contains("device_id: test_device"));
    assert!(yaml.contains("status: unknown"));
    assert!(yaml.contains("timestamp:"));
}

#[test]
fn test_lockdown_manager_with_dry_run() {
    let manager = LockdownManager::with_dry_run(true).unwrap_or_else(|_| {
        let runner = FlintRunner::with_path("/fake/flint").with_dry_run(true);
        LockdownManager::with_runner(runner)
    });

    // Test that dry run is properly propagated
    let result = manager.lock_device("test_device", "12345678");
    assert!(matches!(result, Err(MlxError::DryRun(_))));
}

// With a fake flint path every operation fails when it tries to execute the tool,
// surfacing as CommandFailed. Each manager method has its own signature, so the
// rows feed pre-computed outcomes (mapped to their error variant) through identity.
#[test]
fn test_manager_error_handling() {
    let runner = FlintRunner::with_path("/fake/flint");
    let manager = LockdownManager::with_runner(runner);

    value_scenarios!(
        run = |kind| kind;
        "lock_device" {
            error_kind(manager.lock_device("fake_device", "12345678")) => "CommandFailed",
        }

        "unlock_device" {
            error_kind(manager.unlock_device("fake_device", "12345678")) => "CommandFailed",
        }

        "get_status" {
            error_kind(manager.get_status("fake_device")) => "CommandFailed",
        }

        "set_device_key" {
            error_kind(manager.set_device_key("fake_device", "12345678")) => "CommandFailed",
        }
    );
}

#[cfg(test)]
mod dry_run_tests {
    use super::*;

    // A dry-run runner short-circuits every operation with DryRun before touching
    // flint. Same per-method-signature shape as test_manager_error_handling.
    #[test]
    fn test_dry_run_manager_operations() {
        let runner = FlintRunner::with_path("/fake/flint").with_dry_run(true);
        let manager = LockdownManager::with_runner(runner);

        value_scenarios!(
            run = |kind| kind;
            "lock_device" {
                error_kind(manager.lock_device("test_device", "12345678")) => "DryRun",
            }

            "unlock_device" {
                error_kind(manager.unlock_device("test_device", "12345678")) => "DryRun",
            }

            "get_status" {
                error_kind(manager.get_status("test_device")) => "DryRun",
            }

            "set_device_key" {
                error_kind(manager.set_device_key("test_device", "12345678")) => "DryRun",
            }
        );
    }

    // The DryRun error carries the command string that would have run; each
    // operation's command must mention the flint sub-command plus the arguments
    // it was handed. A helper pulls the command out of the expected DryRun error.
    #[test]
    fn test_dry_run_commands_contain_expected_parts() {
        let runner = FlintRunner::with_path("/test/flint").with_dry_run(true);
        let manager = LockdownManager::with_runner(runner);

        fn dry_run_command<T: std::fmt::Debug>(result: Result<T, MlxError>) -> String {
            match result {
                Err(MlxError::DryRun(cmd)) => cmd,
                other => panic!("expected DryRun, got {other:?}"),
            }
        }

        let lock_cmd = dry_run_command(manager.lock_device("test_device", "12345678"));
        assert!(lock_cmd.contains("hw_access disable"));
        assert!(lock_cmd.contains("test_device"));
        assert!(lock_cmd.contains("12345678"));

        let unlock_cmd = dry_run_command(manager.unlock_device("test_device", "abcdef01"));
        assert!(unlock_cmd.contains("hw_access enable"));
        assert!(unlock_cmd.contains("abcdef01"));

        let set_key_cmd = dry_run_command(manager.set_device_key("test_device", "12345678"));
        assert!(set_key_cmd.contains("set_key"));
        assert!(set_key_cmd.contains("12345678"));
    }
}
