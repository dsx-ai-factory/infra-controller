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
/*

/// admin_cli.rs
///
/// General utility code for working with and displaying data
/// with the admin CLI.

*/

pub use output::OutputFormat;

pub mod output {
    use std::fmt;

    use clap::ValueEnum;

    #[derive(Default, PartialEq, Eq, ValueEnum, Clone, Copy, Debug)]
    #[clap(rename_all = "kebab_case")]
    pub enum OutputFormat {
        #[default]
        AsciiTable,
        Csv,
        Json,
        Yaml,
    }

    impl fmt::Display for OutputFormat {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                OutputFormat::AsciiTable => write!(f, "ASCII table output format"),
                OutputFormat::Csv => write!(f, "CSV output format"),
                OutputFormat::Json => write!(f, "JSON output format"),
                OutputFormat::Yaml => write!(f, "YAML output format"),
            }
        }
    }
}
