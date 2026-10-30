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

use std::process::Command;

#[test]
fn connection_budget() {
    for (value, expected) in [
        (None, Some(50)),
        (Some("5"), Some(5)),
        (Some("900"), Some(900)),
        (Some("4"), None),
        (Some("901"), None),
        (Some("invalid"), None),
    ] {
        // Isolate environment changes and the process-global pool.
        let mut command = Command::new(std::env::current_exe().unwrap());
        command.args([
            "--exact",
            "tests::connection_budget_probe",
            "--ignored",
            "--nocapture",
        ]);
        command.env_remove("NICO_TEST_MAX_PG_CONNECTIONS");
        if let Some(value) = value {
            command.env("NICO_TEST_MAX_PG_CONNECTIONS", value);
        }
        command.env(
            "NICO_TEST_EXPECT_MAX_CONNECTIONS",
            expected.unwrap_or_default().to_string(),
        );
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(
            output.status.success(),
            expected.is_some(),
            "NICO_TEST_MAX_PG_CONNECTIONS={value:?}: stdout: {stdout}\nstderr: {stderr}"
        );
        if expected.is_none() {
            assert!(
                stderr
                    .contains("NICO_TEST_MAX_PG_CONNECTIONS must be an integer from 5 through 900"),
                "NICO_TEST_MAX_PG_CONNECTIONS={value:?}: {stderr}"
            );
        }
    }
}

#[tokio::test]
#[ignore = "subprocess probe invoked by connection_budget"]
async fn connection_budget_probe() {
    let expected: u32 = std::env::var("NICO_TEST_EXPECT_MAX_CONNECTIONS")
        .unwrap()
        .parse()
        .unwrap();
    let pool = super::init_pool().await;
    assert_eq!(pool.options().get_max_connections(), expected);
    pool.close().await;
}
