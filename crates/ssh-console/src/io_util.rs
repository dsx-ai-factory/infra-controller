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

use std::os::fd::{AsFd, OwnedFd, RawFd};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::{OpenptyResult, openpty};
use nix::sys::termios::{Termios, cfmakeraw};
use nix::unistd;
use tokio::io::unix::AsyncFd;

/// Allocate a pty with `nix::pty::openpty`, ensuring its file descriptors are set with O_NONBLOCK,
/// and return it.
pub(crate) fn alloc_pty(cols: u16, rows: u16) -> Result<OpenptyResult, PtyAllocError> {
    // set up raw mode so the child sees a “dumb” terminal
    let libc_termios = libc::termios {
        c_iflag: 0,
        c_oflag: 0,
        c_cflag: 0,
        c_lflag: 0,
        c_line: 0,
        c_cc: [0; 32],
        c_ispeed: 0,
        c_ospeed: 0,
    };
    let mut termios = Termios::from(libc_termios);
    cfmakeraw(&mut termios);

    // set initial window-size
    let winsz = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };

    let pty = openpty(Some(&winsz), Some(&termios)).map_err(|error| PtyAllocError::Io {
        what: "opening PTY",
        error: std::io::Error::from_raw_os_error(error as _),
    })?;

    set_nonblocking(&pty.master).map_err(|error| PtyAllocError::Io {
        what: "making PTY master fd non-blocking",
        error: std::io::Error::from_raw_os_error(error as _),
    })?;
    set_nonblocking(&pty.slave).map_err(|error| PtyAllocError::Io {
        what: "making PTY slave fd non-blocking",
        error: std::io::Error::from_raw_os_error(error as _),
    })?;

    Ok(pty)
}

#[derive(thiserror::Error, Debug)]
pub(crate) enum PtyAllocError {
    #[error("error {what}: {error}")]
    Io {
        error: std::io::Error,
        what: &'static str,
    },
}

/// Make `pty_slave` the controlling terminal for the given command when it is executed.
pub(crate) fn set_controlling_terminal_on_exec(
    command: &mut tokio::process::Command,
    pty_slave: RawFd,
) {
    // SAFETY: the closure captures only a copied descriptor that remains owned through `spawn`
    // and makes direct `setsid` and `TIOCSCTTY` syscalls without allocating, locking, or accessing
    // shared Rust state between `fork` and `exec`.
    unsafe {
        command.pre_exec(move || {
            unistd::setsid()?;
            // https://man7.org/linux/man-pages/man2/TIOCSCTTY.2const.html
            if libc::ioctl(pty_slave, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

/// Set the O_NONBLOCK flag on a file descriptor.
///
/// This is essential to allow reading/writing to the fd from a tokio context, otherwise it will
/// block the runtime.
fn set_nonblocking<F: AsFd>(fd: &F) -> nix::Result<()> {
    let current_flags = fcntl(fd, FcntlArg::F_GETFL)?;
    let flags_with_nonblock = OFlag::from_bits_truncate(current_flags) | OFlag::O_NONBLOCK;
    fcntl(fd, FcntlArg::F_SETFL(flags_with_nonblock))?;
    Ok(())
}

/// Waits for the fd to be ready for writing, and writes the data to it, looping repeatedly until
/// all data is written.
pub(crate) async fn write_data_to_async_fd(
    data: &[u8],
    fd: &AsyncFd<OwnedFd>,
) -> std::io::Result<usize> {
    let mut written = 0;
    loop {
        let mut guard = fd.writable().await?;
        match unistd::write(fd, &data[written..]) {
            Ok(0) => {
                // EOF
                break;
            }
            Ok(n) => {
                written += n;
                if written >= data.len() {
                    break;
                }
            }
            Err(e) if e == Errno::EWOULDBLOCK => {
                // no data, await writable() again
                guard.clear_ready();
            }
            Err(e) => return Err(std::io::Error::from_raw_os_error(e as _)),
        }
    }
    Ok(written)
}
