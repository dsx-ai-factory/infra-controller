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

//! /v1 connections the probe can close at shutdown. A request cut short
//! still has to send its reply, and a client that stops reading (an HTTP/1
//! client that never drains its socket, an HTTP/2 client with a zero
//! window) would hold its connection, and the server's graceful shutdown,
//! open. Once the listener's token fires, every read and write of its
//! connections fails, so each connection ends.

use std::io;
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};

/// A TCP listener whose connections all close once `close` fires.
pub(crate) struct ClosableListener {
    inner: TcpListener,
    close: CancellationToken,
}

impl ClosableListener {
    pub(crate) fn new(inner: TcpListener, close: CancellationToken) -> Self {
        Self { inner, close }
    }
}

impl axum::serve::Listener for ClosableListener {
    type Io = ClosableStream;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        let (io, addr) = axum::serve::Listener::accept(&mut self.inner).await;
        (ClosableStream::new(io, &self.close), addr)
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.inner.local_addr()
    }
}

/// A connection whose reads and writes fail once its token fires. Reads and
/// writes each wait on the token, so a blocked one wakes when it fires.
pub(crate) struct ClosableStream {
    io: TcpStream,
    read_closed: Pin<Box<WaitForCancellationFutureOwned>>,
    write_closed: Pin<Box<WaitForCancellationFutureOwned>>,
}

impl ClosableStream {
    fn new(io: TcpStream, close: &CancellationToken) -> Self {
        Self {
            io,
            read_closed: Box::pin(close.clone().cancelled_owned()),
            write_closed: Box::pin(close.clone().cancelled_owned()),
        }
    }
}

fn closed() -> io::Error {
    io::Error::new(
        io::ErrorKind::ConnectionAborted,
        "the probe is shutting down",
    )
}

impl AsyncRead for ClosableStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.read_closed.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed()));
        }
        Pin::new(&mut this.io).poll_read(cx, buf)
    }
}

impl AsyncWrite for ClosableStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.write_closed.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed()));
        }
        Pin::new(&mut this.io).poll_write(cx, buf)
    }

    fn poll_write_vectored(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if this.write_closed.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed()));
        }
        Pin::new(&mut this.io).poll_write_vectored(cx, bufs)
    }

    fn is_write_vectored(&self) -> bool {
        self.io.is_write_vectored()
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.write_closed.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed()));
        }
        Pin::new(&mut this.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if this.write_closed.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(closed()));
        }
        Pin::new(&mut this.io).poll_shutdown(cx)
    }
}
