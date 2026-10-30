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

//! Linux firmware service and isolated ownership for individual tool commands.

#![cfg_attr(not(test), deny(dead_code_pub_in_binary))]

#[cfg(target_os = "linux")]
use std::path::PathBuf;
#[cfg(target_os = "linux")]
use std::sync::Arc;

#[cfg(target_os = "linux")]
use clap::{Parser, Subcommand};
#[cfg(target_os = "linux")]
use tracing_subscriber::layer::SubscriberExt;
#[cfg(target_os = "linux")]
use tracing_subscriber::util::SubscriberInitExt;

// Source and transport targets can contain credential-bearing URLs. Daemon
// diagnostics deliberately admit only the operation layer's sanitized context.
#[cfg(target_os = "linux")]
const SERVICE_LOG_FILTER: &str = "off,libmlx::firmware::operation=info";

#[cfg(target_os = "linux")]
#[path = "../runner/supervisor.rs"]
mod supervisor;
#[cfg(target_os = "linux")]
#[path = "../runner/supervisor_protocol.rs"]
mod supervisor_protocol;

#[cfg(target_os = "linux")]
#[derive(Debug, Parser)]
#[command(
    name = "mlx-firmware-supervisor",
    about = "Serve local firmware operations, or run the private single-command worker when no arguments are supplied."
)]
struct Arguments {
    #[command(subcommand)]
    command: Option<Command>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, PartialEq, Eq, Subcommand)]
enum Command {
    #[command(about = "Serve firmware operations on a private local Unix socket.")]
    Serve {
        #[arg(
            long,
            value_name = "PCI_ADDRESS",
            help = "PCI address dddd:bb:ss.f, or bb:ss.f for domain zero. MST aliases are unsupported."
        )]
        device: String,
        #[arg(
            long,
            value_name = "ROOT",
            help = "Absolute shared journal root. It must be owned by the effective user with mode 0700 and trusted, non-symlink ancestors. Runtime storage can disappear on reboot. Journals retained on persistent storage remain readable after reboot but reject further mutation."
        )]
        state_directory: PathBuf,
    },
}

/// Run the requested per-card service, or the single-command worker with no arguments.
fn main() -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        match Arguments::parse().command {
            Some(Command::Serve {
                device,
                state_directory,
            }) => {
                // Set process-wide creation permissions before a runtime can
                // start threads. Individual journal/socket modes remain explicit.
                nix::sys::stat::umask(
                    nix::sys::stat::Mode::S_IRWXG | nix::sys::stat::Mode::S_IRWXO,
                );
                initialize_service_logging()?;
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?
                    .block_on(libmlx::firmware::operation::serve(
                        &device,
                        &state_directory,
                    ))
            }
            None => run_command_worker(),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "firmware process supervision requires Linux",
        ))
    }
}

/// Keep service failures on private stderr without enabling raw source or transport logs.
#[cfg(target_os = "linux")]
fn initialize_service_logging() -> std::io::Result<()> {
    service_logging(|| Box::new(std::io::stderr()))
        .try_init()
        .map_err(std::io::Error::other)
}

/// Build a logfmt subscriber that admits only sanitized operation diagnostics.
#[cfg(target_os = "linux")]
fn service_logging(
    writer: impl Fn() -> Box<dyn std::io::Write> + Send + Sync + 'static,
) -> impl tracing::Subscriber + Send + Sync {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new(SERVICE_LOG_FILTER))
        .with(logfmt::layer().with_writer(Arc::new(writer)))
}

/// Run the single-threaded subreaper without introducing an asynchronous runtime.
#[cfg(target_os = "linux")]
fn run_command_worker() -> std::io::Result<()> {
    // This branch must remain single-threaded: its subreaper owns every tool
    // descendant and uses /proc/thread-self/children to locate them.
    worker_result(supervisor::run())
}

#[cfg(target_os = "linux")]
fn worker_result(result: std::io::Result<()>) -> std::io::Result<()> {
    match result {
        // A caller can disconnect before dispatch or while the tool runs.
        // No tool starts before a complete request; once dispatched, bounded
        // cleanup finishes before the helper attempts to write its result.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::BrokenPipe | std::io::ErrorKind::UnexpectedEof
            ) =>
        {
            Ok(())
        }
        result => result,
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use carbide_test_support::Outcome::{Fails, FailsWith, Yields};
    use carbide_test_support::scenarios;

    use super::*;

    #[test]
    fn worker_result_preserves_timeout_failures() {
        use std::io::{Error, ErrorKind};

        scenarios!(run = |result: std::io::Result<()>| worker_result(result)
            .map_err(|error| (error.kind(), error.to_string()));
            "finished work and disconnected callers succeed" {
                Ok(()) => Yields(()),
                Err(Error::from(ErrorKind::BrokenPipe)) => Yields(()),
                Err(Error::from(ErrorKind::UnexpectedEof)) => Yields(()),
            }
            "protocol timeouts retain their failure and diagnostic" {
                Err(Error::new(ErrorKind::TimedOut, "supervisor deadline expired"))
                    => FailsWith((ErrorKind::TimedOut, "supervisor deadline expired".to_string())),
            }
        );
    }

    /// Incomplete service requests must fail parsing rather than enter the private worker.
    #[test]
    fn no_arguments_preserve_worker_mode_and_service_requires_explicit_paths() {
        scenarios!(run = |arguments: Vec<&str>| Arguments::try_parse_from(arguments)
            .map(|arguments| arguments.command).map_err(drop);
            "worker and service dispatch" {
                vec!["mlx-firmware-supervisor"] => Yields(None),
                vec!["mlx-firmware-supervisor", "serve", "--device", "0000:ab:00.0", "--state-directory", "/run/forge/mlx-firmware"] => Yields(Some(Command::Serve {
                    device: "0000:ab:00.0".into(),
                    state_directory: "/run/forge/mlx-firmware".into(),
                })),
            }
            "incomplete service arguments cannot enter worker mode" {
                vec!["mlx-firmware-supervisor", "serve", "--device", "0000:ab:00.0"] => Fails,
                vec!["mlx-firmware-supervisor", "serve", "--state-directory", "/run/forge/mlx-firmware"] => Fails,
                vec!["mlx-firmware-supervisor", "unknown"] => Fails,
            }
        );
    }

    /// Service diagnostics use searchable logfmt fields without exposing source
    /// URLs or transport credentials.
    #[test]
    fn service_logging_preserves_structured_fields_and_excludes_secrets() {
        let output = tempfile::NamedTempFile::new().expect("create service log fixture");
        let writer = output.reopen().expect("open service log writer");
        let subscriber = service_logging(move || {
            Box::new(writer.try_clone().expect("clone service log writer"))
        });
        tracing::subscriber::with_default(subscriber, || {
            tracing::error!(target: "libmlx::firmware::source", url = "https://source.invalid/?token=private-source", "raw source");
            tracing::error!(target: "reqwest", credential = "private-transport", "raw transport");
            tracing::warn!(
                target: "libmlx::firmware::operation::service",
                operation_id = "receipt-id",
                operation_phase = "Burn",
                "safe operation diagnostic"
            );
        });
        let observed = std::fs::read_to_string(output.path()).expect("read service log fixture");
        assert_eq!(observed.lines().count(), 1);
        assert!(observed.starts_with("level=WARN "));
        assert!(observed.contains("msg=\"safe operation diagnostic\""));
        assert!(observed.contains("operation_id=receipt-id"));
        assert!(observed.contains("operation_phase=Burn"));
        assert!(!observed.contains("private-source"));
        assert!(!observed.contains("private-transport"));
    }
}
