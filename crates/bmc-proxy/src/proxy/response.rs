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

//! The answer a caller receives from the BMC: a 4xx or 5xx body scrubbed of
//! the credential its attempt used, or replaced when it cannot be inspected;
//! other bodies stream through unchanged.

use axum::body::Body;
use carbide_utils::redfish::redact_redfish_response_body;
use http::{HeaderMap, Response};

use crate::proxy::upstream::{MAX_BUFFERED_BODY_SIZE, is_hop_by_hop_header};

/// Reuse the established request-body bound when inspecting error responses;
/// anything larger is omitted rather than risking an unbounded allocation or
/// forwarding a credential that could not be searched safely.
const MAX_REDACTABLE_ERROR_BODY_SIZE: usize = MAX_BUFFERED_BODY_SIZE;

const OMITTED_BMC_ERROR_RESPONSE: &str = r#"{"error":{"message":"BMC error response omitted because it could not be safely sanitized"}}"#;

/// Buffers only final HTTP error responses that have a known authentication
/// secret, so they can be scrubbed before leaving the credential-owning proxy.
/// Successful responses, redirects, and responses without a secret retain the
/// existing streaming path. Uninspectable error bodies fail closed rather than
/// forwarding bytes that may contain the credential.
pub(super) async fn prepare_response_body(
    status: reqwest::StatusCode,
    headers: &HeaderMap,
    body: Body,
    sensitive_values: &[String],
) -> PreparedResponseBody {
    if sensitive_values.is_empty() {
        return PreparedResponseBody::Unchanged(body);
    }
    if !status.is_client_error() && !status.is_server_error() {
        return PreparedResponseBody::Unchanged(body);
    }
    // Automatic decompression is disabled on the upstream client. If the caller
    // negotiated a content coding, omit an encoded error instead of searching
    // encoded bytes and potentially forwarding a hidden secret.
    if has_non_identity_content_encoding(headers) {
        return PreparedResponseBody::Replaced(Body::from(OMITTED_BMC_ERROR_RESPONSE));
    }

    let Ok(body) = axum::body::to_bytes(body, MAX_REDACTABLE_ERROR_BODY_SIZE).await else {
        return PreparedResponseBody::Replaced(Body::from(OMITTED_BMC_ERROR_RESPONSE));
    };
    let Ok(text) = std::str::from_utf8(&body) else {
        return PreparedResponseBody::Replaced(Body::from(OMITTED_BMC_ERROR_RESPONSE));
    };
    let redacted = redact_redfish_response_body(text, sensitive_values.iter().map(String::as_str));
    if redacted == text {
        PreparedResponseBody::Unchanged(Body::from(body))
    } else {
        PreparedResponseBody::Redacted(Body::from(redacted))
    }
}

fn has_non_identity_content_encoding(headers: &HeaderMap) -> bool {
    headers
        .get_all(http::header::CONTENT_ENCODING)
        .iter()
        .any(|value| {
            let Ok(value) = value.to_str() else {
                return true;
            };
            value.split(',').any(|encoding| {
                let encoding = encoding.trim();
                encoding.is_empty() || !encoding.eq_ignore_ascii_case("identity")
            })
        })
}

pub(super) enum PreparedResponseBody {
    Unchanged(Body),
    Redacted(Body),
    Replaced(Body),
}

pub(super) fn build_response(
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: PreparedResponseBody,
) -> Response<Body> {
    let body_was_rewritten = !matches!(&body, PreparedResponseBody::Unchanged(_));
    let body_was_replaced = matches!(&body, PreparedResponseBody::Replaced(_));
    let body = match body {
        PreparedResponseBody::Unchanged(body)
        | PreparedResponseBody::Redacted(body)
        | PreparedResponseBody::Replaced(body) => body,
    };
    let mut response = Response::builder().status(status);
    for (name, value) in headers {
        if is_hop_by_hop_header(name.as_str())
            || name == reqwest::header::CONTENT_LENGTH
            || (body_was_rewritten
                && (name == reqwest::header::CONTENT_ENCODING
                    || name == reqwest::header::ETAG
                    || name.as_str().eq_ignore_ascii_case("content-md5")
                    || name.as_str().eq_ignore_ascii_case("digest")))
            || (body_was_replaced && name == reqwest::header::CONTENT_TYPE)
        {
            continue;
        }
        response = response.header(name, value);
    }
    if body_was_replaced {
        response = response.header(reqwest::header::CONTENT_TYPE, "application/json");
    }
    response.body(body).unwrap()
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;

    use axum::body::Body;
    use axum::http::{HeaderMap, HeaderValue, StatusCode};
    use bytes::Bytes;
    use carbide_utils::redfish::redfish_basic_authorization_context;
    use http_body_util::BodyExt;
    use tokio_stream::iter;

    use super::{
        MAX_REDACTABLE_ERROR_BODY_SIZE, OMITTED_BMC_ERROR_RESPONSE, PreparedResponseBody,
        build_response, prepare_response_body,
    };
    use crate::proxy::credentials::BmcCredentials;

    #[tokio::test]
    async fn build_response_keeps_safe_headers_and_streams_body() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            reqwest::header::CONTENT_LENGTH,
            HeaderValue::from_static("999"),
        );
        headers.insert(
            reqwest::header::CONNECTION,
            HeaderValue::from_static("keep-alive"),
        );

        let body = Body::from_stream(iter([
            Result::<Bytes, Infallible>::Ok(Bytes::from_static(br#"{"value":"#)),
            Result::<Bytes, Infallible>::Ok(Bytes::from_static(br#""ok"}"#)),
        ]));

        let response = build_response(
            reqwest::StatusCode::OK,
            &headers,
            PreparedResponseBody::Unchanged(body),
        );

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .unwrap(),
            "application/json"
        );
        assert!(
            !response
                .headers()
                .contains_key(reqwest::header::CONTENT_LENGTH)
        );
        assert!(!response.headers().contains_key(reqwest::header::CONNECTION));

        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body, Bytes::from_static(br#"{"value":"ok"}"#));
    }

    #[tokio::test]
    async fn final_http_error_response_redacts_the_upstream_credential() {
        // Build an error response that echoes both credential representations
        // and carries headers invalidated by body rewriting.
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        headers.insert(
            reqwest::header::CONTENT_LENGTH,
            HeaderValue::from_static("999"),
        );
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            HeaderValue::from_static("identity"),
        );
        headers.insert(reqwest::header::ETAG, HeaderValue::from_static("error-v1"));
        let (basic_authorization, sensitive_values) =
            redfish_basic_authorization_context("admin", Some("secret"));
        let body = Body::from(format!(
            r#"{{"error":{{"@Message.ExtendedInfo":[{{"Message":"credential s\u0065cret or {basic_authorization} rejected"}}]}}}}"#,
        ));

        // Sanitize before constructing the downstream response.
        let body = prepare_response_body(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            &headers,
            body,
            &sensitive_values,
        )
        .await;
        assert!(matches!(&body, PreparedResponseBody::Redacted(_)));

        let response = build_response(reqwest::StatusCode::INTERNAL_SERVER_ERROR, &headers, body);

        // Rewritten responses omit stale entity metadata and every secret form.
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            response.headers().get(reqwest::header::CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
        assert!(
            !response
                .headers()
                .contains_key(reqwest::header::CONTENT_LENGTH)
        );
        assert!(
            !response
                .headers()
                .contains_key(reqwest::header::CONTENT_ENCODING)
        );
        assert!(!response.headers().contains_key(reqwest::header::ETAG));
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let body = std::str::from_utf8(&body).expect("redacted body remains UTF-8");
        assert!(!body.contains("secret"));
        assert!(!body.contains(r"s\u0065cret"));
        assert!(!body.contains(&basic_authorization));
        assert!(body.contains("credential REDACTED or REDACTED rejected"));
    }

    #[tokio::test]
    async fn plain_text_error_response_redacts_a_session_token() {
        // Derive redaction context from the exact credential application path.
        let credentials = BmcCredentials::SessionToken {
            token: "token-123".to_string(),
        };
        let client = reqwest_middleware::ClientBuilder::new(reqwest::Client::new()).build();
        let request = client.get("https://example.com/redfish/v1");
        let (_, sensitive_values) = credentials
            .apply_to_request(request)
            .expect("credentials should apply");

        // Sanitize the plain-text BMC failure with the retained token.
        let headers = HeaderMap::new();
        let prepared = prepare_response_body(
            reqwest::StatusCode::BAD_GATEWAY,
            &headers,
            Body::from("session token-123 rejected"),
            &sensitive_values,
        )
        .await;

        // The proxy preserves the message while removing the reusable token.
        assert!(matches!(&prepared, PreparedResponseBody::Redacted(_)));
        let prepared = match prepared {
            PreparedResponseBody::Redacted(body) => body,
            PreparedResponseBody::Unchanged(_) | PreparedResponseBody::Replaced(_) => {
                unreachable!("the session token should be redacted")
            }
        };
        let body = prepared.collect().await.unwrap().to_bytes();
        assert_eq!(body, Bytes::from_static(b"session REDACTED rejected"));
    }

    #[tokio::test]
    async fn unmatched_error_and_success_bodies_remain_unchanged() {
        let headers = HeaderMap::new();
        let sensitive_values = ["secret".to_string()];
        for (status, body) in [
            (
                reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                "an unrelated BMC error",
            ),
            (
                reqwest::StatusCode::OK,
                "successful value containing secret",
            ),
        ] {
            let prepared =
                prepare_response_body(status, &headers, Body::from(body), &sensitive_values).await;
            assert!(matches!(&prepared, PreparedResponseBody::Unchanged(_)));
            let prepared = match prepared {
                PreparedResponseBody::Unchanged(body) => body,
                PreparedResponseBody::Redacted(_) | PreparedResponseBody::Replaced(_) => {
                    unreachable!("the response body should remain unchanged")
                }
            };
            let actual = prepared.collect().await.unwrap().to_bytes();
            assert_eq!(actual, Bytes::from(body));
        }
    }

    #[tokio::test]
    async fn uninspectable_error_response_fails_closed() {
        let mut body = vec![b'x'; MAX_REDACTABLE_ERROR_BODY_SIZE + 1];
        body[.."secret".len()].copy_from_slice(b"secret");
        let headers = HeaderMap::new();
        let prepared = prepare_response_body(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            &headers,
            Body::from(body),
            &["secret".to_string()],
        )
        .await;
        assert!(matches!(&prepared, PreparedResponseBody::Replaced(_)));
        let response = build_response(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            &reqwest::header::HeaderMap::new(),
            prepared,
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            body,
            Bytes::from_static(OMITTED_BMC_ERROR_RESPONSE.as_bytes())
        );
        assert!(
            !body
                .windows("secret".len())
                .any(|window| window == b"secret")
        );
    }

    #[tokio::test]
    async fn encoded_error_response_fails_closed() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            HeaderValue::from_static("gzip"),
        );
        let prepared = prepare_response_body(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            &headers,
            Body::from("opaque encoded bytes"),
            &["secret".to_string()],
        )
        .await;
        assert!(matches!(&prepared, PreparedResponseBody::Replaced(_)));

        let response = build_response(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            &headers,
            prepared,
        );
        assert!(
            !response
                .headers()
                .contains_key(reqwest::header::CONTENT_ENCODING)
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            body,
            Bytes::from_static(OMITTED_BMC_ERROR_RESPONSE.as_bytes())
        );
    }

    #[tokio::test]
    async fn encoded_success_response_remains_unchanged() {
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::CONTENT_ENCODING,
            HeaderValue::from_static("gzip"),
        );
        let prepared = prepare_response_body(
            reqwest::StatusCode::OK,
            &headers,
            Body::from("opaque encoded bytes"),
            &["secret".to_string()],
        )
        .await;
        assert!(matches!(&prepared, PreparedResponseBody::Unchanged(_)));

        let response = build_response(reqwest::StatusCode::OK, &headers, prepared);
        assert_eq!(
            response.headers().get(reqwest::header::CONTENT_ENCODING),
            Some(&HeaderValue::from_static("gzip"))
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body, Bytes::from_static(b"opaque encoded bytes"));
    }
}
