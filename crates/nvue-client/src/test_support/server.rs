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

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::response::Response;
use bytes::Bytes;
use http::StatusCode;
use tokio::net::UnixListener;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::{MockRequest, MockResponse, NvueMockHandler};
use crate::client::NvueServerAddress;

const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

/// An NVUE mock HTTP server bound to a temporary Unix socket.
///
/// Call [`Self::finish`] to shut the server down and verify every recorded
/// request. Dropping an armed server panics unless another panic is already in
/// progress.
///
/// Note that construction may fail with "Operation not permitted" inside of a
/// sandbox that restricts creating named Unix sockets.
pub(crate) struct MockNvueServer {
    state: Arc<ServerState>,
    socket_path: PathBuf,
    shutdown: Option<oneshot::Sender<()>>,
    task: Option<JoinHandle<io::Result<()>>>,
    _socket_dir: tempfile::TempDir,
    armed: bool,
}

impl MockNvueServer {
    /// Start a server using `handler` for every request.
    ///
    /// # Panics
    ///
    /// Panics when called outside a Tokio runtime.
    pub(crate) fn start(handler: impl NvueMockHandler) -> io::Result<Self> {
        let socket_dir = tempfile::Builder::new().prefix("nvue-mock-").tempdir()?;
        let socket_path = socket_dir.path().join("nvue.sock");
        let listener = UnixListener::bind(&socket_path)?;
        let state = Arc::new(ServerState {
            handler: Arc::new(handler),
            requests: Mutex::new(Vec::new()),
            failures: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .fallback(handle_request)
            .with_state(Arc::clone(&state));
        let (shutdown, shutdown_receiver) = oneshot::channel();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(async move {
                    let _ = shutdown_receiver.await;
                })
                .await
        });

        Ok(Self {
            state,
            socket_path,
            shutdown: Some(shutdown),
            task: Some(task),
            _socket_dir: socket_dir,
            armed: true,
        })
    }

    /// Return the bound Unix socket path.
    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Return an address suitable for constructing an [`crate::NvueClient`].
    pub(crate) fn server_address(&self) -> NvueServerAddress {
        NvueServerAddress::UnixSocket {
            socket_path: self.socket_path.clone(),
        }
    }

    /// Return a snapshot of every recorded request in arrival order.
    pub(crate) fn requests(&self) -> Vec<MockRequest> {
        self.state
            .requests
            .lock()
            .expect("request lock should work")
            .clone()
    }

    /// Return recorded requests as `METHOD URI` summaries in arrival order.
    pub(crate) fn summarize_requests(&self) -> Vec<String> {
        self.requests()
            .into_iter()
            .map(|request| format!("{method} {uri}", method = request.method, uri = request.uri,))
            .collect()
    }

    /// Shut down the server, join its task, and verify all recorded behavior.
    ///
    /// Calling this method disarms the drop panic even when verification fails.
    pub(crate) async fn finish(mut self) -> Result<(), String> {
        self.signal_shutdown();

        let task_result = self
            .task
            .as_mut()
            .expect("armed server should own a task")
            .await;
        self.armed = false;

        let mut failures = self.state.take_failures();
        failures.extend(self.state.handler.verify());
        match task_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => failures.push(format!("server task failed: {error}")),
            Err(error) => failures.push(format!("server task failed: {error}")),
        }

        if failures.is_empty() {
            Ok(())
        } else {
            Err(format_failures(&failures))
        }
    }

    fn signal_shutdown(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }
}

impl Drop for MockNvueServer {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        self.signal_shutdown();
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if !std::thread::panicking() {
            panic!("MockNvueServer dropped without calling finish()");
        }
    }
}

struct ServerState {
    handler: Arc<dyn NvueMockHandler>,
    requests: Mutex<Vec<MockRequest>>,
    failures: Mutex<Vec<String>>,
}

impl ServerState {
    fn record_request(&self, request: MockRequest) {
        self.requests
            .lock()
            .expect("request lock should work")
            .push(request);
    }

    fn record_failure(&self, failure: String) {
        self.failures
            .lock()
            .expect("failure lock should work")
            .push(failure);
    }

    fn take_failures(&self) -> Vec<String> {
        std::mem::take(&mut *self.failures.lock().expect("failure lock should work"))
    }
}

async fn handle_request(State(state): State<Arc<ServerState>>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let request_target = format!("{method} {uri}", method = parts.method, uri = parts.uri,);
    let body = match to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(body) => body,
        Err(error) => {
            state.record_request(MockRequest::from_parts(
                parts.method,
                parts.uri,
                parts.headers,
                Bytes::new(),
            ));
            state.record_failure(format!(
                "request body exceeded the {MAX_REQUEST_BODY_BYTES}-byte limit for {request_target}: {error}"
            ));
            return into_response(MockResponse::empty(StatusCode::PAYLOAD_TOO_LARGE));
        }
    };
    let request = MockRequest::from_parts(parts.method, parts.uri, parts.headers, body);
    state.record_request(request.clone());

    match state.handler.handle(&request) {
        Some(response) => into_response(response),
        None => {
            state.record_failure(format!("unhandled request: {request_target}"));
            into_response(MockResponse::bytes(
                StatusCode::INTERNAL_SERVER_ERROR,
                "unhandled mock request",
            ))
        }
    }
}

fn into_response(response: MockResponse) -> Response {
    let mut http_response = Response::new(Body::from(response.body));
    *http_response.status_mut() = response.status;
    *http_response.headers_mut() = response.headers;
    http_response
}

fn format_failures(failures: &[String]) -> String {
    let failures = failures
        .iter()
        .map(|failure| format!("- {failure}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("NVUE mock verification failed:\n{failures}")
}

#[cfg(test)]
mod tests {
    use std::any::Any;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    use http::{Method, StatusCode};
    use serde_json::json;

    use super::*;
    use crate::test_support::handler_fn;

    fn unix_client(socket_path: &Path) -> reqwest::Client {
        reqwest::Client::builder()
            .unix_socket(socket_path)
            .build()
            .expect("Unix socket client should build")
    }

    #[tokio::test]
    async fn serves_records_and_explicitly_shuts_down() {
        let server = MockNvueServer::start(handler_fn(|request| {
            if request.matches(Method::POST, "/echo") {
                Some(MockResponse::json(
                    StatusCode::CREATED,
                    &json!({"received": request.body.len()}),
                ))
            } else {
                None
            }
        }))
        .expect("mock server should start");
        let socket_path = server.socket_path().to_path_buf();
        let client = unix_client(&socket_path);

        let response = client
            .post("http://nvue.test/echo?source=test")
            .header("x-test", "recorded")
            .body("request body")
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response
                .json::<serde_json::Value>()
                .await
                .expect("response should contain JSON"),
            json!({"received": 12})
        );

        let requests = server.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, Method::POST);
        assert_eq!(requests[0].uri.path(), "/echo");
        assert_eq!(requests[0].uri.query(), Some("source=test"));
        assert_eq!(requests[0].headers["x-test"], "recorded");
        assert_eq!(requests[0].body, Bytes::from_static(b"request body"));
        assert!(matches!(
            server.server_address(),
            NvueServerAddress::UnixSocket { .. }
        ));

        server
            .finish()
            .await
            .expect("mock server should finish cleanly");
        assert!(!socket_path.exists());
    }

    struct VerificationFailures;

    impl NvueMockHandler for VerificationFailures {
        fn handle(&self, _: &MockRequest) -> Option<MockResponse> {
            None
        }

        fn verify(&self) -> Vec<String> {
            vec![
                "first handler verification failure".to_string(),
                "second handler verification failure".to_string(),
            ]
        }
    }

    #[tokio::test]
    async fn aggregates_unhandled_requests_and_handler_verification_failures() {
        let server = MockNvueServer::start(VerificationFailures).expect("mock server should start");
        let client = unix_client(server.socket_path());

        for path in ["/first", "/second"] {
            let response = client
                .get(format!("http://nvue.test{path}"))
                .send()
                .await
                .expect("request should complete");
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        }

        let error = server.finish().await.expect_err("verification should fail");
        for expected in [
            "unhandled request: GET /first",
            "unhandled request: GET /second",
            "first handler verification failure",
            "second handler verification failure",
        ] {
            assert!(
                error.contains(expected),
                "missing {expected:?} in {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn rejects_and_reports_oversized_request_bodies() {
        let server =
            MockNvueServer::start(handler_fn(|_| Some(MockResponse::empty(StatusCode::OK))))
                .expect("mock server should start");
        let client = unix_client(server.socket_path());

        let response = client
            .post("http://nvue.test/large")
            .body(vec![b'x'; MAX_REQUEST_BODY_BYTES + 1])
            .send()
            .await
            .expect("request should complete");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

        let error = server.finish().await.expect_err("verification should fail");
        assert!(error.contains("request body exceeded the 1048576-byte limit for POST /large"));
    }

    #[tokio::test]
    async fn propagates_server_task_failure() {
        let server = MockNvueServer::start(handler_fn(|_| None)).expect("mock server should start");
        server
            .task
            .as_ref()
            .expect("server should own a task")
            .abort();

        let error = server.finish().await.expect_err("joining should fail");
        assert!(error.contains("server task failed:"));
        assert!(error.contains("cancelled"));
    }

    #[tokio::test]
    async fn dropping_without_finish_panics_after_cleanup() {
        let server = MockNvueServer::start(handler_fn(|_| None)).expect("mock server should start");
        let socket_path = server.socket_path().to_path_buf();

        let panic = catch_unwind(AssertUnwindSafe(|| drop(server)))
            .expect_err("dropping an armed server should panic");
        assert_eq!(
            panic_message(panic.as_ref()),
            Some("MockNvueServer dropped without calling finish()")
        );
        assert!(!socket_path.exists());
    }

    #[tokio::test]
    async fn dropping_during_an_existing_panic_does_not_replace_it() {
        let server = MockNvueServer::start(handler_fn(|_| None)).expect("mock server should start");

        let panic = catch_unwind(AssertUnwindSafe(|| {
            let _server = server;
            panic!("outer panic");
        }))
        .expect_err("outer panic should be caught");
        assert_eq!(panic_message(panic.as_ref()), Some("outer panic"));
    }

    fn panic_message(panic: &(dyn Any + Send)) -> Option<&str> {
        panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
    }
}
