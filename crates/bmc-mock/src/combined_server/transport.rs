// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Per-connection transport control: cancellation below Hyper, blocked-write
//! accounting, and the response extension through which a handler requests an
//! output-progress bound on its body.
//!
//! Hyper stops polling a body once HTTP/2 flow control or its own write buffer
//! is exhausted, so a body-level timer alone cannot end a stalled response.
//! Closing the accepted transport can, at the cost of every other HTTP/2
//! stream on that connection. Other connections are unaffected.

use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;
use futures::future::BoxFuture;
use hyper::body::Incoming;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::watch;
use tokio::time::Instant;
use tokio_util::sync::{CancellationToken, WaitForCancellationFutureOwned};
use tower::Service;

use super::progress::ProgressBody;

/// Response extension: close the serving connection when an emitted body frame
/// waits this long for Hyper to poll again, or when a transport write stays
/// blocked this long without any later write or flush completing. Idle waits
/// between frames do not count. See `combined_server::progress` for the two
/// detectors.
#[derive(Clone, Copy, Debug)]
pub(crate) struct OutputStallTimeout(pub(crate) Duration);

#[derive(Clone, Debug)]
pub(super) struct TransportHandle {
    cancel: CancellationToken,
    /// When the transport first refused output, until a flush completes.
    blocked: Arc<watch::Sender<Option<Instant>>>,
}

impl TransportHandle {
    pub(super) fn new() -> Self {
        Self {
            cancel: CancellationToken::new(),
            blocked: Arc::new(watch::channel(None).0),
        }
    }

    /// Observe when the transport started refusing output; `None` while it
    /// accepts writes. Hyper retries a write only after the socket signalled
    /// writable, so any completed write or flush after a `Pending` proves the
    /// kernel accepted bytes, however much TLS buffers in between.
    pub(super) fn blocked_since(&self) -> watch::Receiver<Option<Instant>> {
        self.blocked.subscribe()
    }

    /// Fail the connection's pending and future I/O with `TimedOut`.
    pub(super) fn close(&self) {
        self.cancel.cancel();
    }

    fn record_blocked(&self) {
        self.blocked
            .send_if_modified(|since| since.is_none() && since.replace(Instant::now()).is_none());
    }

    fn record_progress(&self) {
        self.blocked
            .send_if_modified(|since| since.take().is_some());
    }

    #[cfg(test)]
    pub(super) fn set_blocked(&self, blocked: bool) {
        if blocked {
            self.record_blocked();
        } else {
            self.record_progress();
        }
    }

    #[cfg(test)]
    pub(super) fn is_closed(&self) -> bool {
        self.cancel.is_cancelled()
    }
}

#[derive(Clone)]
pub(super) struct Acceptor<A> {
    inner: A,
}

impl<A> Acceptor<A> {
    pub(super) fn new(inner: A) -> Self {
        Self { inner }
    }
}

impl<A, I, S> axum_server::accept::Accept<I, S> for Acceptor<A>
where
    A: axum_server::accept::Accept<I, S>,
    A::Future: Send + 'static,
{
    type Stream = CancelledIo<A::Stream>;
    type Service = ConnectionService<A::Service>;
    type Future = BoxFuture<'static, io::Result<(Self::Stream, Self::Service)>>;

    fn accept(&self, io: I, service: S) -> Self::Future {
        let accept = self.inner.accept(io, service);
        Box::pin(async move {
            let (inner, service) = accept.await?;
            let handle = TransportHandle::new();
            Ok((
                CancelledIo {
                    inner,
                    read_cancel: Box::pin(handle.cancel.clone().cancelled_owned()),
                    write_cancel: Box::pin(handle.cancel.clone().cancelled_owned()),
                    handle: handle.clone(),
                },
                ConnectionService {
                    inner: service,
                    handle,
                },
            ))
        })
    }
}

pub(super) struct CancelledIo<I> {
    inner: I,
    read_cancel: Pin<Box<WaitForCancellationFutureOwned>>,
    write_cancel: Pin<Box<WaitForCancellationFutureOwned>>,
    handle: TransportHandle,
}

impl<I> CancelledIo<I> {
    /// Track whether the transport is refusing output.
    fn observe<T>(&self, result: &Poll<io::Result<T>>) {
        match result {
            Poll::Pending => self.handle.record_blocked(),
            Poll::Ready(Ok(_)) => self.handle.record_progress(),
            Poll::Ready(Err(_)) => {}
        }
    }
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "response output stalled")
}

impl<I: AsyncRead + Unpin> AsyncRead for CancelledIo<I> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if self.read_cancel.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(timed_out()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<I: AsyncWrite + Unpin> AsyncWrite for CancelledIo<I> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        if self.write_cancel.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(timed_out()));
        }
        let written = Pin::new(&mut self.inner).poll_write(cx, buf);
        self.observe(&written);
        written
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.write_cancel.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(timed_out()));
        }
        let flushed = Pin::new(&mut self.inner).poll_flush(cx);
        self.observe(&flushed);
        flushed
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        if self.write_cancel.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(timed_out()));
        }
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[io::IoSlice<'_>],
    ) -> Poll<io::Result<usize>> {
        if self.write_cancel.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(timed_out()));
        }
        let written = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        self.observe(&written);
        written
    }
}

/// Wraps the body of any response that asks for an [`OutputStallTimeout`].
#[derive(Clone)]
pub(super) struct ConnectionService<S> {
    inner: S,
    handle: TransportHandle,
}

impl<S> Service<Request<Incoming>> for ConnectionService<S>
where
    S: Service<Request<Incoming>, Response = Response>,
    S::Future: Send + 'static,
    S::Error: 'static,
{
    type Response = Response;
    type Error = S::Error;
    type Future = BoxFuture<'static, Result<Response, S::Error>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request<Incoming>) -> Self::Future {
        let response = self.inner.call(request);
        let handle = self.handle.clone();
        Box::pin(async move {
            let mut response = response.await?;
            if let Some(OutputStallTimeout(timeout)) =
                response.extensions().get::<OutputStallTimeout>().copied()
            {
                let body = std::mem::replace(response.body_mut(), Body::empty());
                *response.body_mut() = Body::new(ProgressBody::new(body, handle, timeout));
            }
            Ok(response)
        })
    }
}
