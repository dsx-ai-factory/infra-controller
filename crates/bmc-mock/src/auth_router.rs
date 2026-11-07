// Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::WWW_AUTHENTICATE;
use axum::http::{HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::any;
use axum_extra::TypedHeader;
use axum_extra::headers::Authorization;
use axum_extra::headers::authorization::Basic;
use carbide_axum_utils::router::call_router_with_new_request;
use tracing::instrument;

use crate::redfish::account_service::AccountServiceState;
use crate::redfish::session_service::SessionServiceState;
use crate::redfish::{account_service, service_root, session_service};

const WWW_AUTHENTICATE_VALUE: HeaderValue = HeaderValue::from_static("Basic realm=\"bmc-mock\"");
const X_AUTH_TOKEN_HEADER: &str = "x-auth-token";

pub(super) fn append(router: Router, authorizer: Authorizer) -> Router {
    let service_root_path = service_root::resource().odata_id.to_string();
    let service_root_path_with_trailing_slash = format!("{service_root_path}/");
    let account_service_path = account_service::resource().odata_id.to_string();
    let account_service_subtree_path = format!("{account_service_path}/{{*all}}");
    let sessions_collection_path = session_service::sessions_collection().odata_id.to_string();

    Router::new()
        .route(&service_root_path, any(process_without_auth))
        .route(
            &service_root_path_with_trailing_slash,
            any(process_without_auth),
        )
        .route(&sessions_collection_path, any(process_sessions_collection))
        .route(&account_service_path, any(process_account_service))
        .route(&account_service_subtree_path, any(process_account_service))
        .route("/{*all}", any(process))
        .with_state(AuthMiddleware {
            inner: router,
            authorizer,
        })
}

#[instrument(skip_all)]
async fn process_without_auth(
    State(mut state): State<AuthMiddleware>,
    request: Request<Body>,
) -> Response {
    state.call_inner_router(request).await
}

#[instrument(skip_all)]
async fn process_sessions_collection(
    State(mut state): State<AuthMiddleware>,
    authorization: Option<TypedHeader<Authorization<Basic>>>,
    request: Request<Body>,
) -> Response {
    if request.method() == Method::POST {
        return state.call_inner_router(request).await;
    }
    match state
        .authorizer
        .authorize(&request, authorization.as_ref(), true)
    {
        AuthorizationResult::Authorized => state.call_inner_router(request).await,
        AuthorizationResult::Unauthorized => unauthorized_log_and_response(request),
        AuthorizationResult::FactoryDefaultPasswordForbidden => {
            unreachable!(
                "session collection authorization does not forbid factory-default passwords"
            )
        }
    }
}

#[instrument(skip_all)]
async fn process_account_service(
    State(mut state): State<AuthMiddleware>,
    authorization: Option<TypedHeader<Authorization<Basic>>>,
    request: Request<Body>,
) -> Response {
    match state
        .authorizer
        .authorize(&request, authorization.as_ref(), true)
    {
        AuthorizationResult::Authorized => state.call_inner_router(request).await,
        AuthorizationResult::Unauthorized => unauthorized_log_and_response(request),
        AuthorizationResult::FactoryDefaultPasswordForbidden => {
            unreachable!("account service authorization does not forbid factory-default passwords")
        }
    }
}

#[instrument(skip_all)]
async fn process(
    State(mut state): State<AuthMiddleware>,
    authorization: Option<TypedHeader<Authorization<Basic>>>,
    request: Request<Body>,
) -> Response {
    match state
        .authorizer
        .authorize(&request, authorization.as_ref(), false)
    {
        AuthorizationResult::Authorized => state.call_inner_router(request).await,
        AuthorizationResult::Unauthorized => unauthorized_log_and_response(request),
        AuthorizationResult::FactoryDefaultPasswordForbidden => {
            tracing::warn!(
                method = request.method().as_str(),
                path = request.uri().path(),
                "Factory-default password must be changed before accessing this resource",
            );
            StatusCode::FORBIDDEN.into_response()
        }
    }
}

fn unauthorized_log_and_response(request: Request<Body>) -> Response {
    tracing::warn!(
        method = request.method().as_str(),
        path = request.uri().path(),
        "Unauthorized request",
    );
    unauthorized_response()
}

fn unauthorized_response() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(WWW_AUTHENTICATE, WWW_AUTHENTICATE_VALUE)],
    )
        .into_response()
}

#[derive(Clone)]
struct AuthMiddleware {
    inner: Router,
    authorizer: Authorizer,
}

impl AuthMiddleware {
    async fn call_inner_router(&mut self, request: Request<Body>) -> Response {
        call_router_with_new_request(&mut self.inner, request).await
    }
}

#[derive(Clone)]
pub(super) struct Authorizer {
    account_service_state: Arc<AccountServiceState>,
    session_service_state: Arc<SessionServiceState>,
    forbid_factory_default_password: bool,
}

impl Authorizer {
    /// Builds the factory-default authorizer for a mock BMC state.
    pub(super) fn new(
        account_service_state: Arc<AccountServiceState>,
        session_service_state: Arc<SessionServiceState>,
    ) -> Self {
        Self {
            account_service_state,
            session_service_state,
            forbid_factory_default_password: true,
        }
    }

    pub(super) fn permit_factory_default_password(mut self) -> Self {
        self.forbid_factory_default_password = false;
        self
    }

    fn authorize(
        &self,
        request: &Request<Body>,
        authorization: Option<&TypedHeader<Authorization<Basic>>>,
        permit_factory_default: bool,
    ) -> AuthorizationResult {
        if let Some(token) = request
            .headers()
            .get(X_AUTH_TOKEN_HEADER)
            .and_then(|v| v.to_str().ok())
            && self.session_service_state.is_token_valid(token)
        {
            return AuthorizationResult::Authorized;
        }

        let Some(authorization) = authorization else {
            return AuthorizationResult::Unauthorized;
        };
        let actual = &authorization.0.0;
        if self
            .account_service_state
            .is_authorized(actual.username(), actual.password())
        {
            if self.forbid_factory_default_password
                && !permit_factory_default
                && self
                    .account_service_state
                    .is_factory_default_password(actual.username(), actual.password())
            {
                AuthorizationResult::FactoryDefaultPasswordForbidden
            } else {
                AuthorizationResult::Authorized
            }
        } else {
            AuthorizationResult::Unauthorized
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AuthorizationResult {
    Authorized,
    Unauthorized,
    FactoryDefaultPasswordForbidden,
}
