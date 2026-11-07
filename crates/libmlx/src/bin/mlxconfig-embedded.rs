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
#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

use clap::Parser;
use libmlx::embedded::cmd::{Cli, LogLevel, run_cli};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // While --log-level is used to control the log level here, you
    // can also set RUST_LOG in your environment to override.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
        let level = match cli.log_level {
            LogLevel::Error => "error",
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
            LogLevel::Trace => "trace",
        };
        tracing_subscriber::EnvFilter::new(level)
    });

    tracing_subscriber::fmt().with_env_filter(env_filter).init();

    if let Err(e) = run_cli(cli).await {
        eprintln!("Error: {e}");
        std::process::exit(1);
    }
}
