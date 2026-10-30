// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Unix-socket credentials and exact process-exit evidence for the submitting peer.
//!
//! `SO_PEERPIDFD` requires Linux 6.5 or a kernel with that feature backported.
//! Failure to obtain it leaves Continue available; Terminate admission must reject
//! the missing monitor with FailedPrecondition. A numeric PID is not a fallback.
//! The pidfd identifies the process that connected the socket, so clients must
//! connect in the submitting process rather than inherit another process's socket.

use std::io;
use std::os::fd::{AsFd, OwnedFd};
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::sys::socket::{getsockopt, sockopt};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, Interest, ReadBuf};
use tokio::net::UnixStream;
use tokio::sync::OwnedSemaphorePermit;
use tonic::transport::server::Connected;

/// Kernel-provided identity captured when accepting a connected Unix socket.
#[derive(Clone, Debug)]
pub(super) struct PeerInfo {
    /// UID to authenticate before accepting a request from this connection.
    pub(super) uid: u32,
    /// Exact connecting process, independent of connection closure or PID reuse.
    pub(super) pidfd: Option<Arc<OwnedFd>>,
    /// Why process-exit monitoring is unavailable; Continue does not require it.
    pub(super) pidfd_error: Option<Arc<io::Error>>,
}

/// A Unix stream that attaches captured peer identity to each Tonic request.
#[derive(Debug)]
pub(super) struct ConnectedSocket {
    stream: UnixStream,
    peer: PeerInfo,
    _connection_permit: OwnedSemaphorePermit,
}

impl ConnectedSocket {
    /// Capture the peer UID and optional pidfd before serving any requests.
    /// Credential failure rejects the connection; pidfd failure remains visible
    /// in its metadata so admission can reject only policies needing that proof.
    pub(super) fn new(
        stream: UnixStream,
        connection_permit: OwnedSemaphorePermit,
    ) -> io::Result<Self> {
        let uid = stream.peer_cred()?.uid();
        let (pidfd, pidfd_error) = match getsockopt(&stream, sockopt::PeerPidfd) {
            Ok(fd) => (Some(Arc::new(fd)), None),
            Err(error) => (None, Some(Arc::new(io::Error::from(error)))),
        };
        Ok(Self {
            stream,
            peer: PeerInfo {
                uid,
                pidfd,
                pidfd_error,
            },
            _connection_permit: connection_permit,
        })
    }
}

impl Connected for ConnectedSocket {
    type ConnectInfo = PeerInfo;

    fn connect_info(&self) -> Self::ConnectInfo {
        self.peer.clone()
    }
}

impl AsyncRead for ConnectedSocket {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buffer)
    }
}

impl AsyncWrite for ConnectedSocket {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, buffer)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

/// Wait for this exact peer process to exit, without reading or reaping it.
/// The wait intentionally has no deadline while its owning job needs the monitor.
/// Dropping the future releases its duplicate descriptor and reactor registration.
pub(super) async fn wait_for_exit(fd: Arc<OwnedFd>) -> io::Result<()> {
    // try_clone duplicates with CLOEXEC atomically. Plain dup followed by fcntl
    // could leak a monitor into a concurrently spawned firmware tool.
    let descriptor = fd.try_clone()?;
    let flags = OFlag::from_bits_truncate(fcntl(&descriptor, FcntlArg::F_GETFL)?);
    fcntl(&descriptor, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
    let observer = AsyncFd::with_interest(descriptor, Interest::READABLE)?;
    loop {
        let mut ready = observer.readable().await?;
        let mut descriptors = [PollFd::new(observer.get_ref().as_fd(), PollFlags::POLLIN)];
        match poll(&mut descriptors, PollTimeout::ZERO) {
            Ok(_) => {
                let events = descriptors[0].revents().unwrap_or(PollFlags::empty());
                if events.intersects(PollFlags::POLLERR | PollFlags::POLLNVAL) {
                    return Err(io::Error::other(
                        "observer pidfd reported invalid readiness",
                    ));
                }
                if events.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
                    return Ok(());
                }
                // Reactor readiness may be spurious. Confirm the exit on the
                // same pidfd rather than interpreting readiness alone as death.
                ready.clear_ready();
            }
            Err(Errno::EINTR) => continue,
            Err(error) => return Err(error.into()),
        }
    }
}
