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

use crate::ownership::{Ownership, OwnershipHandle};

/// Body of `/readyz` while the controller's initial source list has not been loaded.
pub(crate) const WAITING_FOR_SOURCE_LIST: &str = "waiting for controller source list";

/// Unauthenticated liveness and readiness routes for Kubernetes probes.
///
/// `/readyz` follows the readiness flag the metrics endpoint shares, so both ports agree, and
/// explains a 503 with the rack ownership blockers, one per line.
pub(crate) fn router(readiness: HealthController, ownership: OwnershipHandle) -> Router {
    Router::new()
        .route("/livez", get(livez))
        .route("/readyz", get(readyz))
        .with_state((readiness, ownership))
}

async fn livez() -> impl IntoResponse {
    (StatusCode::OK, "ok")
}

async fn readyz(
    State((readiness, ownership)): State<(HealthController, OwnershipHandle)>,
) -> impl IntoResponse {
    if readiness.is_ready() {
        (StatusCode::OK, "ready".to_string())
    } else {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            ownership.blockers().join("\n"),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::Request;
    use tower::ServiceExt;

    use super::*;
    use crate::ownership::Owner;

    async fn probe(router: Router, path: &str) -> (StatusCode, String) {
        let response = router
            .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    /// An ownership map whose blockers are fixed by the test.
    struct Blocked(Vec<String>);

    impl Ownership for Blocked {
        fn owner_of_rack(&self, _rack_id: &str) -> Owner {
            Owner::Unknown
        }

        fn is_ready(&self) -> bool {
            self.0.is_empty()
        }

        fn blockers(&self) -> Vec<String> {
            self.0.clone()
        }
    }

    #[tokio::test]
    async fn readyz_follows_the_shared_flag_and_explains_a_503_with_the_ownership_blockers() {
        let readiness = HealthController::new();
        readiness.set_ready(false);
        let ownership = OwnershipHandle::new();
        let router = router(readiness.clone(), ownership.clone());

        assert_eq!(
            probe(router.clone(), "/livez").await,
            (StatusCode::OK, "ok".to_string())
        );
        assert_eq!(
            probe(router.clone(), "/readyz").await,
            (
                StatusCode::SERVICE_UNAVAILABLE,
                WAITING_FOR_SOURCE_LIST.to_string()
            )
        );

        ownership.bind(Arc::new(Blocked(vec![
            "no rack status from source mat-b yet".to_string(),
            "rack rack-001 is reported by mat-a, mat-c".to_string(),
        ])));
        assert_eq!(
            probe(router.clone(), "/readyz").await,
            (
                StatusCode::SERVICE_UNAVAILABLE,
                "no rack status from source mat-b yet\nrack rack-001 is reported by mat-a, mat-c"
                    .to_string()
            )
        );

        readiness.set_ready(true);
        assert_eq!(
            probe(router, "/readyz").await,
            (StatusCode::OK, "ready".to_string())
        );
    }
}
