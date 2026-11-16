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
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;

/// Applies `If-Range` (RFC 9110 §13.1.5), which `ServeDir` ignores.
///
/// When the validator does not match the served file, the request is reissued
/// without `Range` so the client receives the complete current file.
pub(crate) async fn honor_if_range(request: Request, next: Next) -> Response {
    let Some(validator) = request.headers().get(header::IF_RANGE).cloned() else {
        return next.run(request).await;
    };
    if request.method() != Method::GET || !request.headers().contains_key(header::RANGE) {
        return next.run(request).await;
    }

    let (parts, body) = request.into_parts();
    let response = next
        .clone()
        .run(Request::from_parts(parts.clone(), body))
        .await;
    if !matches!(
        response.status(),
        StatusCode::PARTIAL_CONTENT | StatusCode::RANGE_NOT_SATISFIABLE
    ) || validator_matches(&validator, response.headers())
    {
        return response;
    }

    drop(response);
    let mut request = Request::from_parts(parts, Body::empty());
    request.headers_mut().remove(header::RANGE);
    next.run(request).await
}

fn validator_matches(validator: &HeaderValue, headers: &HeaderMap) -> bool {
    let Ok(value) = validator.to_str() else {
        return false;
    };
    if value.starts_with('"') {
        return headers.get(header::ETAG) == Some(validator);
    }

    let Ok(requested) = httpdate::parse_http_date(value) else {
        return false;
    };
    headers
        .get(header::LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| httpdate::parse_http_date(value).ok())
        .is_some_and(|modified| requested == modified)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::Router;
    use axum::body::to_bytes;
    use carbide_test_support::Outcome::Yields;
    use carbide_test_support::{Case, check_cases_async};
    use tempfile::TempDir;
    use tower::ServiceExt;
    use tower_http::services::ServeDir;

    use super::*;

    #[tokio::test]
    async fn conditional_ranges_preserve_the_current_representation() {
        let directory = TempDir::new().unwrap();
        let contents = b"current artifact";
        std::fs::write(directory.path().join("artifact"), contents).unwrap();
        let app = Router::new()
            .nest_service("/public", ServeDir::new(directory.path()))
            .layer(axum::middleware::from_fn(honor_if_range));
        let baseline = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/public/artifact")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let etag = baseline.headers()[header::ETAG].clone();
        let modified = baseline.headers()[header::LAST_MODIFIED].clone();
        let future = httpdate::fmt_http_date(
            httpdate::parse_http_date(modified.to_str().unwrap()).unwrap()
                + Duration::from_secs(60),
        );
        let weak_etag = format!("W/{}", etag.to_str().unwrap());
        let cases = [
            (
                "changed etag",
                Some("\"previous\""),
                "bytes=0-3",
                None,
                StatusCode::OK,
            ),
            (
                "matching etag",
                Some(etag.to_str().unwrap()),
                "bytes=0-3",
                None,
                StatusCode::PARTIAL_CONTENT,
            ),
            (
                "weak etag",
                Some(weak_etag.as_str()),
                "bytes=0-3",
                None,
                StatusCode::OK,
            ),
            (
                "changed date",
                Some("Tue, 01 Jan 1980 00:00:00 GMT"),
                "bytes=0-3",
                None,
                StatusCode::OK,
            ),
            (
                "matching date",
                Some(modified.to_str().unwrap()),
                "bytes=0-3",
                None,
                StatusCode::PARTIAL_CONTENT,
            ),
            (
                "future date is not an exact match",
                Some(future.as_str()),
                "bytes=0-3",
                None,
                StatusCode::OK,
            ),
            (
                "changed artifact shorter than resume offset",
                Some("\"previous\""),
                "bytes=100-",
                None,
                StatusCode::OK,
            ),
            (
                "other preconditions take precedence",
                Some("\"previous\""),
                "bytes=0-3",
                Some("\"previous\""),
                StatusCode::PRECONDITION_FAILED,
            ),
            (
                "ordinary range",
                None,
                "bytes=0-3",
                None,
                StatusCode::PARTIAL_CONTENT,
            ),
        ];

        check_cases_async(
            cases
                .into_iter()
                .map(|(scenario, validator, range, if_match, expected_status)| {
                    let expected_range = (expected_status == StatusCode::PARTIAL_CONTENT)
                        .then(|| "bytes 0-3/16".to_string());
                    let expected_body: &[u8] = match expected_status {
                        StatusCode::OK => contents,
                        StatusCode::PARTIAL_CONTENT => &contents[..4],
                        _ => &[],
                    };
                    Case {
                        scenario,
                        input: (validator, range, if_match),
                        expect: Yields((expected_status, expected_range, expected_body.to_vec())),
                    }
                }),
            |(validator, range, if_match)| {
                let app = app.clone();
                async move {
                    let mut request = Request::builder()
                        .uri("/public/artifact")
                        .header(header::RANGE, range);
                    if let Some(validator) = validator {
                        request = request.header(header::IF_RANGE, validator);
                    }
                    if let Some(if_match) = if_match {
                        request = request.header(header::IF_MATCH, if_match);
                    }
                    let response = app
                        .oneshot(request.body(Body::empty()).unwrap())
                        .await
                        .unwrap();
                    let status = response.status();
                    let range = response
                        .headers()
                        .get(header::CONTENT_RANGE)
                        .map(|value| value.to_str().unwrap().to_string());
                    let body = to_bytes(response.into_body(), 1024)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok::<_, String>((status, range, body.to_vec()))
                }
            },
        )
        .await;
    }
}
