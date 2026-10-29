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

//! One tool, its descendants, and their output belong to this fresh process.
//!
//! There is one thread and one reaper. `ECHILD` proves that all owned
//! descendants have finished; a directory scan can never supply that proof. The
//! embedding application neither adopts these children nor installs a reaper.

use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FdFlag, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::prctl::set_child_subreaper;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, kill, sigaction};
use nix::sys::wait::{Id, WaitPidFlag, WaitStatus, waitid, waitpid};
use nix::unistd::{Pid, dup};

use super::supervisor_protocol::{
    CLEANUP_TIMEOUT, Failure, HELLO, OUTPUT_LIMIT, REQUEST_LIMIT, RESPONSE_LIMIT, RESULT_TIMEOUT,
    Request, Response, ToolOwnerExitBehavior,
};

const POLL_INTERVAL: Duration = Duration::from_millis(10);
// Bound an unused helper even if a live caller never finishes dispatching.
// The caller's attempt deadline can cancel this wait earlier by closing stdin.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

pub(super) fn run() -> io::Result<()> {
    // SIG_IGN survives exec and makes Linux discard child exit statuses. This
    // fresh, single-threaded process must own waitable children before dispatch.
    // SAFETY: no signal callback is installed, and no tool has been spawned.
    unsafe {
        sigaction(
            Signal::SIGCHLD,
            &SigAction::new(SigHandler::SigDfl, SaFlags::empty(), SigSet::empty()),
        )?;
    }
    // Only this disposable process adopts orphan descendants. In particular,
    // enabling subreaping inside Scout would capture unrelated subprocesses.
    set_child_subreaper(true)?;
    // Cancellation needs this proc entry to find signal candidates. Reject a
    // Linux environment without it before accepting any tool request.
    drop(File::open("/proc/thread-self/children").map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "cannot open /proc/thread-self/children: {error}; firmware supervision requires \
                 CONFIG_PROC_CHILDREN=y and readable procfs mounted for the helper's PID namespace"
            ),
        )
    })?);
    // Unbuffered descriptor owners avoid stdio retaining a request prefix or
    // a response tail outside the bounded protocol operations below.
    let mut control = File::from(dup(io::stdin())?);
    let mut result = File::from(dup(io::stdout())?);
    fcntl(&control, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    fcntl(&result, FcntlArg::F_SETFD(FdFlag::FD_CLOEXEC))?;
    nonblocking(&control)?;
    nonblocking(&result)?;
    write_until(&mut result, HELLO, Instant::now() + REQUEST_TIMEOUT)?;
    let request = read_request(&mut control, REQUEST_TIMEOUT)?;
    let response = supervise(request, &mut control);
    write_response(&mut result, &response, RESULT_TIMEOUT)
}

fn write_response(result: &mut File, response: &Response, timeout: Duration) -> io::Result<()> {
    let bytes = serde_json::to_vec(response).map_err(io::Error::other)?;
    if bytes.len() > RESPONSE_LIMIT {
        return Err(io::Error::other("supervisor response exceeded its bound"));
    }
    // A completed helper also bounds delivery if its caller stops reading.
    let deadline = Instant::now() + timeout;
    write_until(result, &(bytes.len() as u32).to_be_bytes(), deadline)?;
    write_until(result, &bytes, deadline)
}

fn read_request(control: &mut File, timeout: Duration) -> io::Result<Request> {
    let deadline = Instant::now() + timeout;
    let mut length = [0; 4];
    read_until(control, &mut length, deadline)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > REQUEST_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("supervisor request exceeds {} KiB", REQUEST_LIMIT / 1024),
        ));
    }
    let mut bytes = vec![0; length];
    read_until(control, &mut bytes, deadline)?;
    serde_json::from_slice(&bytes)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

fn supervise(request: Request, control: &mut File) -> Response {
    let Some(deadline) = Instant::now().checked_add(request.timeout) else {
        return Response::NotStarted(
            io::Error::new(io::ErrorKind::InvalidInput, "invalid attempt deadline").into(),
        );
    };
    if request.timeout.is_zero() {
        return Response::NotStarted(
            io::Error::new(io::ErrorKind::TimedOut, "attempt deadline already expired").into(),
        );
    }
    let mut child = match Command::new(request.program)
        .args(request.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return Response::NotStarted(error.into()),
    };
    let leader = Pid::from_raw(child.id() as i32);
    let mut stdout = OutputPipe::new(child.stdout.take().expect("stdout was piped"));
    let mut stderr = OutputPipe::new(child.stderr.take().expect("stderr was piped"));
    // std::process::Child has no background reaper. All waits, including the
    // leader's status, stay with the loop below.
    drop(child);
    let mut status = None;
    let mut reason = None;
    let mut cleanup_errors = Vec::new();
    for (configuration, finished) in [
        (nonblocking(&stdout.pipe), &mut stdout.finished),
        (nonblocking(&stderr.pipe), &mut stderr.finished),
    ] {
        if let Err(error) = configuration {
            // Never enter a potentially blocking read after setup failed.
            // These streams cannot supply a confirmed output-drain result.
            *finished = true;
            reason.get_or_insert_with(|| Failure::Io(error.into()));
            cleanup_errors.push(
                io::Error::other("tool output could not be collected without blocking").into(),
            );
        }
    }
    let mut cleanup_deadline = reason.as_ref().map(|_| Instant::now() + CLEANUP_TIMEOUT);
    let mut control_open = true;

    loop {
        // Limit work per turn so a noisy tool or continual forking cannot
        // prevent the deadline and cancellation checks from running.
        let finished = match reap_children(leader, &mut status) {
            Ok(finished) => finished,
            Err(error) => {
                start_cleanup(
                    &mut reason,
                    &mut cleanup_deadline,
                    Failure::Io(error.into()),
                );
                false
            }
        };
        for error in [stdout.drain(), stderr.drain()].into_iter().flatten() {
            if reason.is_some() {
                if let Failure::Io(error) = error {
                    cleanup_errors.push(error);
                }
            } else {
                start_cleanup(&mut reason, &mut cleanup_deadline, error);
            }
        }
        if finished && stdout.finished && stderr.finished {
            break;
        }
        if reason.is_none() {
            if control_open {
                let mut cancellation = [0; 1];
                match control.read(&mut cancellation) {
                    // Accepted work can outlive its owner, but the tool still
                    // has the deadline and output limits from its request.
                    Ok(0) if request.owner_exit_behavior == ToolOwnerExitBehavior::Continue => {
                        control_open = false;
                    }
                    Ok(_) => {
                        start_cleanup(&mut reason, &mut cleanup_deadline, Failure::Cancelled);
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(error) => start_cleanup(
                        &mut reason,
                        &mut cleanup_deadline,
                        Failure::Io(error.into()),
                    ),
                }
            }
            if Instant::now() >= deadline {
                start_cleanup(&mut reason, &mut cleanup_deadline, Failure::Deadline);
            }
        }
        if let Some(cleanup_deadline) = cleanup_deadline {
            if let Err(error) = terminate_children(cleanup_deadline) {
                // Repeated failures need not grow the result without bound.
                // The first error already explains why cleanup is uncertain.
                if cleanup_errors.is_empty() {
                    cleanup_errors.push(error.into());
                }
            }
            if Instant::now() >= cleanup_deadline {
                cleanup_errors.push(
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!(
                            "descendant termination and output drain exceeded {CLEANUP_TIMEOUT:?}"
                        ),
                    )
                    .into(),
                );
                break;
            }
        }
        // A closed pipe always reports HUP, even with an empty interest mask.
        // Omit it so waiting for an adopted child does not busy-loop.
        let mut descriptors = Vec::with_capacity(3);
        if reason.is_none() && control_open {
            descriptors.push(PollFd::new(control.as_fd(), PollFlags::POLLIN));
        }
        if !stdout.finished {
            descriptors.push(PollFd::new(stdout.pipe.as_fd(), PollFlags::POLLIN));
        }
        if !stderr.finished {
            descriptors.push(PollFd::new(stderr.pipe.as_fd(), PollFlags::POLLIN));
        }
        if let Err(error) = poll(
            &mut descriptors,
            PollTimeout::try_from(POLL_INTERVAL).expect("10ms fits poll timeout"),
        ) && error != Errno::EINTR
        {
            start_cleanup(
                &mut reason,
                &mut cleanup_deadline,
                Failure::Io(io::Error::from(error).into()),
            );
        }
    }
    match (reason, status) {
        (None, Some(status)) => Response::Finished {
            status,
            stdout: stdout.retained,
            stderr: stderr.retained,
        },
        (reason, status) => Response::Failed {
            status,
            stdout: stdout.retained,
            stderr: stderr.retained,
            reason: reason.unwrap_or_else(|| {
                Failure::Io(io::Error::other("missing tool exit status").into())
            }),
            cleanup_errors,
        },
    }
}

fn start_cleanup(reason: &mut Option<Failure>, deadline: &mut Option<Instant>, failure: Failure) {
    if reason.is_none() {
        *reason = Some(failure);
        *deadline = Some(Instant::now() + CLEANUP_TIMEOUT);
    }
}

fn reap_children(leader: Pid, leader_status: &mut Option<i32>) -> io::Result<bool> {
    // The parent reconstructs ExitStatus::from_raw: Linux stores normal exit
    // codes in bits 8..15 and terminating signals/core-dump flags in bits 0..7.
    for _ in 0..64 {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(pid, status)) => {
                if pid == leader {
                    *leader_status = Some(status << 8);
                }
            }
            Ok(WaitStatus::Signaled(pid, signal, core_dumped)) => {
                if pid == leader {
                    *leader_status = Some(signal as i32 | if core_dumped { 0x80 } else { 0 });
                }
            }
            Ok(WaitStatus::StillAlive) => return Ok(false),
            Ok(_) | Err(Errno::EINTR) => {}
            Err(Errno::ECHILD) => return Ok(true),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(false)
}

fn terminate_children(deadline: Instant) -> io::Result<()> {
    // Cap this proc-file read at 1 MiB independently of tool output, preventing
    // unbounded allocation while finding signal candidates. Only waitpid proves
    // completion, not the list's size or contents.
    const MAX_CHILD_LIST_BYTES: usize = 1024 * 1024;
    let mut children = String::new();
    File::open("/proc/thread-self/children")?
        .take((MAX_CHILD_LIST_BYTES + 1) as u64)
        .read_to_string(&mut children)?;
    if children.len() > MAX_CHILD_LIST_BYTES {
        return Err(io::Error::other(format!(
            "owned child list exceeded {} MiB",
            MAX_CHILD_LIST_BYTES / (1024 * 1024)
        )));
    }
    for child in children.split_ascii_whitespace() {
        check_deadline(deadline)?;
        let pid = Pid::from_raw(
            child
                .parse()
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
        );
        // The proc list selects signal candidates, not completion. Confirm
        // ownership without reaping before signalling: this sole reaper pins
        // the PID even if the child exits between waitid and kill.
        match waitid(
            Id::Pid(pid),
            WaitPidFlag::WEXITED | WaitPidFlag::WNOHANG | WaitPidFlag::WNOWAIT,
        ) {
            Ok(WaitStatus::StillAlive) => match kill(pid, Signal::SIGKILL) {
                Ok(()) | Err(Errno::ESRCH) => {}
                Err(error) => return Err(error.into()),
            },
            Ok(_) | Err(Errno::ECHILD) | Err(Errno::EINTR) => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

struct OutputPipe<T> {
    pipe: T,
    retained: Vec<u8>,
    finished: bool,
    exceeded: bool,
}

impl<T: Read> OutputPipe<T> {
    fn new(pipe: T) -> Self {
        Self {
            pipe,
            retained: Vec::new(),
            finished: false,
            exceeded: false,
        }
    }

    fn drain(&mut self) -> Option<Failure> {
        if self.finished {
            return None;
        }
        let mut buffer = [0; 8192];
        for _ in 0..64 {
            match self.pipe.read(&mut buffer) {
                Ok(0) => {
                    self.finished = true;
                    return None;
                }
                Ok(length) => {
                    let keep = length.min(OUTPUT_LIMIT - self.retained.len());
                    self.retained.extend_from_slice(&buffer[..keep]);
                    if keep < length && !self.exceeded {
                        self.exceeded = true;
                        return Some(Failure::OutputLimit);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return None,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => {
                    self.finished = true;
                    return Some(Failure::Io(error.into()));
                }
            }
        }
        None
    }
}

fn nonblocking(descriptor: &impl AsFd) -> io::Result<()> {
    let flags = OFlag::from_bits_truncate(fcntl(descriptor, FcntlArg::F_GETFL)?);
    fcntl(descriptor, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    Ok(())
}

fn read_until(
    reader: &mut (impl Read + AsFd),
    mut bytes: &mut [u8],
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        check_deadline(deadline)?;
        match reader.read(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "supervisor control closed before dispatch",
                ));
            }
            Ok(length) => bytes = &mut bytes[length..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_ready(reader, PollFlags::POLLIN, deadline)?
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn write_until(
    writer: &mut (impl Write + AsFd),
    mut bytes: &[u8],
    deadline: Instant,
) -> io::Result<()> {
    while !bytes.is_empty() {
        check_deadline(deadline)?;
        match writer.write(bytes) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "supervisor result write made no progress",
                ));
            }
            Ok(length) => bytes = &bytes[length..],
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                wait_ready(writer, PollFlags::POLLOUT, deadline)?
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn wait_ready(descriptor: &impl AsFd, flags: PollFlags, deadline: Instant) -> io::Result<()> {
    check_deadline(deadline)?;
    let remaining = deadline.saturating_duration_since(Instant::now());
    let mut descriptors = [PollFd::new(descriptor.as_fd(), flags)];
    match poll(
        &mut descriptors,
        PollTimeout::try_from(remaining.min(POLL_INTERVAL)).expect("10ms fits poll timeout"),
    ) {
        Ok(_) | Err(Errno::EINTR) => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn check_deadline(deadline: Instant) -> io::Result<()> {
    if Instant::now() >= deadline {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "supervisor deadline expired",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;

    use nix::unistd::pipe2;

    use super::*;

    const IO_TIMEOUT: Duration = Duration::from_millis(100);
    const WATCHDOG: Duration = Duration::from_secs(10);

    /// A partial frame must consume the request deadline while its writer stays open.
    #[test]
    fn stalled_request_expires_without_eof() {
        let (reader, writer) = pipe2(OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).unwrap();
        let mut reader = File::from(reader);
        let mut writer = File::from(writer);
        writer.write_all(&100_u32.to_be_bytes()).unwrap();
        writer.write_all(b"{").unwrap();

        let (finished, completion) = mpsc::sync_channel(1);
        let started = Instant::now();
        let worker = thread::spawn(move || {
            finished
                .send(read_request(&mut reader, IO_TIMEOUT).map(|_| ()))
                .unwrap();
        });
        let result = completion.recv_timeout(WATCHDOG);
        // If the watchdog fired, closing the writer lets `read_request` observe
        // EOF. Join the worker before reporting the watchdog failure.
        drop(writer);
        worker.join().expect("join request reader");
        let error = result
            .expect("request read finishes within its independent watchdog")
            .expect_err("incomplete request must expire");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "supervisor deadline expired");
        assert!(started.elapsed() >= IO_TIMEOUT);
    }

    /// A result larger than the pipe must expire without reader progress.
    #[test]
    fn blocked_result_delivery_expires_without_reader_progress() {
        let (reader, writer) = pipe2(OFlag::O_CLOEXEC | OFlag::O_NONBLOCK).unwrap();
        let mut reader = File::from(reader);
        let mut writer = File::from(writer);
        fcntl(&reader, FcntlArg::F_SETPIPE_SZ(4096)).expect("reduce result pipe capacity");
        let capacity = usize::try_from(fcntl(&reader, FcntlArg::F_GETPIPE_SZ).unwrap()).unwrap();
        let response = Response::Finished {
            status: 0,
            stdout: vec![b'0'; capacity + 1],
            stderr: Vec::new(),
        };
        let expected = serde_json::to_vec(&response).unwrap();

        let (finished, completion) = mpsc::sync_channel(1);
        let started = Instant::now();
        let worker = thread::spawn(move || {
            finished
                .send(write_response(&mut writer, &response, IO_TIMEOUT))
                .unwrap();
        });
        let result = match completion.recv_timeout(WATCHDOG) {
            Ok(result) => result,
            Err(error) => {
                // Closing the reader makes a blocked write return `BrokenPipe`,
                // so we can join the worker before reporting the failed deadline.
                drop(reader);
                worker.join().expect("join stalled result writer");
                panic!("result delivery exceeded its independent watchdog: {error}");
            }
        };
        worker.join().expect("join result writer");
        let error = result.expect_err("unread result must expire");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert_eq!(error.to_string(), "supervisor deadline expired");
        let elapsed = started.elapsed();
        assert!(elapsed >= IO_TIMEOUT);
        assert!(
            elapsed < Duration::from_secs(3),
            "delivery must use its supplied timeout, not the production result timeout: {elapsed:?}"
        );

        let mut frame = Vec::new();
        reader.read_to_end(&mut frame).unwrap();
        let length = u32::from_be_bytes(frame[..4].try_into().unwrap()) as usize;
        assert_eq!(length, expected.len());
        assert!(length > capacity, "result exceeds measured pipe capacity");
        assert!(frame.len() - 4 < length, "delivery remains incomplete");
        assert_eq!(&frame[4..], &expected[..frame.len() - 4]);
    }
}
