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

use std::collections::BTreeMap;

use bytes::Bytes;
use http::header::{CONTENT_TYPE, HeaderValue};
use http::{HeaderMap, Method, StatusCode, Uri};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// An owned snapshot of an HTTP request received by the mock server.
#[derive(Clone, Debug)]
pub(crate) struct MockRequest {
    pub(crate) method: Method,
    pub(crate) uri: Uri,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

impl MockRequest {
    /// Create a request snapshot with no headers.
    ///
    /// # Panics
    ///
    /// Panics when `uri` is not a valid HTTP URI.
    pub(crate) fn new(method: Method, uri: impl AsRef<str>, body: impl Into<Bytes>) -> Self {
        Self {
            method,
            uri: uri.as_ref().parse().expect("mock request URI should parse"),
            headers: HeaderMap::new(),
            body: body.into(),
        }
    }

    /// Create a request snapshot from HTTP request parts and a buffered body.
    pub(crate) fn from_parts(method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Self {
        Self {
            method,
            uri,
            headers,
            body,
        }
    }

    /// Return whether the request method and path exactly match the expected values.
    pub(crate) fn matches(&self, method: Method, path: &str) -> bool {
        self.method == method && self.uri.path() == path
    }

    /// Match a route pattern and return its percent-decoded named path captures.
    ///
    /// A capture occupies one complete path segment and uses `{name}` syntax.
    /// Literal segments and the number of segments must match exactly.
    pub(crate) fn route_captures(
        &self,
        method: Method,
        route: &str,
    ) -> Option<BTreeMap<String, String>> {
        if self.method != method {
            return None;
        }

        let request_segments = self.uri.path().split('/').collect::<Vec<_>>();
        let route_segments = route.split('/').collect::<Vec<_>>();
        if request_segments.len() != route_segments.len() {
            return None;
        }

        let mut captures = BTreeMap::new();
        for (request_segment, route_segment) in request_segments.into_iter().zip(route_segments) {
            let Some(name) = route_segment
                .strip_prefix('{')
                .and_then(|segment| segment.strip_suffix('}'))
                .filter(|name| !name.is_empty())
            else {
                if request_segment != route_segment {
                    return None;
                }
                continue;
            };

            let value = urlencoding::decode(request_segment).ok()?.into_owned();
            if captures.insert(name.to_string(), value).is_some() {
                return None;
            }
        }
        Some(captures)
    }

    /// Decode the form query into owned key-value pairs while preserving order.
    pub(crate) fn query_pairs(&self) -> Vec<(String, String)> {
        self.uri
            .query()
            .map(|query| {
                url::form_urlencoded::parse(query.as_bytes())
                    .into_owned()
                    .collect()
            })
            .unwrap_or_default()
    }

    // TODO: Add `matches_query(&[(&str, &str)])` if exact query matching becomes common.

    /// Return every decoded value for `name` in request order.
    pub(crate) fn query_values(&self, name: &str) -> Vec<String> {
        self.query_pairs()
            .into_iter()
            .filter_map(|(key, value)| (key == name).then_some(value))
            .collect()
    }

    /// Deserialize the buffered request body as JSON without consuming it.
    pub(crate) fn json<T: DeserializeOwned>(&self) -> serde_json::Result<T> {
        serde_json::from_slice(&self.body)
    }
}

/// An owned HTTP response returned by a mock handler.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MockResponse {
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
}

impl MockResponse {
    /// Create a JSON response with an `application/json` content type.
    ///
    /// # Panics
    ///
    /// Panics when `value` cannot be serialized as JSON.
    pub(crate) fn json(status: StatusCode, value: &impl Serialize) -> Self {
        let body = serde_json::to_vec(value).expect("mock response JSON should serialize");
        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Self {
            status,
            headers,
            body: body.into(),
        }
    }

    /// Create a response containing raw bytes and no default headers.
    pub(crate) fn bytes(status: StatusCode, body: impl Into<Bytes>) -> Self {
        Self {
            status,
            headers: HeaderMap::new(),
            body: body.into(),
        }
    }

    /// Create an empty response with no default headers.
    pub(crate) fn empty(status: StatusCode) -> Self {
        Self::bytes(status, Bytes::new())
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;
    use serde_json::json;

    use super::*;

    #[test]
    fn exact_method_and_path_matching() {
        let request = MockRequest::new(Method::GET, "/nvue_v1/system?filled=true", Bytes::new());

        value_scenarios!(run = |(method, path)| request.matches(method, path);
            "exact request target" {
                (Method::GET, "/nvue_v1/system") => true,
                (Method::POST, "/nvue_v1/system") => false,
                (Method::GET, "/nvue_v1/system/") => false,
            }
        );
    }

    #[test]
    fn route_captures_are_named_and_percent_decoded() {
        let request = MockRequest::new(
            Method::GET,
            "/nvue_v1/revision/rev%201%2Fblue",
            Bytes::new(),
        );
        let captures = request
            .route_captures(Method::GET, "/nvue_v1/revision/{revision_id}")
            .expect("route should match");

        assert_eq!(
            captures.get("revision_id").map(String::as_str),
            Some("rev 1/blue")
        );
        assert!(
            request
                .route_captures(Method::POST, "/nvue_v1/revision/{revision_id}")
                .is_none()
        );
        assert!(
            request
                .route_captures(Method::GET, "/nvue_v1/{revision_id}/{revision_id}")
                .is_none()
        );
    }

    #[test]
    fn query_decoding_preserves_order_and_repeated_values() {
        let request = MockRequest::new(
            Method::GET,
            "/nvue_v1/?include=%2Fsystem&omit=%2Fbridge&include=%2Fvrf%2F*",
            Bytes::new(),
        );

        assert_eq!(
            request.query_pairs(),
            [
                ("include".to_string(), "/system".to_string()),
                ("omit".to_string(), "/bridge".to_string()),
                ("include".to_string(), "/vrf/*".to_string()),
            ]
        );
        assert_eq!(request.query_values("include"), ["/system", "/vrf/*"]);
        assert!(request.query_values("missing").is_empty());
    }

    #[test]
    fn json_body_can_be_inspected_repeatedly() {
        let request = MockRequest::new(
            Method::PATCH,
            "/nvue_v1/",
            serde_json::to_vec(&json!({"system": {"hostname": "leaf-1"}}))
                .expect("JSON should serialize"),
        );

        let first: serde_json::Value = request.json().expect("body should parse");
        let second: serde_json::Value = request.json().expect("body should parse again");
        assert_eq!(first, second);
    }

    #[test]
    fn response_constructors_preserve_status_headers_and_body() {
        let json_response = MockResponse::json(StatusCode::CREATED, &json!({"revision": "1"}));
        assert_eq!(json_response.status, StatusCode::CREATED);
        assert_eq!(
            json_response.headers.get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("application/json"))
        );
        assert_eq!(
            json_response.body,
            Bytes::from_static(br#"{"revision":"1"}"#)
        );

        let bytes_response = MockResponse::bytes(StatusCode::ACCEPTED, "pending");
        assert_eq!(bytes_response.body, Bytes::from_static(b"pending"));
        assert!(bytes_response.headers.is_empty());

        let empty_response = MockResponse::empty(StatusCode::NO_CONTENT);
        assert!(empty_response.body.is_empty());
        assert!(empty_response.headers.is_empty());
    }
}
