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

use ssh_console::config::Config;

#[test]
fn default_run_config_is_clean_toml() {
    for (scenario, args, endpoint) in [
        ("debug logging", vec!["--debug", "default-run-config"], None),
        (
            "OTLP export",
            vec!["default-run-config"],
            Some("http://127.0.0.1:4317"),
        ),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_ssh-console"));
        command.args(args).env_remove("RUST_LOG");
        command.env_remove("OTEL_EXPORTER_OTLP_ENDPOINT");
        command.env_remove("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT");
        if let Some(endpoint) = endpoint {
            command.env("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT", endpoint);
        }
        let output = command.output().expect("ssh-console should run");
        assert!(output.status.success(), "{scenario}: {output:?}");
        let stdout = String::from_utf8(output.stdout).expect("config should be UTF-8");
        let config: Config = toml::from_str(&stdout)
            .unwrap_or_else(|error| panic!("{scenario}: invalid TOML: {error}\n{stdout}"));
        assert_eq!(config, Config::default(), "{scenario}");
    }
}
