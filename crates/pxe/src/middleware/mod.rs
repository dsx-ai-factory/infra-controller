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
use std::str::FromStr;

use axum::http::uri::PathAndQuery;
use axum::http::{Method, Request, Uri, header};

pub(super) mod logging;
pub(super) mod metrics;

pub(super) async fn normalize_request<B>(mut request: Request<B>) -> Request<B> {
    // tower-http's file services incorrectly apply Range to HEAD requests,
    // producing partial metadata or 416 instead of ignoring it (RFC 9110 §14.2).
    // Strip the header before routing to work around this dependency bug.
    if request.method() == Method::HEAD {
        request.headers_mut().remove(header::RANGE);
    }

    let uri = request.uri_mut();
    if let Some(p_q) = uri.path_and_query() {
        let path_and_query = p_q.to_string();
        let removed_duplicated_slashes = path_and_query.as_str().replace("//", "/");
        let new_path_and_query = match PathAndQuery::from_str(removed_duplicated_slashes.as_str()) {
            Ok(path_and_query) => path_and_query,
            _ => unreachable!(),
        };
        *uri = Uri::from(new_path_and_query);
    }
    request
}

#[cfg(test)]
mod test {
    use axum::Router;
    use axum::body::{Body, to_bytes};
    use axum::http::header::{CONTENT_LENGTH, CONTENT_RANGE, RANGE};
    use axum::http::{HeaderValue, Method, StatusCode};
    use bytes::Bytes;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use tempfile::TempDir;
    use tower::ServiceExt;
    use tower_http::services::ServeDir;
    use tower_layer::Layer;

    use super::*;
    #[tokio::test]
    async fn test_url_normalize() {
        let request = Request::builder()
            .uri("http://localhost:8080/api/v0/cloud-init//dpu/user-data")
            .body(())
            .unwrap();
        let result = normalize_request(request).await;
        assert_eq!(result.uri().path(), "/api/v0/cloud-init/dpu/user-data");
    }

    #[tokio::test]
    async fn test_static_file_ranges() -> Result<(), Box<dyn std::error::Error>> {
        let tmp_dir = TempDir::new()?;
        std::fs::write(tmp_dir.path().join("artifact"), b"artifact")?;
        let app = axum::middleware::map_request(normalize_request)
            .layer(Router::new().nest_service("/public", ServeDir::new(tmp_dir.path())));

        check_cases_async(
            [
                Case {
                    scenario: "HEAD ignores a satisfiable range",
                    input: (Method::HEAD, "bytes=0-0"),
                    expect: Yields((
                        StatusCode::OK,
                        Some(HeaderValue::from_static("8")),
                        None,
                        Bytes::new(),
                    )),
                },
                Case {
                    scenario: "HEAD ignores an unsatisfiable range",
                    input: (Method::HEAD, "bytes=999999-"),
                    expect: Yields((
                        StatusCode::OK,
                        Some(HeaderValue::from_static("8")),
                        None,
                        Bytes::new(),
                    )),
                },
                Case {
                    scenario: "GET preserves range support",
                    input: (Method::GET, "bytes=0-0"),
                    expect: Yields((
                        StatusCode::PARTIAL_CONTENT,
                        Some(HeaderValue::from_static("1")),
                        Some(HeaderValue::from_static("bytes 0-0/8")),
                        Bytes::from_static(b"a"),
                    )),
                },
            ],
            |(method, range)| {
                let app = app.clone();
                async move {
                    let response = app
                        .oneshot(
                            Request::builder()
                                .method(method)
                                .uri("/public/artifact")
                                .header(RANGE, range)
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    let status = response.status();
                    let content_length = response.headers().get(CONTENT_LENGTH).cloned();
                    let content_range = response.headers().get(CONTENT_RANGE).cloned();
                    to_bytes(response.into_body(), 1024)
                        .await
                        .map(|body| (status, content_length, content_range, body))
                        .map_err(|error| error.to_string())
                }
            },
        )
        .await;

        Ok(())
    }
}
