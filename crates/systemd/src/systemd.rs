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

use std::env;
#[cfg(target_os = "linux")]
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixDatagram};

use eyre::WrapErr;
use tokio::net::UnixDatagram as TokioUnixDatagram;

/// Tell systemd we have started
pub async fn notify_start() -> eyre::Result<()> {
    sd_notify("READY=1\n").await
}

/// Tell systemd we are still alive.
/// We must do this at least every WatchdogSec or else systemd will us SIGABRT and restart us.
pub async fn notify_watchdog() -> eyre::Result<()> {
    if env::var("WATCHDOG_USEC").is_err() {
        tracing::trace!("systemd watchdog disabled");
        return Ok(());
    }
    sd_notify("WATCHDOG=1\n").await
}

/// Tell systemd we are stopping
pub async fn notify_stop() -> eyre::Result<()> {
    sd_notify("STOPPING=1\n").await
}

async fn sd_notify(msg: &str) -> eyre::Result<()> {
    #[cfg_attr(not(target_os = "linux"), allow(unused_mut))]
    let mut sock_path = match env::var("NOTIFY_SOCKET") {
        Ok(path) if !path.is_empty() => path,
        _ => {
            tracing::trace!("Not started by systemd, skip sd_notify");
            return Ok(());
        }
    };

    let sock = UnixDatagram::unbound()?;
    sock.set_nonblocking(true)?;
    let addr = if sock_path.as_bytes()[0] == b'@' {
        #[cfg(target_os = "linux")]
        {
            // Linux abstract socket names must begin with a NUL byte.
            // SAFETY: this branch established that the first byte is ASCII `@`. Replacing it
            // with NUL preserves the UTF-8 invariant required by `String::as_mut_vec`.
            unsafe {
                sock_path.as_mut_vec()[0] = 0;
            }
            SocketAddr::from_abstract_name(sock_path.as_bytes())
                .wrap_err_with(|| format!("invalid abstract socket name {sock_path}"))?
        }
        #[cfg(not(target_os = "linux"))]
        {
            eyre::bail!(
                "abstract unix sockets (NOTIFY_SOCKET starting with @) are only supported on linux"
            );
        }
    } else {
        SocketAddr::from_pathname(&sock_path)
            .wrap_err_with(|| format!("invalid socket name {sock_path}"))?
    };
    sock.connect_addr(&addr)?;
    // Convert it to a tokio socket because we want this to be stuck if tokio's
    // epoll / mio reactor is stuck.
    let tokio_sock = TokioUnixDatagram::from_std(sock)?;
    let sent = tokio_sock
        .send(msg.as_bytes())
        .await
        .wrap_err("socket send error")?;
    if sent != msg.len() {
        eyre::bail!("short send {sent} / {}", msg.len());
    }
    Ok(())
}
