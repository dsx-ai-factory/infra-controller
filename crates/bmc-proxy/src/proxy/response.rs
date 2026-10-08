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

//! The answer a caller receives from the BMC: a 4xx, 5xx, or returned redirect
//! body scrubbed of the credential its attempt used, or replaced when it
//! cannot be inspected; other bodies stream through unchanged.

use std::borrow::Cow;
use std::net::IpAddr;

use axum::body::Body;
use carbide_instrument::emit;
use carbide_utils::redfish::redact_redfish_response_body;
use http::{HeaderMap, Method, Response, StatusCode};
use url::Url;

use crate::config::RedirectMode;
use crate::metrics::{RedirectDisposition, RedirectObserved, RedirectStatus, RedirectTarget};
use crate::proxy::error_response;
use crate::proxy::upstream::{
    MAX_BUFFERED_BODY_SIZE, classify_redirect_target, is_hop_by_hop_header,
};

/// Reuse the established request-body bound when inspecting response bodies;
/// anything larger is omitted rather than risking an unbounded allocation or
/// forwarding a credential that could not be searched safely.
const MAX_REDACTABLE_RESPONSE_BODY_SIZE: usize = MAX_BUFFERED_BODY_SIZE;

const OMITTED_BMC_ERROR_RESPONSE: &str = r#"{"error":{"message":"BMC error response omitted because it could not be safely sanitized"}}"#;

/// Buffers final HTTP errors and redirects returned to the caller when the
/// request used a known authentication secret. Successful responses and
/// responses without a secret retain the existing streaming path.
/// Uninspectable bodies fail closed rather than forwarding bytes that may
/// contain the credential.
pub(super) async fn prepare_response_body(
    status: reqwest::StatusCode,
    headers: &HeaderMap,
    body: Body,
    sensitive_values: &[String],
) -> PreparedResponseBody {
    if sensitive_values.is_empty() {
        return PreparedResponseBody::Unchanged(body);
    }
    if !status.is_client_error() && !status.is_server_error() && !is_redirect_response(status) {
        return PreparedResponseBody::Unchanged(body);
    }
    // Automatic decompression is disabled on the upstream client. If the caller
    // negotiated a content coding, omit an encoded error instead of searching
    // encoded bytes and potentially forwarding a hidden secret.
    if has_non_identity_content_encoding(headers) {
        return PreparedResponseBody::Replaced(Body::from(OMITTED_BMC_ERROR_RESPONSE));
    }

    let Ok(body) = axum::body::to_bytes(body, MAX_REDACTABLE_RESPONSE_BODY_SIZE).await else {
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
    origins: &BmcOrigins,
    method: &Method,
    redirect_mode: RedirectMode,
    sensitive_values: &[String],
) -> Response<Body> {
    // A 3xx redirect with any malformed, unsafe, or duplicate `Location`
    // becomes a 502. Other responses keep their status and relay at most one
    // safe `Location`. Inspect all values so a safe first value cannot mask an
    // unsafe duplicate.
    let is_redirect = is_redirect_response(status);
    let mut validated_location = None;
    for value in headers.get_all(reqwest::header::LOCATION) {
        let (reason, target) = match redirect_location(value, origins, sensitive_values) {
            RedirectLocation::Relative(relative, target) if validated_location.is_none() => {
                validated_location = Some((relative, target));
                continue;
            }
            RedirectLocation::Relative(_, target) => {
                (RedirectSuppressionReason::DuplicateLocation, target)
            }
            RedirectLocation::Suppressed(reason, target) => (reason, target),
        };
        if is_redirect {
            emit(RedirectObserved {
                mode: redirect_mode,
                status: RedirectStatus::from(status),
                target,
                disposition: RedirectDisposition::Rejected,
            });
            tracing::warn!(
                method = %method,
                response_status = status.as_u16(),
                reason = reason.as_str(),
                "Upstream redirect rejected: Location could not be safely relayed",
            );
            return error_response(
                (
                    StatusCode::BAD_GATEWAY,
                    "redirect could not be safely relayed",
                )
                    .into(),
            );
        }
        tracing::warn!(
            method = %method,
            response_status = status.as_u16(),
            reason = reason.as_str(),
            "Upstream Location withheld: it could not be safely relayed",
        );
    }

    let mut response = Response::builder().status(status);
    if let Some((relative, target)) = validated_location {
        response = response.header(reqwest::header::LOCATION, relative);
        if is_redirect {
            emit(RedirectObserved {
                mode: redirect_mode,
                status: RedirectStatus::from(status),
                target,
                disposition: RedirectDisposition::Returned,
            });
        }
    }

    let body_was_rewritten = !matches!(&body, PreparedResponseBody::Unchanged(_));
    let body_was_replaced = matches!(&body, PreparedResponseBody::Replaced(_));
    let body = match body {
        PreparedResponseBody::Unchanged(body)
        | PreparedResponseBody::Redacted(body)
        | PreparedResponseBody::Replaced(body) => body,
    };
    for (name, value) in headers {
        if is_hop_by_hop_header(name.as_str())
            || name == reqwest::header::CONTENT_LENGTH
            || name == reqwest::header::LOCATION
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

/// Treat every 3xx other than cache-only 304 as a potential redirect. This
/// includes deprecated or unassigned statuses so they cannot bypass Location
/// validation or response-body scrubbing.
fn is_redirect_response(status: reqwest::StatusCode) -> bool {
    status.is_redirection() && status != reqwest::StatusCode::NOT_MODIFIED
}

/// The origins that can identify the destination BMC: the URL used for the
/// upstream request and the BMC's direct address. They differ when
/// `bmc_proxy` chains the request through another proxy.
pub(super) struct BmcOrigins {
    upstream: Url,
    bmc: Url,
}

impl BmcOrigins {
    /// Builds the origins that can legitimately name the destination BMC.
    /// The direct form uses HTTPS's default port because the `Forwarded`
    /// target contract carries only an IP address. A configured non-default
    /// direct port is already part of `upstream`; a chained proxy's port must
    /// not be attributed to the BMC.
    pub(super) fn new(upstream: Url, target_ip: IpAddr) -> Self {
        let mut bmc = Url::parse("https://0.0.0.0").expect("static HTTPS URL is valid");
        bmc.set_ip_host(target_ip)
            .expect("an HTTPS URL accepts an IP host");
        Self { upstream, bmc }
    }

    /// Classifies an HTTP(S) target as the request origin, the direct BMC origin,
    /// or a different origin. Other schemes are invalid.
    fn classify(&self, target: &Url) -> RedirectTarget {
        match classify_redirect_target(&self.upstream, target) {
            RedirectTarget::CrossOrigin
                if classify_redirect_target(&self.bmc, target) == RedirectTarget::SameOrigin =>
            {
                RedirectTarget::SameBmc
            }
            classification => classification,
        }
    }
}

/// How an upstream `Location` can be relayed to the proxy client.
#[derive(Debug, PartialEq)]
enum RedirectLocation {
    /// This BMC: strip the authority so a follow-up re-enters this proxy.
    Relative(http::HeaderValue, RedirectTarget),
    /// The target is malformed, ambiguous, unsupported, or external.
    Suppressed(RedirectSuppressionReason, RedirectTarget),
}

/// The bounded reason an upstream `Location` cannot be relayed safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RedirectSuppressionReason {
    InvalidHeaderValue,
    InvalidUriReference,
    UnsupportedScheme,
    CrossOrigin,
    AmbiguousPath,
    InvalidRelativeReference,
    DuplicateLocation,
    SensitiveValue,
}

impl RedirectSuppressionReason {
    /// Returns the stable, non-sensitive spelling recorded in logs.
    const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidHeaderValue => "invalid_header_value",
            Self::InvalidUriReference => "invalid_uri_reference",
            Self::UnsupportedScheme => "unsupported_scheme",
            Self::CrossOrigin => "cross_origin",
            Self::AmbiguousPath => "ambiguous_path",
            Self::InvalidRelativeReference => "invalid_relative_reference",
            Self::DuplicateLocation => "duplicate_location",
            Self::SensitiveValue => "sensitive_value",
        }
    }
}

/// Resolves an upstream `Location` against the response URL. A target naming
/// this BMC is rewritten as a relative reference so a client follow-up returns
/// through the proxy. Cross-origin, non-HTTP(S), malformed, and ambiguous
/// targets are withheld. An explicit fragment is preserved; an absent fragment
/// remains absent so HTTP's fragment-inheritance rule still applies.
fn redirect_location(
    value: &http::HeaderValue,
    origins: &BmcOrigins,
    sensitive_values: &[String],
) -> RedirectLocation {
    let Ok(raw) = value.to_str() else {
        return RedirectLocation::Suppressed(
            RedirectSuppressionReason::InvalidHeaderValue,
            RedirectTarget::Invalid,
        );
    };
    let Ok(location) = origins.upstream.join(raw) else {
        return RedirectLocation::Suppressed(
            RedirectSuppressionReason::InvalidUriReference,
            RedirectTarget::Invalid,
        );
    };
    let target = origins.classify(&location);
    match target {
        RedirectTarget::Invalid => {
            return RedirectLocation::Suppressed(
                RedirectSuppressionReason::UnsupportedScheme,
                target,
            );
        }
        RedirectTarget::CrossOrigin => {
            return RedirectLocation::Suppressed(RedirectSuppressionReason::CrossOrigin, target);
        }
        RedirectTarget::SameOrigin | RedirectTarget::SameBmc => {}
    }
    if location.path().starts_with("//") {
        return RedirectLocation::Suppressed(
            RedirectSuppressionReason::AmbiguousPath,
            RedirectTarget::Invalid,
        );
    }

    let mut relative = location.path().to_string();
    if let Some(query) = location.query() {
        relative.push('?');
        relative.push_str(query);
    }
    if let Some(fragment) = location.fragment() {
        relative.push('#');
        relative.push_str(fragment);
    }
    if contains_sensitive_value(&relative, sensitive_values) {
        return RedirectLocation::Suppressed(RedirectSuppressionReason::SensitiveValue, target);
    }
    match http::HeaderValue::from_str(&relative) {
        Ok(value) => RedirectLocation::Relative(value, target),
        Err(_) => RedirectLocation::Suppressed(
            RedirectSuppressionReason::InvalidRelativeReference,
            RedirectTarget::Invalid,
        ),
    }
}

/// Whether a relayed header would expose any credential representation.
/// Repeated decoding catches a secret hidden behind one or more `%HH` layers.
fn contains_sensitive_value(value: &str, sensitive_values: &[String]) -> bool {
    let contains_secret = |candidate: &str| {
        sensitive_values
            .iter()
            .any(|secret| !secret.is_empty() && candidate.contains(secret))
    };
    let mut candidate = Cow::Borrowed(value);
    loop {
        if contains_secret(&candidate) {
            return true;
        }
        let Ok(decoded) = urlencoding::decode(&candidate) else {
            return false;
        };
        if decoded == candidate {
            return false;
        }
        candidate = Cow::Owned(decoded.into_owned());
    }
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::net::IpAddr;

    use axum::body::Body;
    use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
    use bytes::Bytes;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::value_scenarios;
    use carbide_utils::redfish::redfish_basic_authorization_context;
    use http_body_util::BodyExt;
    use tokio_stream::iter;
    use url::Url;

    use super::{
        BmcOrigins, MAX_REDACTABLE_RESPONSE_BODY_SIZE, OMITTED_BMC_ERROR_RESPONSE,
        PreparedResponseBody, RedirectLocation, RedirectSuppressionReason,
        build_response as build_proxy_response, prepare_response_body, redirect_location,
    };
    use crate::config::RedirectMode;
    use crate::metrics::RedirectTarget;
    use crate::proxy::credentials::BmcCredentials;

    fn origins(upstream: &str) -> BmcOrigins {
        let upstream = Url::parse(upstream).expect("upstream URL");
        let ip: IpAddr = upstream
            .host_str()
            .expect("upstream host")
            .trim_matches(['[', ']'])
            .parse()
            .expect("upstream IP literal");
        BmcOrigins::new(upstream, ip)
    }

    fn build_response(
        status: reqwest::StatusCode,
        headers: &reqwest::header::HeaderMap,
        body: PreparedResponseBody,
    ) -> http::Response<Body> {
        build_proxy_response(
            status,
            headers,
            body,
            &origins("https://192.0.2.5/redfish/v1"),
            &Method::GET,
            RedirectMode::FollowSameOrigin,
            &[],
        )
    }

    #[test]
    fn locations_are_rewritten_only_when_they_name_this_bmc() {
        value_scenarios!(
            run = |(upstream, bmc, location): (&str, &str, &str)| {
                let origins = BmcOrigins::new(
                    Url::parse(upstream).expect("upstream URL"),
                    bmc.parse().expect("BMC IP"),
                );
                redirect_location(
                    &HeaderValue::from_str(location).expect("Location header"),
                    &origins,
                    &[],
                )
            };

            "relative target stays on the upstream origin" {
                ("https://192.0.2.5/redfish/v1", "192.0.2.5", "Systems?expand=1#Status")
                    => RedirectLocation::Relative(
                        HeaderValue::from_static("/redfish/Systems?expand=1#Status"),
                        RedirectTarget::SameOrigin,
                    ),
            }

            "absolute upstream target becomes relative" {
                ("https://192.0.2.5/redfish/v1", "192.0.2.5", "https://192.0.2.5/redfish/v1/Systems")
                    => RedirectLocation::Relative(
                        HeaderValue::from_static("/redfish/v1/Systems"),
                        RedirectTarget::SameOrigin,
                    ),
            }

            "non-default direct BMC port stays in the upstream origin" {
                ("https://192.0.2.5:8443/redfish/v1", "192.0.2.5", "https://192.0.2.5:8443/redfish/v1/Systems")
                    => RedirectLocation::Relative(
                        HeaderValue::from_static("/redfish/v1/Systems"),
                        RedirectTarget::SameOrigin,
                    ),
            }

            "direct BMC target behind a chained proxy becomes relative" {
                ("https://proxy.local:8443/redfish/v1", "192.0.2.5", "https://192.0.2.5/redfish/v1/Systems")
                    => RedirectLocation::Relative(
                        HeaderValue::from_static("/redfish/v1/Systems"),
                        RedirectTarget::SameBmc,
                    ),
            }

            "a chained proxy port is not attributed to the BMC" {
                ("https://proxy.local:8443/redfish/v1", "192.0.2.5", "https://192.0.2.5:8443/redfish/v1/Systems")
                    => RedirectLocation::Suppressed(
                        RedirectSuppressionReason::CrossOrigin,
                        RedirectTarget::CrossOrigin,
                    ),
            }

            "another host is refused" {
                ("https://192.0.2.5/redfish/v1", "192.0.2.5", "https://example.com/collect?token=secret")
                    => RedirectLocation::Suppressed(
                        RedirectSuppressionReason::CrossOrigin,
                        RedirectTarget::CrossOrigin,
                    ),
            }

            "another port is refused" {
                ("https://192.0.2.5/redfish/v1", "192.0.2.5", "https://192.0.2.5:8443/redfish/v1")
                    => RedirectLocation::Suppressed(
                        RedirectSuppressionReason::CrossOrigin,
                        RedirectTarget::CrossOrigin,
                    ),
            }

            "non-HTTP scheme is refused" {
                ("https://192.0.2.5/redfish/v1", "192.0.2.5", "javascript:alert(1)")
                    => RedirectLocation::Suppressed(
                        RedirectSuppressionReason::UnsupportedScheme,
                        RedirectTarget::Invalid,
                    ),
            }

            "authority-like rewritten path is refused" {
                ("https://192.0.2.5/redfish/v1", "192.0.2.5", "https://192.0.2.5//169.254.169.254/x")
                    => RedirectLocation::Suppressed(
                        RedirectSuppressionReason::AmbiguousPath,
                        RedirectTarget::Invalid,
                    ),
            }
        );
    }

    #[test]
    fn every_non_cache_3xx_is_treated_as_a_redirect() {
        value_scenarios!(
            run = super::is_redirect_response;

            "temporary redirect" {
                reqwest::StatusCode::TEMPORARY_REDIRECT => true,
            }

            "deprecated use-proxy response" {
                reqwest::StatusCode::USE_PROXY => true,
            }

            "unassigned 3xx response" {
                reqwest::StatusCode::from_u16(399).expect("valid 3xx status") => true,
            }

            "cache validation response" {
                reqwest::StatusCode::NOT_MODIFIED => false,
            }

            "successful response" {
                reqwest::StatusCode::OK => false,
            }
        );
    }

    #[test]
    fn safe_redirect_is_returned_relative_and_counted_once() {
        let metrics = MetricsCapture::start();
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            HeaderValue::from_static("https://192.0.2.5/redfish/v1/Systems"),
        );

        let response = build_proxy_response(
            reqwest::StatusCode::TEMPORARY_REDIRECT,
            &headers,
            PreparedResponseBody::Unchanged(Body::empty()),
            &origins("https://192.0.2.5/redfish/v1"),
            &Method::GET,
            RedirectMode::ReturnToClient,
            &[],
        );

        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers().get(reqwest::header::LOCATION),
            Some(&HeaderValue::from_static("/redfish/v1/Systems"))
        );
        assert_eq!(
            metrics.counter_delta(
                "carbide_bmc_proxy_redirects_total",
                &[
                    ("mode", "return_to_client"),
                    ("status", "307"),
                    ("target", "same_origin"),
                    ("disposition", "returned"),
                ],
            ),
            1.0
        );
    }

    #[test]
    fn external_redirect_is_rejected_without_logging_its_location() {
        let metrics = MetricsCapture::start();
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            HeaderValue::from_static(
                "https://user:password@example.com/private?token=secret#fragment",
            ),
        );

        let mut response = None;
        let logs = capture_logs(|| {
            response = Some(build_proxy_response(
                reqwest::StatusCode::FOUND,
                &headers,
                PreparedResponseBody::Unchanged(Body::empty()),
                &origins("https://192.0.2.5/redfish/v1"),
                &Method::GET,
                RedirectMode::FollowSameOrigin,
                &[],
            ));
        });
        let response = response.expect("response built");

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(!response.headers().contains_key(reqwest::header::LOCATION));
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].field("reason"), Some("cross_origin"));
        let rendered_logs = format!("{logs:?}");
        for secret in ["user", "password", "private", "token", "secret", "fragment"] {
            assert!(!rendered_logs.contains(secret), "Location leaked: {secret}");
        }
        assert_eq!(
            metrics.counter_delta(
                "carbide_bmc_proxy_redirects_total",
                &[
                    ("mode", "follow_same_origin"),
                    ("status", "302"),
                    ("target", "cross_origin"),
                    ("disposition", "rejected"),
                ],
            ),
            1.0
        );
    }

    #[test]
    fn credential_bearing_locations_are_never_relayed() {
        let metrics = MetricsCapture::start();
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            HeaderValue::from_static("/redfish/v1/Systems?session=%2574oken-123"),
        );
        let sensitive_values = ["token-123".to_string()];

        let mut redirect = None;
        let logs = capture_logs(|| {
            redirect = Some(build_proxy_response(
                reqwest::StatusCode::FOUND,
                &headers,
                PreparedResponseBody::Unchanged(Body::empty()),
                &origins("https://192.0.2.5/redfish/v1"),
                &Method::GET,
                RedirectMode::ReturnToClient,
                &sensitive_values,
            ));
        });
        let redirect = redirect.expect("response built");
        assert_eq!(redirect.status(), StatusCode::BAD_GATEWAY);
        assert!(!redirect.headers().contains_key(reqwest::header::LOCATION));
        assert_eq!(logs.len(), 1);
        assert_eq!(logs[0].field("reason"), Some("sensitive_value"));
        assert!(!format!("{logs:?}").contains("token-123"));
        assert_eq!(
            metrics.counter_delta(
                "carbide_bmc_proxy_redirects_total",
                &[
                    ("mode", "return_to_client"),
                    ("status", "302"),
                    ("target", "same_origin"),
                    ("disposition", "rejected"),
                ],
            ),
            1.0
        );

        let created = build_proxy_response(
            reqwest::StatusCode::CREATED,
            &headers,
            PreparedResponseBody::Unchanged(Body::empty()),
            &origins("https://192.0.2.5/redfish/v1"),
            &Method::POST,
            RedirectMode::ReturnToClient,
            &sensitive_values,
        );
        assert_eq!(created.status(), StatusCode::CREATED);
        assert!(!created.headers().contains_key(reqwest::header::LOCATION));
    }

    #[test]
    fn unsafe_duplicate_location_rejects_the_redirect() {
        let metrics = MetricsCapture::start();
        let mut headers = HeaderMap::new();
        headers.append(
            reqwest::header::LOCATION,
            HeaderValue::from_static("/redfish/v1/Systems"),
        );
        headers.append(
            reqwest::header::LOCATION,
            HeaderValue::from_static("https://example.com/private"),
        );

        let response = build_proxy_response(
            reqwest::StatusCode::FOUND,
            &headers,
            PreparedResponseBody::Unchanged(Body::empty()),
            &origins("https://192.0.2.5/redfish/v1"),
            &Method::GET,
            RedirectMode::ReturnToClient,
            &[],
        );

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(!response.headers().contains_key(reqwest::header::LOCATION));
        assert_eq!(
            metrics.counter_delta(
                "carbide_bmc_proxy_redirects_total",
                &[
                    ("mode", "return_to_client"),
                    ("status", "302"),
                    ("target", "cross_origin"),
                    ("disposition", "rejected"),
                ],
            ),
            1.0
        );
    }

    #[test]
    fn safe_duplicate_locations_are_never_relayed_ambiguously() {
        let metrics = MetricsCapture::start();
        let mut headers = HeaderMap::new();
        headers.append(
            reqwest::header::LOCATION,
            HeaderValue::from_static("/redfish/v1/Systems/1"),
        );
        headers.append(
            reqwest::header::LOCATION,
            HeaderValue::from_static("/redfish/v1/Systems/2"),
        );

        let redirect = build_proxy_response(
            reqwest::StatusCode::FOUND,
            &headers,
            PreparedResponseBody::Unchanged(Body::empty()),
            &origins("https://192.0.2.5/redfish/v1"),
            &Method::GET,
            RedirectMode::ReturnToClient,
            &[],
        );
        assert_eq!(redirect.status(), StatusCode::BAD_GATEWAY);
        assert!(!redirect.headers().contains_key(reqwest::header::LOCATION));
        assert_eq!(
            metrics.counter_delta(
                "carbide_bmc_proxy_redirects_total",
                &[
                    ("mode", "return_to_client"),
                    ("status", "302"),
                    ("target", "same_origin"),
                    ("disposition", "rejected"),
                ],
            ),
            1.0
        );

        let created = build_proxy_response(
            reqwest::StatusCode::CREATED,
            &headers,
            PreparedResponseBody::Unchanged(Body::empty()),
            &origins("https://192.0.2.5/redfish/v1"),
            &Method::POST,
            RedirectMode::ReturnToClient,
            &[],
        );
        assert_eq!(created.status(), StatusCode::CREATED);
        assert_eq!(
            created
                .headers()
                .get_all(reqwest::header::LOCATION)
                .iter()
                .count(),
            1
        );
        assert_eq!(
            created.headers().get(reqwest::header::LOCATION),
            Some(&HeaderValue::from_static("/redfish/v1/Systems/1"))
        );
    }

    #[test]
    fn identical_duplicate_locations_are_rejected() {
        let _metrics = MetricsCapture::start();
        let mut headers = HeaderMap::new();
        for _ in 0..2 {
            headers.append(
                reqwest::header::LOCATION,
                HeaderValue::from_static("/redfish/v1/Systems/1"),
            );
        }

        let response = build_response(
            reqwest::StatusCode::FOUND,
            &headers,
            PreparedResponseBody::Unchanged(Body::empty()),
        );

        assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
        assert!(!response.headers().contains_key(reqwest::header::LOCATION));
    }

    #[test]
    fn non_redirect_response_keeps_its_status_and_withholds_external_location() {
        value_scenarios!(
            run = |locations: &[&str]| {
                let metrics = MetricsCapture::start();
                let mut headers = HeaderMap::new();
                for location in locations {
                    headers.append(
                        reqwest::header::LOCATION,
                        HeaderValue::from_str(location).expect("Location header"),
                    );
                }

                let response = build_proxy_response(
                    reqwest::StatusCode::CREATED,
                    &headers,
                    PreparedResponseBody::Unchanged(Body::empty()),
                    &origins("https://192.0.2.5/redfish/v1"),
                    &Method::POST,
                    RedirectMode::FollowSameOrigin,
                    &[],
                );

                assert_eq!(response.status(), StatusCode::CREATED);
                assert_eq!(
                    metrics.counter_delta(
                        "carbide_bmc_proxy_redirects_total",
                        &[
                            ("mode", "follow_same_origin"),
                            ("status", "other"),
                            ("target", "cross_origin"),
                            ("disposition", "rejected"),
                        ],
                    ),
                    0.0
                );
                response
                    .headers()
                    .get_all(reqwest::header::LOCATION)
                    .iter()
                    .map(|value| value.to_str().expect("relative Location").to_owned())
                    .collect::<Vec<_>>()
            };

            "external Location is withheld" {
                &["https://example.com/private"][..] => Vec::<String>::new(),
            }

            "safe Location survives surrounding unsafe values" {
                &[
                    "https://example.com/private",
                    "https://192.0.2.5/redfish/v1/Systems/1",
                    "https://example.com/other",
                ][..] => vec!["/redfish/v1/Systems/1".to_string()],
            }
        );
    }

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
    async fn returned_redirect_body_redacts_the_upstream_credential() {
        let _metrics = MetricsCapture::start();
        let mut headers = HeaderMap::new();
        headers.insert(
            reqwest::header::LOCATION,
            HeaderValue::from_static("/redfish/v1/Systems"),
        );
        let prepared = prepare_response_body(
            reqwest::StatusCode::TEMPORARY_REDIRECT,
            &headers,
            Body::from("session token-123 moved"),
            &["token-123".to_string()],
        )
        .await;

        assert!(matches!(&prepared, PreparedResponseBody::Redacted(_)));
        let response = build_response(reqwest::StatusCode::TEMPORARY_REDIRECT, &headers, prepared);
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(
            response.headers().get(reqwest::header::LOCATION),
            Some(&HeaderValue::from_static("/redfish/v1/Systems"))
        );
        let body = response.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(body, Bytes::from_static(b"session REDACTED moved"));
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
        let mut body = vec![b'x'; MAX_REDACTABLE_RESPONSE_BODY_SIZE + 1];
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
