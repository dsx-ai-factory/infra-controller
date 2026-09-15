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
use axum::extract::Request;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::json::JsonExt;

pub(crate) fn not_found() -> Response {
    redfish_error(StatusCode::NOT_FOUND, "resource not found")
}

pub(crate) fn bad_request(message: &str) -> Response {
    redfish_error(StatusCode::BAD_REQUEST, message)
}

pub(crate) fn ok_no_content() -> Response {
    StatusCode::NO_CONTENT.into_response()
}

/// Redfish error envelope (DSP0266 section 7.12) with the given status.
pub(crate) fn redfish_error(status: StatusCode, message: &str) -> Response {
    json!({"error": {"code": "Base.1.0.GeneralError", "message": message}}).into_response(status)
}

/// Largest plain-text error body carried into the envelope's `message`.
const ERROR_TEXT_LIMIT: usize = 16 * 1024;

/// Outermost router layer. Authentication, extraction, availability, and
/// status injection can answer without reaching a handler; keep their status
/// and headers, and give bodiless or non-JSON client and server errors the
/// Redfish envelope clients expect from a real BMC. An existing plain-text
/// diagnostic, such as an extractor rejection, becomes the envelope's
/// `message`. JSON error bodies are preserved and HEAD responses stay bodiless.
pub async fn redfish_error_envelope(request: Request, next: Next) -> Response {
    let head = request.method() == Method::HEAD;
    let response = next.run(request).await;
    let status = response.status();
    if head
        || !(status.is_client_error() || status.is_server_error())
        || carbide_axum_utils::is_json_response(&response)
    {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let text = match axum::body::to_bytes(body, ERROR_TEXT_LIMIT).await {
        Ok(bytes) => String::from_utf8(bytes.to_vec())
            .ok()
            .map(|text| text.trim().to_owned())
            .filter(|text| !text.is_empty()),
        Err(_) => None,
    };
    let message = text.unwrap_or_else(|| {
        status
            .canonical_reason()
            .unwrap_or("request failed")
            .to_owned()
    });
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.remove(header::CONTENT_ENCODING);
    Response::from_parts(parts, redfish_error(status, &message).into_body())
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::body::Body;
    use axum::routing::get;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use serde_json::Value;
    use tower::ServiceExt;

    use super::*;

    #[tokio::test]
    async fn envelopes_bodiless_and_plain_text_errors_only() {
        let original =
            json!({"error":{"code":"Mock.SpecificFailure","message":"Useful diagnostic"}});
        let router = Router::new()
            .route(
                "/bodiless",
                get(|| async { (StatusCode::UNAUTHORIZED, [("www-authenticate", "Basic")]) }),
            )
            .route(
                "/text",
                get(|| async { (StatusCode::BAD_REQUEST, "Invalid URL") }),
            )
            .route("/json", {
                let body = original.to_string();
                get(move || async move {
                    (
                        StatusCode::BAD_REQUEST,
                        [("content-type", "application/json; charset=utf-8")],
                        body,
                    )
                })
            })
            .route("/ok", get(|| async { "plain success" }))
            .layer(axum::middleware::from_fn(redfish_error_envelope));
        let envelope = |code: &str, message: &str| Some((code.to_owned(), message.to_owned()));
        check_cases_async(
            [
                Case {
                    scenario: "bodiless auth error gains an envelope and keeps headers",
                    input: ("GET", "/bodiless"),
                    expect: Yields((
                        StatusCode::UNAUTHORIZED,
                        true,
                        envelope("Base.1.0.GeneralError", "Unauthorized"),
                    )),
                },
                Case {
                    scenario: "plain-text rejection keeps its diagnostic",
                    input: ("GET", "/text"),
                    expect: Yields((
                        StatusCode::BAD_REQUEST,
                        false,
                        envelope("Base.1.0.GeneralError", "Invalid URL"),
                    )),
                },
                Case {
                    scenario: "parameterized JSON error is preserved",
                    input: ("GET", "/json"),
                    expect: Yields((
                        StatusCode::BAD_REQUEST,
                        false,
                        envelope("Mock.SpecificFailure", "Useful diagnostic"),
                    )),
                },
                Case {
                    scenario: "HEAD error stays bodiless",
                    input: ("HEAD", "/text"),
                    expect: Yields((StatusCode::BAD_REQUEST, false, None)),
                },
                Case {
                    scenario: "success is untouched",
                    input: ("GET", "/ok"),
                    expect: Yields((StatusCode::OK, false, None)),
                },
            ],
            |(method, path)| {
                let router = router.clone();
                async move {
                    let response = router
                        .oneshot(
                            axum::http::Request::builder()
                                .method(method)
                                .uri(path)
                                .body(Body::empty())
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    let status = response.status();
                    let authenticate = response.headers().contains_key("www-authenticate");
                    let json = response
                        .headers()
                        .get("content-type")
                        .is_some_and(|v| v.to_str().unwrap().starts_with("application/json"));
                    let bytes = axum::body::to_bytes(response.into_body(), 4096)
                        .await
                        .unwrap();
                    let error = json
                        .then(|| serde_json::from_slice::<Value>(&bytes).unwrap())
                        .and_then(|v| {
                            Some((
                                v["error"]["code"].as_str()?.to_owned(),
                                v["error"]["message"].as_str()?.to_owned(),
                            ))
                        });
                    if method == "HEAD" {
                        assert!(bytes.is_empty());
                    }
                    Ok::<_, std::convert::Infallible>((status, authenticate, error))
                }
            },
        )
        .await;
    }
}
