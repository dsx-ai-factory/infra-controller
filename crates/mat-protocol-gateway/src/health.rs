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

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;

/// Readiness flag shared between the bootstrap sequence and the `/readyz` handler.
///
/// It starts false and flips to true once the controller's initial source list has been
/// validated and handed to UFM reconciliation. It never flips back: a source-list change ends
/// the process instead.
#[derive(Clone, Debug, Default)]
pub(crate) struct Readiness(Arc<AtomicBool>);

impl Readiness {
    /// A flag that reports not ready; clones share the same state.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Makes `/readyz` return 200 from now on.
    pub(crate) fn mark_ready(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Current value of the shared flag.
    fn is_ready(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Unauthenticated liveness and readiness routes for Kubernetes probes.
pub(crate) fn router(readiness: Readiness) -> Router {
    Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .with_state(readiness)
}

async fn livez() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(State(readiness): State<Readiness>) -> impl IntoResponse {
    if readiness.is_ready() {
        (StatusCode::OK, "ready")
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "waiting for controller source list",
        )
    }
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;

    async fn status(router: Router, path: &str) -> StatusCode {
        router
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    }

    #[tokio::test]
    async fn readyz_follows_the_shared_flag_and_livez_is_always_ok() {
        let readiness = Readiness::new();
        let router = router(readiness.clone());

        assert_eq!(status(router.clone(), "/livez").await, StatusCode::OK);
        assert_eq!(
            status(router.clone(), "/readyz").await,
            StatusCode::SERVICE_UNAVAILABLE
        );

        readiness.mark_ready();

        assert_eq!(status(router, "/readyz").await, StatusCode::OK);
    }
}
