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

//! Node-auth tokens for co-located services that hold no key (issue #355).
//!
//! On a DPU, only the dpu-agent holds the machine's private key; other NICo
//! pods (fmds, …) obtain bearer tokens from the agent's local API — the
//! `AgentLocal` gRPC service on a unix socket in the shared `/opt/forge`
//! directory — instead of mounting the key to do their own mTLS/minting.
//!
//! [`SocketTokenSource`] keeps a cached token fresh with a background task and
//! serves it synchronously from [`NodeTokenProvider::current`] on the request
//! path. Until the first successful fetch (e.g. the agent hasn't started or
//! registered yet), requests simply go out without a bearer header.

use std::sync::{Arc, RwLock, Weak};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hyper_util::rt::TokioIo;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

use crate::node_jwt::NodeTokenProvider;
use crate::protos::agent_local::GetNodeTokenRequest;
use crate::protos::agent_local::agent_local_client::AgentLocalClient;

/// Default path of the dpu-agent's local API socket. It lives in a dedicated
/// `run/` subdirectory of the shared `/opt/forge` credentials directory so a
/// containerized consumer can be given the socket without being given the
/// credentials beside it: the pod mounts `run/` and never the directory that
/// holds the machine key.
///
/// The mount can be read-only. Connecting does not write to the filesystem,
/// and Linux applies the read-only-mount check only to regular files,
/// directories and symlinks -- socket inodes are exempt -- so `connect(2)`
/// succeeds. Access is gated by the socket's own 0600 mode instead.
pub const DEFAULT_AGENT_LOCAL_SOCKET: &str = "/opt/forge/run/agent.sock";

/// The background task re-fetches when less than this long remains on the
/// cached token. The request path keeps serving the cached token down to
/// HALF this margin — the refresher normally replaces it well before that —
/// so a briefly-late refresh doesn't strip requests of their header.
const REFRESH_MARGIN_SECS: u64 = 60;

/// Delay between fetch attempts while the agent socket is absent or erroring,
/// and the per-attempt deadline for a fetch (connect + RPC) so a stalled
/// handshake can't wedge the refresh loop.
const RETRY_DELAY: Duration = Duration::from_secs(5);

pub struct SocketTokenSource {
    socket_path: String,
    cached: RwLock<Option<(String, u64)>>,
    /// Signalled when the last `Arc` is dropped.
    ///
    /// The weak reference alone is enough for the refresh loop to *finish*,
    /// but only at the top of its next iteration — which, on a healthy token,
    /// is minutes away. That leaves a task sleeping on a source nobody holds,
    /// which shows up as a runtime that will not quiesce in tests and as
    /// pointless wakeups in production.
    shutdown: Arc<tokio::sync::Notify>,
}

impl Drop for SocketTokenSource {
    fn drop(&mut self) {
        // `notify_one` leaves a permit behind when nothing is waiting yet, so
        // a drop that lands between the loop's iterations is still observed on
        // its next await rather than being lost.
        self.shutdown.notify_one();
    }
}

/// Manual impl so the cached bearer token (a live credential) never lands in
/// debug output of the client config, which is itself `Debug`-logged. Mirrors
/// the redaction on [`NodeJwtMinter`](crate::node_jwt::NodeJwtMinter).
impl std::fmt::Debug for SocketTokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SocketTokenSource")
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl SocketTokenSource {
    /// Creates the source and spawns its refresh loop on the current tokio
    /// runtime. The loop holds only a weak reference, and is woken on drop, so
    /// releasing the last `Arc` (and the clients built from it) stops it
    /// within moments rather than at its next scheduled refresh.
    #[must_use]
    pub fn spawn(socket_path: String) -> Arc<Self> {
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(Self {
            socket_path,
            cached: RwLock::new(None),
            shutdown: Arc::clone(&shutdown),
        });
        tokio::spawn(refresh_loop(Arc::downgrade(&source), shutdown));
        source
    }

    async fn fetch(socket_path: &str) -> Result<(String, u64), tonic::Status> {
        // Deadline over connect + RPC together: a peer that accepts the
        // connection but never answers must fail the attempt, not wedge the
        // refresh loop forever.
        tokio::time::timeout(RETRY_DELAY, async {
            let channel = connect_uds(socket_path)
                .await
                .map_err(|e| tonic::Status::unavailable(format!("agent local socket: {e}")))?;
            let response = AgentLocalClient::new(channel)
                .get_node_token(GetNodeTokenRequest {})
                .await?
                .into_inner();
            Ok((response.token, response.expires_at))
        })
        .await
        .map_err(|_| tonic::Status::deadline_exceeded("agent local API fetch timed out"))?
    }
}

impl NodeTokenProvider for SocketTokenSource {
    fn current(&self) -> Option<String> {
        let now = unix_now()?;
        // Recover from poisoning rather than treating it as "no token". A
        // fetched token is the only credential this source has — unlike the
        // minter, it cannot fall back to producing one — so `.ok()?` here
        // would strip the bearer header from every later request, with the
        // refresher below equally unable to repopulate the cache. The guarded
        // value is a plain `(String, u64)` that a panicking reader cannot have
        // left inconsistent. Matches `NodeJwtValidator`'s handling.
        self.cached
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .filter(|(_, expires_at)| *expires_at > now + REFRESH_MARGIN_SECS / 2)
            .map(|(token, _)| token.clone())
    }
}

async fn connect_uds(socket_path: &str) -> Result<Channel, tonic::transport::Error> {
    let socket_path = socket_path.to_owned();
    // The URI is required by the Endpoint API but unused for UDS connections.
    Endpoint::try_from("http://[::]:50051")?
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = socket_path.clone();
            async move {
                let stream = UnixStream::connect(path).await?;
                Ok::<_, std::io::Error>(TokioIo::new(stream))
            }
        }))
        .await
}

async fn refresh_loop(source: Weak<SocketTokenSource>, shutdown: Arc<tokio::sync::Notify>) {
    // Sleeping is where this loop spends almost all of its life — up to a
    // whole token lifetime — so that is where a drop has to be observable.
    // The fetch itself is already bounded by its own deadline, so it is left
    // to finish rather than being torn down mid-RPC.
    let sleep_or_shutdown = async |duration| {
        tokio::select! {
            () = shutdown.notified() => false,
            () = tokio::time::sleep(duration) => true,
        }
    };

    loop {
        // Re-upgrade each iteration so the loop exits once every user of the
        // source is gone, instead of pinning it alive forever.
        let Some(source) = source.upgrade() else {
            return;
        };
        let socket_path = source.socket_path.clone();
        match SocketTokenSource::fetch(&socket_path).await {
            Ok((token, expires_at)) => {
                *source
                    .cached
                    .write()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((token, expires_at));
                drop(source);
                let sleep_secs = unix_now()
                    .map(|now| {
                        expires_at
                            .saturating_sub(now)
                            .saturating_sub(REFRESH_MARGIN_SECS)
                    })
                    .unwrap_or(0)
                    .max(RETRY_DELAY.as_secs());
                if !sleep_or_shutdown(Duration::from_secs(sleep_secs)).await {
                    return;
                }
            }
            Err(status) => {
                tracing::debug!(
                    target: "node_auth",
                    socket = %socket_path,
                    %status,
                    "node-auth: could not fetch node token from agent local API; will retry"
                );
                drop(source);
                if !sleep_or_shutdown(RETRY_DELAY).await {
                    return;
                }
            }
        }
    }
}

fn unix_now() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .ok()
}

#[cfg(test)]
mod tests {
    use tokio_stream::wrappers::UnixListenerStream;
    use tonic::{Request, Response, Status};

    use super::*;
    use crate::protos::agent_local::GetNodeTokenResponse;
    use crate::protos::agent_local::agent_local_server::{AgentLocal, AgentLocalServer};

    struct FixedToken(String, u64);

    #[tonic::async_trait]
    impl AgentLocal for FixedToken {
        async fn get_node_token(
            &self,
            _request: Request<GetNodeTokenRequest>,
        ) -> Result<Response<GetNodeTokenResponse>, Status> {
            Ok(Response::new(GetNodeTokenResponse {
                token: self.0.clone(),
                expires_at: self.1,
            }))
        }
    }

    fn serve(socket: &std::path::Path, token: &str, expires_at: u64) {
        let listener = tokio::net::UnixListener::bind(socket).expect("bind uds");
        let service = AgentLocalServer::new(FixedToken(token.to_string(), expires_at));
        tokio::spawn(
            tonic::transport::Server::builder()
                .add_service(service)
                .serve_with_incoming(UnixListenerStream::new(listener)),
        );
    }

    async fn wait_for_token(source: &SocketTokenSource) -> Option<String> {
        for _ in 0..100 {
            if let Some(token) = source.current() {
                return Some(token);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        None
    }

    #[tokio::test]
    async fn fetches_token_from_agent_socket() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agent.sock");
        let expires_at = unix_now().expect("clock") + 300;
        serve(&socket, "the.node.token", expires_at);

        let source = SocketTokenSource::spawn(socket.to_string_lossy().into_owned());
        assert_eq!(
            wait_for_token(&source).await.as_deref(),
            Some("the.node.token")
        );
    }

    #[tokio::test]
    async fn missing_socket_yields_none_and_no_panic() {
        let source = SocketTokenSource::spawn("/nonexistent/agent.sock".to_string());
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(source.current().is_none());
    }

    #[tokio::test]
    async fn debug_output_redacts_the_cached_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agent.sock");
        serve(
            &socket,
            "super.secret.token",
            unix_now().expect("clock") + 300,
        );

        let source = SocketTokenSource::spawn(socket.to_string_lossy().into_owned());
        wait_for_token(&source).await.expect("token cached");

        // ForgeClientConfig is Debug-logged, so a cached token must never
        // appear in the provider's debug output.
        let rendered = format!("{source:?}");
        assert!(
            !rendered.contains("super.secret.token"),
            "cached token leaked into Debug: {rendered}"
        );
    }

    #[tokio::test]
    async fn expired_cached_token_is_not_served() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agent.sock");
        // The agent hands out a token that is already (nearly) expired.
        serve(&socket, "stale.token", unix_now().expect("clock"));

        let source = SocketTokenSource::spawn(socket.to_string_lossy().into_owned());
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(
            source.current().is_none(),
            "a token inside the refresh margin must not be presented"
        );
    }

    /// Fetching is the only way this source can ever hold a credential, so a
    /// poisoned cache must not become a permanent "no token" — that would
    /// strip the bearer header from every later request, with the refresher
    /// equally unable to recover.
    #[tokio::test]
    async fn a_poisoned_cache_still_serves_its_token() {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("agent.sock");
        let expires_at = unix_now().expect("clock") + 3600;
        serve(&socket, "token.after.poison", expires_at);

        let source = SocketTokenSource::spawn(socket.to_string_lossy().into_owned());
        assert_eq!(
            wait_for_token(&source).await.as_deref(),
            Some("token.after.poison"),
            "precondition: the source must hold a token before it is poisoned"
        );

        // Poison the lock the only way it can happen: panic while holding the
        // write guard. The panic hook is left alone — it is global, and
        // swapping it here would suppress the output of any test panicking
        // concurrently. The backtrace this prints is expected.
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = source.cached.write().expect("not yet poisoned");
            panic!("deliberate panic to poison the cache (expected)");
        }));
        assert!(poisoned.is_err(), "the panic should have been caught");
        assert!(
            source.cached.read().is_err(),
            "precondition: the lock must actually be poisoned"
        );

        assert_eq!(
            source.current().as_deref(),
            Some("token.after.poison"),
            "a poisoned cache must still serve the token it holds"
        );
    }

    /// Dropping the last `Arc` must stop the refresh task promptly, not at its
    /// next scheduled wake. With nothing listening the loop parks on
    /// `RETRY_DELAY`, so a loop that only re-checks its `Weak` at the top of
    /// the iteration would outlive the source by that whole delay — this
    /// asserts it exits in a fraction of it.
    #[tokio::test]
    async fn dropping_the_source_stops_the_refresh_task_promptly() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No server: the fetch fails fast and the loop settles into its retry
        // sleep, which is the state a drop has to be able to interrupt.
        let socket = dir.path().join("agent.sock");
        let shutdown = Arc::new(tokio::sync::Notify::new());
        let source = Arc::new(SocketTokenSource {
            socket_path: socket.to_string_lossy().into_owned(),
            cached: RwLock::new(None),
            shutdown: Arc::clone(&shutdown),
        });
        let task = tokio::spawn(refresh_loop(Arc::downgrade(&source), shutdown));

        // Let it fail once and enter the retry sleep.
        tokio::time::sleep(Duration::from_millis(100)).await;
        drop(source);

        let stopped = tokio::time::timeout(Duration::from_secs(1), task).await;
        assert!(
            stopped.is_ok(),
            "the refresh task must observe the drop rather than sleeping out RETRY_DELAY ({:?})",
            RETRY_DELAY
        );
        stopped.unwrap().expect("the refresh task must not panic");
    }
}
