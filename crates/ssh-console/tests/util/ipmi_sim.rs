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
use std::os::fd::{AsRawFd, OwnedFd};
use std::time::Duration;

use eyre::Context;
use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::openpty;
use nix::unistd;
use tokio::io::unix::AsyncFd;

pub(super) type IpmiSimHandle = bmc_mock::ipmi_sim::IpmiSimHandle;

pub(crate) struct ActiveSolSession {
    _ipmitool: tokio::process::Child,
    _pty_master: AsyncFd<OwnedFd>,
}

impl ActiveSolSession {
    pub(crate) async fn assert_console_works(&self, expected_prompt: &[u8]) -> eyre::Result<()> {
        const PROBE: &[u8] = b"original-sol-owner-probe\r";

        tokio::time::timeout(Duration::from_secs(10), async {
            let mut written = 0;
            while written < PROBE.len() {
                let mut guard = self._pty_master.writable().await?;
                match unistd::write(&self._pty_master, &PROBE[written..]) {
                    Ok(0) => return Err(eyre::eyre!("conflicting SOL session PTY closed")),
                    Ok(n) => written += n,
                    Err(Errno::EWOULDBLOCK) => guard.clear_ready(),
                    Err(error) => return Err(error.into()),
                }
            }

            let mut output = Vec::new();
            let mut buf = [0; 1024];
            loop {
                let mut guard = self._pty_master.readable().await?;
                match unistd::read(guard.get_inner(), &mut buf) {
                    Ok(0) | Err(Errno::EIO) => {
                        return Err(eyre::eyre!(
                            "conflicting SOL session closed while probing it: {}",
                            String::from_utf8_lossy(&output)
                        ));
                    }
                    Ok(n) => {
                        output.extend_from_slice(&buf[..n]);
                        if output
                            .windows(PROBE.len())
                            .position(|window| window == PROBE)
                            .is_some_and(|probe_start| {
                                output[probe_start + PROBE.len()..]
                                    .windows(expected_prompt.len())
                                    .any(|window| window == expected_prompt)
                            })
                        {
                            return Ok::<(), eyre::Report>(());
                        }
                    }
                    Err(Errno::EWOULDBLOCK) => guard.clear_ready(),
                    Err(error) => return Err(error.into()),
                }
            }
        })
        .await
        .context("timed out probing the original conflicting SOL session")?
    }
}

pub(crate) async fn activate_sol(port: u16) -> eyre::Result<ActiveSolSession> {
    let pty = openpty(None, None).context("failed to allocate ipmitool pty")?;
    set_nonblocking(&pty.master).context("failed to make ipmitool pty nonblocking")?;

    let mut command = tokio::process::Command::new("ipmitool");
    command
        .arg("-I")
        .arg("lanplus")
        .arg("-H")
        .arg("127.0.0.1")
        .arg("-p")
        .arg(port.to_string())
        .arg("-U")
        .arg("root")
        .arg("-P")
        .arg("password")
        .arg("-C")
        .arg("3")
        .arg("sol")
        .arg("activate")
        .stdin(pty.slave.try_clone().context("clone pty for stdin")?)
        .stdout(pty.slave.try_clone().context("clone pty for stdout")?)
        .stderr(pty.slave.try_clone().context("clone pty for stderr")?)
        .kill_on_drop(true);

    let pty_slave_fd = pty.slave.as_raw_fd();
    // SAFETY: the closure captures only a copied descriptor that remains owned through `spawn`
    // and makes direct `setsid` and `TIOCSCTTY` syscalls without allocating, locking, or accessing
    // shared Rust state between `fork` and `exec`.
    unsafe {
        command.pre_exec(move || {
            unistd::setsid()?;
            if libc::ioctl(pty_slave_fd, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let ipmitool = command
        .spawn()
        .context("failed to start conflicting SOL session")?;
    drop(command);
    drop(pty.slave);
    let pty_master = AsyncFd::new(pty.master).context("failed to register ipmitool pty")?;

    tokio::time::timeout(Duration::from_secs(10), async {
        let mut output = Vec::new();
        let mut buf = [0; 1024];
        loop {
            let mut guard = pty_master.readable().await?;
            match unistd::read(guard.get_inner(), &mut buf) {
                Ok(0) | Err(Errno::EIO) => {
                    return Err(eyre::eyre!(
                        "ipmitool exited before activating SOL: {}",
                        String::from_utf8_lossy(&output)
                    ));
                }
                Ok(n) => {
                    output.extend_from_slice(&buf[..n]);
                    if output
                        .windows(b"SOL Session operational".len())
                        .any(|window| window == b"SOL Session operational")
                    {
                        return Ok::<(), eyre::Report>(());
                    }
                }
                Err(Errno::EWOULDBLOCK) => guard.clear_ready(),
                Err(error) => return Err(error.into()),
            }
        }
    })
    .await
    .context("timed out waiting for the conflicting SOL session to activate")??;

    Ok(ActiveSolSession {
        _ipmitool: ipmitool,
        _pty_master: pty_master,
    })
}

fn set_nonblocking(fd: &OwnedFd) -> nix::Result<()> {
    let current_flags = fcntl(fd, FcntlArg::F_GETFL)?;
    fcntl(
        fd,
        FcntlArg::F_SETFL(OFlag::from_bits_truncate(current_flags) | OFlag::O_NONBLOCK),
    )?;
    Ok(())
}

/// Run an instance of ipmi_sim and a corresponding instance of a mock serial console, for tests to
/// use. Accepts a `prompt` parameter which will be echoed back when the clients send data (for
/// tests to assert that it's the expected host), and the machine-owned controller that supplies
/// simulated boot output.
pub(super) async fn run(
    prompt: String,
    console_output: machine_a_tron::ConsoleOutputController,
) -> eyre::Result<IpmiSimHandle> {
    let bmc = bmc_mock::test_support::generic_supermicro_bmc().await;
    bmc.state
        .account_service_state
        .change_factory_default_password("password");
    let console_output = Box::new(move || console_output.stream());
    bmc_mock::ipmi_sim::start(
        &bmc.state,
        bmc_mock::ipmi_sim::IpmiSimConfig {
            stable_id: prompt.clone(),
            console_prompt: prompt,
        },
        Some(console_output),
    )
    .await
    .map_err(Into::into)
}
