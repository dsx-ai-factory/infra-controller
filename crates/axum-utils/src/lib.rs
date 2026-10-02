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

//! Reusable middleware and routing utilities for Axum-based simulators.

pub mod authority_router;
pub mod injection;
pub mod router;

/// Whether a response declares JSON, including structured JSON suffixes and media parameters.
pub fn is_json_response(response: &axum::response::Response) -> bool {
    let Some(value) = response.headers().get(axum::http::header::CONTENT_TYPE) else {
        return false;
    };
    let Ok(s) = value.to_str() else { return false };
    let mime = s.split(';').next().unwrap_or(s).trim();
    let Some((kind, subtype)) = mime.split_once('/') else {
        return false;
    };
    if kind.is_empty() || subtype.is_empty() {
        return false;
    }
    mime.eq_ignore_ascii_case("application/json") || subtype.to_ascii_lowercase().ends_with("+json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognizes_json_media_types() {
        carbide_test_support::value_scenarios!(run = |content_type: Option<&str>| {
            let mut response = axum::response::Response::new(axum::body::Body::empty());
            if let Some(value) = content_type {
                response.headers_mut().insert(axum::http::header::CONTENT_TYPE, value.parse().unwrap());
            }
            is_json_response(&response)
        };
            "JSON media types and parameters" {
                Some("application/json") => true,
                Some("Application/JSON") => true,
                Some("application/json; charset=utf-8") => true,
                Some("application/problem+json") => true,
                Some("Application/Vnd.Redfish+JSON; charset=utf-8") => true,
            }
            "non-JSON and missing media types" {
                None => false,
                Some("text/event-stream") => false,
                Some("text/plain") => false,
                Some("text/json") => false,
                Some("application/octet-stream") => false,
                Some("application/json-garbage") => false,
                Some("bogus+json") => false,
                Some("/+json") => false,
            }
        );
    }
}
