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

//! Who a caller is and what it may send: its identity from its client
//! certificate, whether it may use the proxy at all (the principal
//! allow-list), and whether it may send this request (the ACL).

use axum::body::Body;
use axum::extract::State;
use axum::middleware::Next;
use carbide_authn::SpiffeContext;
use carbide_authn::middleware::{AuthContext, Authorization, CertDescriptionMiddleware, Principal};
use carbide_instrument::emit;
use http::{Request, Response, StatusCode};

use crate::config::AuthConfig;
use crate::metrics::{AuthContextMissing, PrincipalAllowListDenied, RequestAclDenied};
use crate::proxy::{BmcProxyError, BmcProxyState};

impl BmcProxyState {
    /// The caller's principal identifiers, when the ACL lets one of them send
    /// `request`.
    pub(super) fn authorized_caller(&self, request: &Request<Body>) -> Option<Vec<String>> {
        let Some(auth_context) = request.extensions().get::<AuthContext<()>>() else {
            emit(AuthContextMissing::RequestAcl {
                method_label: request.method().into(),
            });
            return None;
        };

        let principal_ids = request_principal_ids(auth_context);
        let allowed = principal_ids.iter().any(|principal| {
            self.config
                .auth
                .acls
                .allows(principal, request.method(), request.uri().path())
        });

        if !allowed {
            emit(RequestAclDenied::new(
                request.method(),
                format!("{principal_ids:?}"),
                request.uri().path().to_string(),
            ));
            return None;
        }

        Some(principal_ids)
    }
}

pub(super) async fn authorize_proxy_request(
    State(state): State<BmcProxyState>,
    request: Request<Body>,
    next: Next,
) -> Result<Response<Body>, StatusCode> {
    authorize_principal_allow_list(&state, &request)?;
    Ok(next.run(request).await)
}

fn authorize_principal_allow_list(
    state: &BmcProxyState,
    request: &Request<Body>,
) -> Result<(), StatusCode> {
    let auth_context = request
        .extensions()
        .get::<AuthContext<()>>()
        .ok_or_else(|| {
            emit(AuthContextMissing::PrincipalAllowList {
                method_label: request.method().into(),
            });
            StatusCode::INTERNAL_SERVER_ERROR
        })?;

    let present_principals = request_principal_ids(auth_context);

    let allowed = present_principals
        .iter()
        .any(|principal| state.config.allowed_principals.contains(principal));

    if allowed {
        Ok(())
    } else {
        emit(PrincipalAllowListDenied::new(
            request.method(),
            format!("{:?}", state.config.allowed_principals),
            format!("{present_principals:?}"),
            request.uri().path().to_string(),
        ));
        Err(StatusCode::FORBIDDEN)
    }
}

fn request_principal_ids(auth_context: &AuthContext<()>) -> Vec<String> {
    let mut principals = auth_context
        .principals
        .iter()
        .map(Principal::as_identifier)
        .collect::<Vec<_>>();
    principals.push(Principal::Anonymous.as_identifier());
    principals
}

pub(super) fn cert_description_layer<AZ: Authorization>(
    auth_config: &AuthConfig,
) -> Result<CertDescriptionMiddleware<AZ>, BmcProxyError> {
    tracing::info!(trust_config = ?auth_config.trust, "TrustConfig rendered from config");
    let spiffe_context = SpiffeContext::try_from(auth_config.trust.clone()).map_err(|e| {
        BmcProxyError::InvalidConfiguration(format!(
            "Invalid trust config in bmc-proxy config toml file: {e}"
        ))
    })?;

    Ok(CertDescriptionMiddleware::new(
        auth_config.cli_certs.clone(),
        spiffe_context,
    ))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use carbide_authn::middleware::{AuthContext, ExternalUserInfo, Principal};
    use carbide_instrument::LabelValue;
    use carbide_instrument::testing::{MetricsCapture, capture_logs};
    use carbide_test_support::{Check, check_values, value_scenarios};

    use super::{BmcProxyState, authorize_principal_allow_list, request_principal_ids};
    use crate::metrics::MethodLabel;
    use crate::proxy::test_support::*;

    const AUTHORIZATION_TEST_CONFIG: &str = r#"
        allowed_principals = ["spiffe-service-id/forge-system/carbide-api"]

        [tls]
        identity_pemfile_path = ""
        identity_keyfile_path = ""
        root_cafile_path = ""
        admin_root_cafile_path = ""

        [auth]

        [auth.acls]
        "spiffe-service-id/forge-system/carbide-api" = ["GET /redfish/v1/**"]
    "#;

    const AUTHORIZATION_DENIED_METRIC: &str = "carbide_bmc_proxy_authorization_denied_total";
    const AUTHORIZATION_ERROR_METRIC: &str = "carbide_bmc_proxy_authorization_errors_total";

    struct AuthorizationRequestCase {
        method: Method,
        path: &'static str,
        principals: Option<Vec<Principal>>,
    }

    #[derive(Debug, PartialEq)]
    struct AuthorizationObservation<T> {
        result: T,
        denial_delta: f64,
        error_delta: f64,
        event_names: Vec<String>,
    }

    fn authorization_request(input: AuthorizationRequestCase) -> Request<Body> {
        let mut request = Request::builder()
            .method(input.method)
            .uri(input.path)
            .body(Body::empty())
            .expect("authorization test request should build");
        if let Some(principals) = input.principals {
            request.extensions_mut().insert(AuthContext::<()> {
                principals,
                authorization: None,
            });
        }
        request
    }

    fn authorization_event_names(logs: &[carbide_instrument::testing::CapturedLog]) -> Vec<String> {
        logs.iter()
            .filter_map(|log| log.field("event_name").map(str::to_owned))
            .collect()
    }

    fn observe_request_acl(
        state: &BmcProxyState,
        input: AuthorizationRequestCase,
    ) -> AuthorizationObservation<bool> {
        let method_label = MethodLabel::from(&input.method).label_value().to_string();
        let request = authorization_request(input);
        let labels = [
            ("authorization_layer", "request_acl"),
            ("method", method_label.as_str()),
        ];
        let metrics = MetricsCapture::start();
        let mut result = false;
        let logs = capture_logs(|| result = state.authorized_caller(&request).is_some());

        AuthorizationObservation {
            result,
            denial_delta: metrics.counter_delta(AUTHORIZATION_DENIED_METRIC, &labels),
            error_delta: metrics.counter_delta(AUTHORIZATION_ERROR_METRIC, &labels),
            event_names: authorization_event_names(&logs),
        }
    }

    fn observe_principal_allow_list(
        state: &BmcProxyState,
        input: AuthorizationRequestCase,
    ) -> AuthorizationObservation<Result<(), StatusCode>> {
        let method_label = MethodLabel::from(&input.method).label_value().to_string();
        let request = authorization_request(input);
        let labels = [
            ("authorization_layer", "principal_allow_list"),
            ("method", method_label.as_str()),
        ];
        let metrics = MetricsCapture::start();
        let mut result = Ok(());
        let logs = capture_logs(|| result = authorize_principal_allow_list(state, &request));

        AuthorizationObservation {
            result,
            denial_delta: metrics.counter_delta(AUTHORIZATION_DENIED_METRIC, &labels),
            error_delta: metrics.counter_delta(AUTHORIZATION_ERROR_METRIC, &labels),
            event_names: authorization_event_names(&logs),
        }
    }

    #[test]
    fn request_principal_identifiers_include_anonymous_fallback() {
        value_scenarios!(
            run = |principals| {
                request_principal_ids(&AuthContext {
                    principals,
                    authorization: None,
                })
            };
            "no authenticated principals" {
                vec![] => vec!["anonymous".to_string()],
            }

            "service principal" {
                vec![Principal::SpiffeServiceIdentifier(
                    "forge-system/carbide-api".to_string(),
                )] => vec![
                    "spiffe-service-id/forge-system/carbide-api".to_string(),
                    "anonymous".to_string(),
                ],
            }

            "machine and external user principals" {
                vec![
                    // Machine identities currently authorize by type token;
                    // the concrete machine id is intentionally not included.
                    Principal::SpiffeMachineIdentifier("machine-1".to_string()),
                    Principal::ExternalUser(ExternalUserInfo::new(
                        Some("nvidia".to_string()),
                        "admin".to_string(),
                        Some("chet".to_string()),
                    )),
                ] => vec![
                    "spiffe-machine-id".to_string(),
                    "external-role/admin".to_string(),
                    "anonymous".to_string(),
                ],
            }
        );
    }

    /// `BmcProxyState::authorized_caller` owns the per-principal ACL boundary. An ordinary
    /// policy rejection moves the denial counter, while a missing `AuthContext`
    /// still rejects the request but moves only the middleware-error counter.
    #[test]
    fn request_acl_authorization_emits_the_matching_event() {
        let state = test_state_with_config(AUTHORIZATION_TEST_CONFIG);
        let service_principal =
            || Principal::SpiffeServiceIdentifier("forge-system/carbide-api".to_string());

        check_values(
            [
                Check {
                    scenario: "configured principal and path are allowed",
                    input: AuthorizationRequestCase {
                        method: Method::GET,
                        path: "/redfish/v1/Systems/1",
                        principals: Some(vec![service_principal()]),
                    },
                    expect: AuthorizationObservation {
                        result: true,
                        denial_delta: 0.0,
                        error_delta: 0.0,
                        event_names: vec![],
                    },
                },
                Check {
                    scenario: "configured principal with denied method",
                    input: AuthorizationRequestCase {
                        method: Method::POST,
                        path: "/redfish/v1/Systems/1",
                        principals: Some(vec![service_principal()]),
                    },
                    expect: AuthorizationObservation {
                        result: false,
                        denial_delta: 1.0,
                        error_delta: 0.0,
                        event_names: vec!["bmc_proxy_request_acl_denied".to_string()],
                    },
                },
                Check {
                    scenario: "authentication context is missing",
                    input: AuthorizationRequestCase {
                        method: Method::DELETE,
                        path: "/redfish/v1/Systems/1",
                        principals: None,
                    },
                    expect: AuthorizationObservation {
                        result: false,
                        denial_delta: 0.0,
                        error_delta: 1.0,
                        event_names: vec!["bmc_proxy_auth_context_missing".to_string()],
                    },
                },
            ],
            |input| observe_request_acl(&state, input),
        );
    }

    /// The outer allow-list returns 403 only for a real policy rejection. A
    /// request that never passed through authentication keeps its existing 500
    /// response and is counted as an authorization wiring error instead.
    #[test]
    fn principal_allow_list_authorization_emits_the_matching_event() {
        let state = test_state_with_config(AUTHORIZATION_TEST_CONFIG);
        let service_principal =
            || Principal::SpiffeServiceIdentifier("forge-system/carbide-api".to_string());

        check_values(
            [
                Check {
                    scenario: "configured principal is allowed",
                    input: AuthorizationRequestCase {
                        method: Method::GET,
                        path: "/redfish/v1",
                        principals: Some(vec![service_principal()]),
                    },
                    expect: AuthorizationObservation {
                        result: Ok(()),
                        denial_delta: 0.0,
                        error_delta: 0.0,
                        event_names: vec![],
                    },
                },
                Check {
                    scenario: "principal is not on the allow-list",
                    input: AuthorizationRequestCase {
                        method: Method::PATCH,
                        path: "/redfish/v1",
                        principals: Some(vec![Principal::TrustedCertificate]),
                    },
                    expect: AuthorizationObservation {
                        result: Err(StatusCode::FORBIDDEN),
                        denial_delta: 1.0,
                        error_delta: 0.0,
                        event_names: vec!["bmc_proxy_principal_allow_list_denied".to_string()],
                    },
                },
                Check {
                    scenario: "authentication context is missing",
                    input: AuthorizationRequestCase {
                        method: Method::OPTIONS,
                        path: "/redfish/v1",
                        principals: None,
                    },
                    expect: AuthorizationObservation {
                        result: Err(StatusCode::INTERNAL_SERVER_ERROR),
                        denial_delta: 0.0,
                        error_delta: 1.0,
                        event_names: vec!["bmc_proxy_auth_context_missing".to_string()],
                    },
                },
            ],
            |input| observe_principal_allow_list(&state, input),
        );
    }
}
