// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Output-progress deadline for one response body.
//!
//! Two independent stall detectors share one deadline, and whichever expires
//! first closes the serving transport connection:
//!
//! - **Body poll gap.** The clock starts when a nonempty frame is handed to
//!   Hyper and stops when Hyper polls for the next one. Hyper stops polling a
//!   body while HTTP/2 flow control withholds send credit, which no transport
//!   write can reveal. Hyper peeks at most one frame ahead, so a stalled stream
//!   still goes silent after the peek.
//! - **Blocked transport.** The clock starts when a transport write or flush
//!   returns `Pending` and stops when any later write or flush completes,
//!   which Hyper attempts only once the socket signalled writable. Hyper
//!   re-polls a body as soon as it has buffered a frame, so on HTTP/1.1 only
//!   the kernel refusing bytes reveals a reader that stopped consuming; a slow
//!   but steady reader keeps completing writes and is never closed.
//!
//! Waiting for a publication, heartbeat, or intentional script delay arms
//! neither clock. Neither clock can see below the kernel: a reader that stops
//! consuming is detected once the socket send buffer fills, so for small
//! heartbeat frames the latency is that buffer's size divided by the byte rate,
//! plus the timeout.

use std::pin::Pin;
use std::task::{Context, Poll, ready};
use std::time::Duration;

use axum::body::Body;
use bytes::Bytes;
use hyper::body::{Body as HttpBody, Frame, SizeHint};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio::time::Instant;

use super::transport::TransportHandle;

pub(super) struct ProgressBody {
    inner: Body,
    /// Deadline of the body poll gap, if a frame is in Hyper's hands.
    deadline: watch::Sender<Option<Instant>>,
    timeout: Duration,
    watchdog: JoinHandle<()>,
}

impl ProgressBody {
    pub(super) fn new(inner: Body, transport: TransportHandle, timeout: Duration) -> Self {
        let (deadline, mut poll_gap) = watch::channel(None::<Instant>);
        let watchdog = tokio::spawn(async move {
            let mut blocked = transport.blocked_since();
            loop {
                let gap = *poll_gap.borrow_and_update();
                let block = blocked.borrow_and_update().map(|since| since + timeout);
                let expires = match (gap, block) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                let timer = async {
                    match expires {
                        Some(at) => tokio::time::sleep_until(at).await,
                        None => std::future::pending().await,
                    }
                };
                tokio::select! {
                    biased;
                    changed = poll_gap.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    _ = blocked.changed() => {}
                    _ = timer => {
                        tracing::warn!(
                            ?timeout,
                            poll_gap = gap.is_some(),
                            transport_blocked = block.is_some(),
                            "response output stalled; closing the shared transport connection"
                        );
                        transport.close();
                        return;
                    }
                }
            }
        });
        Self {
            inner,
            deadline,
            timeout,
            watchdog,
        }
    }
}

impl Drop for ProgressBody {
    fn drop(&mut self) {
        self.watchdog.abort();
    }
}

impl HttpBody for ProgressBody {
    type Data = Bytes;
    type Error = axum::Error;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, axum::Error>>> {
        // Hyper asking for another frame ends the previous frame's poll gap.
        self.deadline
            .send_if_modified(|deadline| deadline.take().is_some());
        let frame = ready!(Pin::new(&mut self.inner).poll_frame(cx));
        let data = frame.as_ref().is_some_and(|result| {
            result
                .as_ref()
                .is_ok_and(|frame| frame.data_ref().is_some_and(|data| !data.is_empty()))
        });
        if data {
            let deadline = Instant::now() + self.timeout;
            self.deadline.send_replace(Some(deadline));
        }
        Poll::Ready(frame)
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use futures::FutureExt;
    use http_body_util::BodyExt;

    use super::*;

    const TIMEOUT: Duration = Duration::from_secs(60);

    fn body(transport: &TransportHandle) -> (ProgressBody, tokio::sync::mpsc::Sender<Bytes>) {
        let (sender, receiver) = tokio::sync::mpsc::channel::<Bytes>(1);
        let stream = futures::stream::unfold(receiver, |mut receiver| async move {
            receiver
                .recv()
                .await
                .map(|data| (Ok::<_, std::io::Error>(data), receiver))
        });
        (
            ProgressBody::new(Body::from_stream(stream), transport.clone(), TIMEOUT),
            sender,
        )
    }

    #[tokio::test(start_paused = true)]
    async fn poll_gap_and_blocked_transport_expire_independently() {
        check_cases_async(
            [
                Case {
                    scenario: "frame taken, never polled again: HTTP/2 flow control stall",
                    input: (false, false),
                    expect: Yields(true),
                },
                Case {
                    scenario: "polled again, transport unblocked: healthy or idle",
                    input: (true, false),
                    expect: Yields(false),
                },
                Case {
                    scenario: "polled again, transport blocked: kernel refusing bytes",
                    input: (true, true),
                    expect: Yields(true),
                },
            ],
            |(repoll, blocked)| async move {
                let transport = TransportHandle::new();
                let (mut body, sender) = body(&transport);
                sender.send(Bytes::from_static(b"first")).await.unwrap();
                body.frame().await.unwrap().unwrap();
                if repoll {
                    assert!(body.frame().now_or_never().is_none());
                }
                if blocked {
                    transport.set_blocked(true);
                }
                // Idle waiting, and intentional script delays, do not count.
                tokio::time::advance(TIMEOUT * 3).await;
                tokio::task::yield_now().await;
                Ok::<_, std::convert::Infallible>(transport.is_closed())
            },
        )
        .await;
    }

    #[tokio::test(start_paused = true)]
    async fn write_progress_clears_the_blocked_clock() {
        let transport = TransportHandle::new();
        let (mut body, sender) = body(&transport);
        sender.send(Bytes::from_static(b"first")).await.unwrap();
        body.frame().await.unwrap().unwrap();
        assert!(body.frame().now_or_never().is_none());
        transport.set_blocked(true);
        tokio::time::advance(TIMEOUT / 2).await;
        transport.set_blocked(false);
        tokio::time::advance(TIMEOUT).await;
        tokio::task::yield_now().await;
        assert!(
            !transport.is_closed(),
            "the kernel accepted the bytes in time"
        );
        transport.set_blocked(true);
        tokio::time::advance(TIMEOUT).await;
        tokio::task::yield_now().await;
        assert!(transport.is_closed(), "a later block starts a fresh clock");
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_body_cancels_its_watchdog_not_the_connection() {
        let transport = TransportHandle::new();
        let mut body = ProgressBody::new(Body::from("data"), transport.clone(), TIMEOUT);
        body.frame().await.unwrap().unwrap();
        drop(body);
        tokio::time::advance(TIMEOUT * 2).await;
        tokio::task::yield_now().await;
        assert!(!transport.is_closed());
    }
}
