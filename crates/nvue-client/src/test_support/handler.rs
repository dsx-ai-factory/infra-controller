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

mod config_revision;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

pub(crate) use config_revision::ConfigRevisionHandler;
use tokio::sync::Notify;

use super::{MockRequest, MockResponse};

/// Synchronously handles and verifies NVUE mock requests.
pub(crate) trait NvueMockHandler: Send + Sync + 'static {
    /// Handle `request`, or return `None` to delegate it.
    fn handle(&self, request: &MockRequest) -> Option<MockResponse>;

    /// Return every verification failure accumulated by this handler.
    fn verify(&self) -> Vec<String> {
        Vec::new()
    }

    /// Add a handler that receives requests before this handler.
    ///
    /// Chaining this method gives the most recently added override first
    /// priority.
    fn with_override<H>(self, override_handler: H) -> impl NvueMockHandler
    where
        Self: Sized,
        H: NvueMockHandler,
    {
        OverrideHandler {
            override_handler,
            fallback_handler: self,
        }
    }

    /// Trigger a checkpoint when this handler first produces a response.
    ///
    /// Returns a wrapped handler with otherwise unchanged behavior.
    fn with_response_checkpoint(self) -> (impl NvueMockHandler, ResponseCheckpoint)
    where
        Self: Sized,
    {
        let state = Arc::new(ResponseCheckpointState {
            reached: AtomicBool::new(false),
            notification: Notify::new(),
        });
        let checkpoint = ResponseCheckpoint {
            state: Arc::clone(&state),
        };
        (
            ResponseCheckpointHandler {
                handler: self,
                state,
            },
            checkpoint,
        )
    }

    /// Count every response produced by this handler.
    ///
    /// Returns a wrapped handler with otherwise unchanged behavior and a
    /// tracker that can wait for a requested response count.
    fn with_response_checkpoints(self) -> (impl NvueMockHandler, ResponseCheckpoints)
    where
        Self: Sized,
    {
        let state = Arc::new(ResponseCheckpointsState {
            served: AtomicUsize::new(0),
            notification: Notify::new(),
        });
        let checkpoints = ResponseCheckpoints {
            state: Arc::clone(&state),
        };
        (
            ResponseCheckpointsHandler {
                handler: self,
                state,
            },
            checkpoints,
        )
    }
}

impl<H> NvueMockHandler for Arc<H>
where
    H: NvueMockHandler + ?Sized,
{
    fn handle(&self, request: &MockRequest) -> Option<MockResponse> {
        self.as_ref().handle(request)
    }

    fn verify(&self) -> Vec<String> {
        self.as_ref().verify()
    }
}

struct HandlerFn<F>(F);

impl<F> NvueMockHandler for HandlerFn<F>
where
    F: Fn(&MockRequest) -> Option<MockResponse> + Send + Sync + 'static,
{
    fn handle(&self, request: &MockRequest) -> Option<MockResponse> {
        (self.0)(request)
    }
}

/// Adapt a synchronous function or closure into an [`NvueMockHandler`].
pub(crate) fn handler_fn<F>(handler: F) -> impl NvueMockHandler
where
    F: Fn(&MockRequest) -> Option<MockResponse> + Send + Sync + 'static,
{
    HandlerFn(handler)
}

struct ResponseCheckpointState {
    reached: AtomicBool,
    notification: Notify,
}

/// Waits until an observed mock handler produces a response.
pub(crate) struct ResponseCheckpoint {
    state: Arc<ResponseCheckpointState>,
}

impl ResponseCheckpoint {
    /// Wait until the checkpoint is reached, or return immediately if it was
    /// already reached.
    pub(crate) async fn wait_until_reached(&self) {
        let notification = self.state.notification.notified();
        tokio::pin!(notification);
        let _ = notification.as_mut().enable();

        if !self.state.reached.load(Ordering::Acquire) {
            notification.await;
        }
    }
}

struct ResponseCheckpointHandler<H> {
    handler: H,
    state: Arc<ResponseCheckpointState>,
}

impl<H> NvueMockHandler for ResponseCheckpointHandler<H>
where
    H: NvueMockHandler,
{
    fn handle(&self, request: &MockRequest) -> Option<MockResponse> {
        let response = self.handler.handle(request);
        if response.is_some() {
            self.state.reached.store(true, Ordering::Release);
            self.state.notification.notify_waiters();
        }
        response
    }

    fn verify(&self) -> Vec<String> {
        self.handler.verify()
    }
}

struct ResponseCheckpointsState {
    served: AtomicUsize,
    notification: Notify,
}

/// Waits until an observed mock handler produces a requested number of responses.
pub(crate) struct ResponseCheckpoints {
    state: Arc<ResponseCheckpointsState>,
}

impl ResponseCheckpoints {
    /// Wait until at least `count` responses have been served.
    ///
    /// Returns immediately when `count` is zero or has already been reached.
    pub(crate) async fn wait_until_response(&self, count: usize) {
        if count == 0 {
            return;
        }

        loop {
            let notification = self.state.notification.notified();
            tokio::pin!(notification);
            let _ = notification.as_mut().enable();

            if self.state.served.load(Ordering::Acquire) >= count {
                return;
            }
            notification.await;
        }
    }
}

struct ResponseCheckpointsHandler<H> {
    handler: H,
    state: Arc<ResponseCheckpointsState>,
}

impl<H> NvueMockHandler for ResponseCheckpointsHandler<H>
where
    H: NvueMockHandler,
{
    fn handle(&self, request: &MockRequest) -> Option<MockResponse> {
        let response = self.handler.handle(request);
        if response.is_some() {
            self.state.served.fetch_add(1, Ordering::Release);
            self.state.notification.notify_waiters();
        }
        response
    }

    fn verify(&self) -> Vec<String> {
        self.handler.verify()
    }
}

struct RespondOnceHandler<H> {
    handler: H,
    responded: Mutex<bool>,
}

impl<H> NvueMockHandler for RespondOnceHandler<H>
where
    H: NvueMockHandler,
{
    fn handle(&self, request: &MockRequest) -> Option<MockResponse> {
        let mut responded = self
            .responded
            .lock()
            .expect("respond-once state lock should work");
        if *responded {
            return None;
        }

        let response = self.handler.handle(request);
        if response.is_some() {
            *responded = true;
        }
        response
    }

    fn verify(&self) -> Vec<String> {
        let mut failures = self.handler.verify();
        if !*self
            .responded
            .lock()
            .expect("respond-once state lock should work")
        {
            failures.push("respond-once handler did not produce a response".to_string());
        }
        failures
    }
}

/// Allow `handler` to produce exactly one response, then delegate requests.
pub(crate) fn respond_once<H>(handler: H) -> impl NvueMockHandler
where
    H: NvueMockHandler,
{
    RespondOnceHandler {
        handler,
        responded: Mutex::new(false),
    }
}

struct OverrideHandler<O, F> {
    override_handler: O,
    fallback_handler: F,
}

impl<O, F> NvueMockHandler for OverrideHandler<O, F>
where
    O: NvueMockHandler,
    F: NvueMockHandler,
{
    fn handle(&self, request: &MockRequest) -> Option<MockResponse> {
        self.override_handler
            .handle(request)
            .or_else(|| self.fallback_handler.handle(request))
    }

    fn verify(&self) -> Vec<String> {
        let mut failures = self.override_handler.verify();
        failures.extend(self.fallback_handler.verify());
        failures
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;
    use http::{Method, StatusCode};

    use super::*;

    #[test]
    fn arc_forwards_handler_calls() {
        let handler = Arc::new(handler_fn(|request| {
            if request.matches(Method::GET, "/ready") {
                Some(MockResponse::empty(StatusCode::NO_CONTENT))
            } else {
                None
            }
        }));
        let request = MockRequest::new(Method::GET, "/ready", Bytes::new());

        assert_eq!(
            handler.handle(&request),
            Some(MockResponse::empty(StatusCode::NO_CONTENT))
        );
    }

    #[test]
    fn overrides_delegate_and_most_recent_override_has_priority() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let base_calls = Arc::clone(&calls);
        let base = handler_fn(move |_| {
            base_calls
                .lock()
                .expect("calls lock should work")
                .push("base");
            Some(MockResponse::empty(StatusCode::OK))
        });
        let first_calls = Arc::clone(&calls);
        let first = handler_fn(move |request| {
            first_calls
                .lock()
                .expect("calls lock should work")
                .push("first");
            if request.matches(Method::GET, "/delegated") {
                Some(MockResponse::empty(StatusCode::CREATED))
            } else {
                None
            }
        });
        let last_calls = Arc::clone(&calls);
        let last = handler_fn(move |request| {
            last_calls
                .lock()
                .expect("calls lock should work")
                .push("last");
            if request.matches(Method::GET, "/priority") {
                Some(MockResponse::empty(StatusCode::ACCEPTED))
            } else {
                None
            }
        });
        let handler = base.with_override(first).with_override(last);

        let priority = MockRequest::new(Method::GET, "/priority", Bytes::new());
        assert_eq!(
            handler.handle(&priority),
            Some(MockResponse::empty(StatusCode::ACCEPTED))
        );
        assert_eq!(*calls.lock().expect("calls lock should work"), ["last"]);

        calls.lock().expect("calls lock should work").clear();
        let delegated = MockRequest::new(Method::GET, "/delegated", Bytes::new());
        assert_eq!(
            handler.handle(&delegated),
            Some(MockResponse::empty(StatusCode::CREATED))
        );
        assert_eq!(
            *calls.lock().expect("calls lock should work"),
            ["last", "first"]
        );
    }

    #[test]
    fn respond_once_composes_with_override() {
        let fallback = handler_fn(|request| {
            if request.matches(Method::GET, "/matching") {
                Some(MockResponse::empty(StatusCode::OK))
            } else {
                Some(MockResponse::empty(StatusCode::NO_CONTENT))
            }
        });
        let one_time = handler_fn(|request| {
            request
                .matches(Method::GET, "/matching")
                .then(|| MockResponse::empty(StatusCode::CREATED))
        });
        let handler = fallback.with_override(respond_once(one_time));

        let unrelated = MockRequest::new(Method::GET, "/unrelated", Bytes::new());
        assert_eq!(
            handler.handle(&unrelated),
            Some(MockResponse::empty(StatusCode::NO_CONTENT))
        );
        let matching = MockRequest::new(Method::GET, "/matching", Bytes::new());
        assert_eq!(
            handler.handle(&matching),
            Some(MockResponse::empty(StatusCode::CREATED))
        );
        assert_eq!(
            handler.handle(&matching),
            Some(MockResponse::empty(StatusCode::OK))
        );
        assert!(handler.verify().is_empty());
    }

    #[test]
    fn unused_respond_once_fails_verification() {
        let handler = respond_once(handler_fn(|_| None));

        assert_eq!(
            handler.verify(),
            ["respond-once handler did not produce a response"]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn response_checkpoint_delegates_without_reaching() {
        let fallback = handler_fn(|_| Some(MockResponse::empty(StatusCode::NO_CONTENT)));
        let (delegating, checkpoint) = handler_fn(|_| None).with_response_checkpoint();
        let handler = fallback.with_override(delegating);
        let request = MockRequest::new(Method::GET, "/delegated", Bytes::new());

        assert_eq!(
            handler.handle(&request),
            Some(MockResponse::empty(StatusCode::NO_CONTENT))
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                checkpoint.wait_until_reached()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn response_checkpoint_remembers_response_before_waiting() {
        let (handler, checkpoint) =
            handler_fn(|_| Some(MockResponse::empty(StatusCode::NO_CONTENT)))
                .with_response_checkpoint();
        let request = MockRequest::new(Method::GET, "/response", Bytes::new());

        assert_eq!(
            handler.handle(&request),
            Some(MockResponse::empty(StatusCode::NO_CONTENT))
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            checkpoint.wait_until_reached(),
        )
        .await
        .expect("checkpoint should remember the response");
    }

    #[tokio::test(start_paused = true)]
    async fn response_checkpoints_count_and_remember_responses() {
        let (handler, checkpoints) =
            handler_fn(|_| Some(MockResponse::empty(StatusCode::NO_CONTENT)))
                .with_response_checkpoints();
        let request = MockRequest::new(Method::GET, "/response", Bytes::new());

        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            checkpoints.wait_until_response(0),
        )
        .await
        .expect("zero responses should already be served");
        assert!(handler.handle(&request).is_some());
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            checkpoints.wait_until_response(1),
        )
        .await
        .expect("checkpoint should remember the first response");

        let wait_for_second = checkpoints.wait_until_response(2);
        tokio::pin!(wait_for_second);
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), wait_for_second.as_mut())
                .await
                .is_err()
        );
        assert!(handler.handle(&request).is_some());
        tokio::time::timeout(std::time::Duration::from_secs(1), wait_for_second)
            .await
            .expect("second response should reach the requested count");
    }

    #[tokio::test(start_paused = true)]
    async fn response_checkpoints_ignore_delegated_requests() {
        let fallback = handler_fn(|_| Some(MockResponse::empty(StatusCode::NO_CONTENT)));
        let (delegating, checkpoints) = handler_fn(|_| None).with_response_checkpoints();
        let handler = fallback.with_override(delegating);
        let request = MockRequest::new(Method::GET, "/delegated", Bytes::new());

        assert_eq!(
            handler.handle(&request),
            Some(MockResponse::empty(StatusCode::NO_CONTENT))
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_secs(1),
                checkpoints.wait_until_response(1)
            )
            .await
            .is_err()
        );
    }

    struct VerificationFailure;

    impl NvueMockHandler for VerificationFailure {
        fn handle(&self, _: &MockRequest) -> Option<MockResponse> {
            Some(MockResponse::empty(StatusCode::OK))
        }

        fn verify(&self) -> Vec<String> {
            vec!["wrapped handler verification failure".to_string()]
        }
    }

    #[test]
    fn respond_once_forwards_wrapped_verification_failures() {
        let handler = respond_once(VerificationFailure);
        let request = MockRequest::new(Method::GET, "/matching", Bytes::new());
        assert!(handler.handle(&request).is_some());

        assert_eq!(handler.verify(), ["wrapped handler verification failure"]);
    }

    #[test]
    fn response_checkpoint_forwards_wrapped_verification_failures() {
        let (handler, _checkpoint) = VerificationFailure.with_response_checkpoint();

        assert_eq!(handler.verify(), ["wrapped handler verification failure"]);
    }

    #[test]
    fn response_checkpoints_forward_wrapped_verification_failures() {
        let (handler, _checkpoints) = VerificationFailure.with_response_checkpoints();

        assert_eq!(handler.verify(), ["wrapped handler verification failure"]);
    }
}
