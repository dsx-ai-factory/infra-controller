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
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use tower_http::services::ServeFile;

use crate::common::AppState;

async fn root_ca(headers: HeaderMap, state: State<AppState>) -> impl IntoResponse {
    let mut req = Request::new(Body::empty());
    *req.headers_mut() = headers;

    // the docs for this fn actually say it should return Ok(404) if the file isn't there or
    // you don't have permissions to it... but it still returns a std::io::Result so we do
    // still have to handle the apparently possible failure modes.
    match ServeFile::new_with_mime(
        &state.runtime_config.bootstrap_root_ca_path,
        &mime::APPLICATION_OCTET_STREAM,
    )
    .try_call(req)
    .await
    {
        Ok(response) => response.into_response(),
        Err(err) => {
            tracing::error!(error = %err, "error reading root ca cert file");
            let response = Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(Body::from("error reading root ca cert file?"))
                .unwrap();

            response.into_response()
        }
    }
}

pub(crate) fn get_router(path_prefix: &str) -> Router<AppState> {
    Router::new().route(
        format!("{}/{}", path_prefix, "root_ca").as_str(),
        get(root_ca),
    )
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use axum::http::Request;
    use tempfile::tempdir;
    use tower::ServiceExt;

    use super::*;
    use crate::common::test_app_state;

    #[tokio::test]
    async fn root_ca_route_serves_the_bootstrap_ca() {
        let temp_dir = tempdir().expect("create temporary CA directory");
        let api_ca_path = temp_dir.path().join("api-ca.pem");
        let bootstrap_ca_path = temp_dir.path().join("bootstrap-ca.pem");
        std::fs::write(&api_ca_path, b"api trust CA").expect("write API CA");
        std::fs::write(&bootstrap_ca_path, b"bootstrap trust CA").expect("write bootstrap CA");

        let mut state = test_app_state();
        state.runtime_config.forge_root_ca_path = api_ca_path.to_string_lossy().into_owned();
        state.runtime_config.bootstrap_root_ca_path =
            bootstrap_ca_path.to_string_lossy().into_owned();

        let response = get_router("/api/v0/tls")
            .with_state(state)
            .oneshot(
                Request::builder()
                    .uri("/api/v0/tls/root_ca")
                    .body(Body::empty())
                    .expect("build request"),
            )
            .await
            .expect("serve bootstrap CA");

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read response body");
        assert_eq!(body.as_ref(), b"bootstrap trust CA");
    }
}
