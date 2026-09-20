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

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::get;
use metrics_endpoint::HealthController;

/// Unauthenticated liveness and readiness routes for Kubernetes probes.
///
/// `/readyz` follows the readiness flag the metrics endpoint shares, so both ports agree.
pub(crate) fn router(readiness: HealthController) -> Router {
    Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .with_state(readiness)
}

async fn livez() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(State(readiness): State<HealthController>) -> impl IntoResponse {
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
        let readiness = HealthController::new();
        readiness.set_ready(false);
        let router = router(readiness.clone());

        assert_eq!(status(router.clone(), "/livez").await, StatusCode::OK);
        assert_eq!(
            status(router.clone(), "/readyz").await,
            StatusCode::SERVICE_UNAVAILABLE
        );

        readiness.set_ready(true);

        assert_eq!(status(router, "/readyz").await, StatusCode::OK);
    }
}
