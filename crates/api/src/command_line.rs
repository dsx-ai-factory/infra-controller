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

use clap::{ArgAction, Parser};

#[derive(Parser)]
#[clap(name = "carbide-api")]
pub struct Options {
    #[clap(long, default_value = "false", help = "Print version number and exit")]
    pub version: bool,

    #[clap(short, long, action = ArgAction::Count)]
    pub debug: u8,

    #[clap(subcommand)]
    pub sub_cmd: Option<Command>,
}

#[derive(Parser)]
pub enum Command {
    #[clap(about = "Performs database migrations")]
    Migrate(Migrate),

    #[clap(about = "Run the API service")]
    Run(Box<Daemon>),
}

#[derive(Parser)]
pub struct Daemon {
    /// Path to the configuration file
    /// The contents of this configuration file can be patched by providing
    /// site specific configuration overrides via an additional config file at
    /// `site-config-path`.
    /// Additionally all configuration file contents can be overridden using
    /// environmental variables that are prefixed with `CARBIDE_API_`.
    /// E.g. an environmental variable with the name `CARBIDE_API_DATABASE_URL`
    /// will take precedence over the field `database_url` in the site specific
    /// configuration. And the field `database_url` in the site specific configuration
    /// will take precedence over the same field in the global configuration.
    #[clap(long)]
    pub config_path: PathBuf,
    /// Path to the configuration file which contains per-site overwrites
    #[clap(long)]
    pub site_config_path: Option<PathBuf>,
}

#[derive(Parser)]
pub struct Migrate {
    #[clap(long, require_equals(true), env = "DATABASE_URL")]
    pub datastore: String,
}

impl Options {
    pub fn load() -> Self {
        Self::parse()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use carbide_test_support::Outcome::*;
    use carbide_test_support::scenarios;
    use clap::{CommandFactory, Parser};

    use super::{Command, Options};

    #[derive(Debug, PartialEq)]
    struct ParsedOptions {
        version: bool,
        debug: u8,
        command: ParsedCommand,
    }

    #[derive(Debug, PartialEq)]
    enum ParsedCommand {
        None,
        Migrate {
            datastore: String,
        },
        Run {
            config_path: PathBuf,
            site_config_path: Option<PathBuf>,
        },
    }

    #[test]
    fn command_name_is_stable() {
        assert_eq!(Options::command().get_name(), "carbide-api");
    }

    #[test]
    fn parses_command_line_variants() {
        scenarios!(run = |args: Vec<&str>| {
            Options::try_parse_from(args)
                .map(|options| ParsedOptions {
                    version: options.version,
                    debug: options.debug,
                    command: match options.sub_cmd {
                        None => ParsedCommand::None,
                        Some(Command::Migrate(migrate)) => ParsedCommand::Migrate {
                            datastore: migrate.datastore,
                        },
                        Some(Command::Run(run)) => ParsedCommand::Run {
                            config_path: run.config_path,
                            site_config_path: run.site_config_path,
                        },
                    },
                })
                .map_err(drop)
        };
            "valid global options" {
                vec!["carbide-api", "--version", "-ddd"] => Yields(ParsedOptions {
                    version: true,
                    debug: 3,
                    command: ParsedCommand::None,
                }),
            }

            "valid subcommands" {
                vec![
                    "carbide-api",
                    "run",
                    "--config-path",
                    "/etc/nico/config.toml",
                    "--site-config-path",
                    "/etc/nico/site.toml",
                ] => Yields(ParsedOptions {
                    version: false,
                    debug: 0,
                    command: ParsedCommand::Run {
                        config_path: PathBuf::from("/etc/nico/config.toml"),
                        site_config_path: Some(PathBuf::from("/etc/nico/site.toml")),
                    },
                }),
                vec![
                    "carbide-api",
                    "migrate",
                    "--datastore=postgres://localhost/carbide",
                ] => Yields(ParsedOptions {
                    version: false,
                    debug: 0,
                    command: ParsedCommand::Migrate {
                        datastore: "postgres://localhost/carbide".to_string(),
                    },
                }),
            }

            "invalid subcommands" {
                vec!["carbide-api", "run"] => Fails,
                vec![
                    "carbide-api",
                    "migrate",
                    "--datastore",
                    "postgres://localhost/carbide",
                ] => Fails,
            }
        );
    }
}
