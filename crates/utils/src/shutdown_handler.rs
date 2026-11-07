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

use std::fmt::Display;
use std::io;

use tokio_util::sync::CancellationToken;

/// Start a task that will wait for SIGINT or SIGTERM, returning a CancellationToken which will be
/// cancelled when either of them occur.
pub fn start() -> io::Result<CancellationToken> {
    // Create a cancellation token that will be signalled when a sigterm/sigint is handled
    let cancel_token = CancellationToken::new();

    // Register the signals before returning
    let signal_future = shutdown_signal()?;

    tokio::spawn({
        let cancel_token = cancel_token.clone();
        async move {
            tokio::select! {
                _ = cancel_token.cancelled() => {}
                signal = signal_future => {
                    tracing::info!(%signal, "Shutdown signal received");
                    cancel_token.cancel();
                }
            }
        }
    });

    Ok(cancel_token)
}

enum ShutdownCause {
    Int,
    Term,
}

impl Display for ShutdownCause {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShutdownCause::Int => write!(f, "SIGINT"),
            ShutdownCause::Term => write!(f, "SIGTERM"),
        }
    }
}

fn shutdown_signal() -> io::Result<impl Future<Output = ShutdownCause>> {
    use tokio::signal::unix;

    // Register the signals before returning
    let mut terminate = unix::signal(unix::SignalKind::terminate())?;
    let mut interrupt = unix::signal(unix::SignalKind::interrupt())?;

    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => ShutdownCause::Int,
            _ = terminate.recv() => ShutdownCause::Term,
        }
    })
}
