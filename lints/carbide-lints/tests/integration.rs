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
use std::path::PathBuf;
use std::process::Command;

#[test]
fn driver_runs_and_emits_expected_lint() {
    // Path to the fixture crate
    let manifest_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("app")
        .join("Cargo.toml");

    let stderr_fixture_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("app")
        .join("src")
        .join("main.stderr");

    let expected_stderr =
        std::fs::read_to_string(&stderr_fixture_path).expect("Could not read main.stderr");

    // Path to the just-built compiler driver binary
    let driver = env!("CARGO_BIN_EXE_carbide-lints");

    // Optionally isolate target dir so we don't fight with main workspace targets
    let target_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("driver_tests");

    let cargo = std::env::var("CARGO").expect("Cargo should set $CARGO to the running binary");

    let clean_status = Command::new(&cargo)
        .arg("clean")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .arg("-p")
        .arg("sqlx_app")
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .expect("failed to clean sqlx_app fixture");
    assert!(clean_status.success(), "failed to clean sqlx_app fixture");

    let output = Command::new(cargo)
        .arg("check")
        .arg("--manifest-path")
        .arg(&manifest_path)
        .env("RUSTC", driver)
        .env("CARGO_TARGET_DIR", &target_dir)
        .output()
        .expect("failed to run cargo check with carbide-lints");

    let stderr = String::from_utf8_lossy(&output.stderr);
    let relevant_stderr = stderr
        .lines()
        .skip_while(|l| !l.contains("Checking sqlx_app ")) // last line before our stderr
        .skip(1) // skip that line
        .collect::<Vec<_>>()
        .join("\n");

    let diff =
        similar::TextDiff::from_lines(expected_stderr.trim_end(), relevant_stderr.trim_end())
            .unified_diff()
            .context_radius(3)
            .header("expected", "output")
            .to_string();

    // Now assert on stderr
    assert!(
        diff.is_empty(),
        "Compiler output did not match expected. Diff:\n\n{diff}"
    );
}
