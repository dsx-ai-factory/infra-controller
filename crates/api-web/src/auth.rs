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
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::Extension;
use axum::extract::{Query, State as AxumState};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum_extra::extract::cookie::{Cookie, PrivateCookieJar};
use base64::Engine;
use base64::prelude::BASE64_URL_SAFE_NO_PAD;
use carbide_api_core::Api;
use http::HeaderMap;
use oauth2::http::HeaderValue as Oauth2HeaderValue;
use oauth2::{
    AsyncHttpClient, AuthorizationCode, ClientSecret, HttpRequest, PkceCodeVerifier, Scope,
    TokenResponse,
};
use serde::Deserialize;
use time::Duration;

use crate::Oauth2Layer;

lazy_static::lazy_static! {
    static ref CONTENT_TYPE_APPLICATION_FORM_URL_ENCODED: Oauth2HeaderValue = Oauth2HeaderValue::from_str("application/x-www-form-urlencoded").unwrap();
    static ref MS_GRAPH_CONSISTENCY_LEVEL_EVENTUAL: Oauth2HeaderValue = Oauth2HeaderValue::from_str("eventual").unwrap();
}

const GRAPH_USER_GROUPS_ENDPOINT: &str = "https://graph.microsoft.com/v1.0/users";
const CLIENT_SECRET_HEADER: &str = "client_secret";

const CLIENT_CREDENTIALS_FLOW_SESSION_EXPIRATION_SECONDS: u32 = 600;

#[derive(Debug, Deserialize)]
pub(super) struct AuthRequest {
    code: Option<String>,
    state: Option<String>,
}

pub(super) async fn callback(
    AxumState(_state): AxumState<Arc<Api>>,
    request_headers: HeaderMap,
    Query(query): Query<AuthRequest>,
    Extension(oauth2_layer): Extension<Option<Arc<Oauth2Layer>>>,
) -> AuthCallbackResponse {
    use AuthCallbackError::*;
    let Some(oauth2_layer) = oauth2_layer else {
        return EmptyOauth2Layer.into();
    };

    let cookiejar: PrivateCookieJar = PrivateCookieJar::from_headers(
        &request_headers,
        oauth2_layer.private_cookiejar_key.clone(),
    );

    // See if the caller is really some other app/script
    // calling in with a secret we assigned to it.
    // If it is, we're going to just validate the secret
    // and then drop the cookie.
    if let Some(client_secret) = request_headers.get(CLIENT_SECRET_HEADER) {
        let Ok(client_secret) = client_secret.to_str() else {
            return (StatusCode::BAD_REQUEST, "invalid client secret format").into();
        };

        let client_id = oauth2_layer.client.client_id().as_str().to_owned();
        // We don't actually care about the token response right now.
        // What matters is that we received one with the proper structure,
        // which includes an access token.
        let _ = match oauth2_layer
            .client
            .clone()
            .set_client_secret(ClientSecret::new(client_secret.to_string()))
            .exchange_client_credentials()
            .add_scope(Scope::new(format!("{client_id}/.default")))
            .request_async(&AsyncRequestHandlerWithTimeouts::new(
                &oauth2_layer.http_client,
            ))
            .await
        {
            Ok(s) => s,
            Err(e) => {
                return BadClientCredentialsTokenResponse(e.to_string()).into();
            }
        };

        let now_seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("implausible future date")
            .as_secs();

        let cookie = Cookie::build((
            "sid",
            format!(
                "{}",
                now_seconds + CLIENT_CREDENTIALS_FLOW_SESSION_EXPIRATION_SECONDS as u64
            ),
        ))
        .path("/")
        .secure(true)
        .http_only(true)
        .max_age(Duration::seconds(
            CLIENT_CREDENTIALS_FLOW_SESSION_EXPIRATION_SECONDS as i64,
        ))
        .build();

        return (
            // Strip out any old cookies that might possibly exist,
            // add in the new sid cookie, and send it along.
            cookiejar.remove(cookie.clone()).add(cookie),
            Redirect::to("/admin/"),
        )
            .into_response()
            .into();
    }

    let Some(query_state) = query.state else {
        return (
            StatusCode::BAD_REQUEST,
            "'state' parameter required for MFA flow",
        )
            .into();
    };

    let Some(query_code) = query.code else {
        return (StatusCode::BAD_REQUEST, "'code' required for MFA flow").into();
    };

    // Grab the csrf state cookie we stored when we generated the original auth redirect.
    // We'll proactively remove it after we grab the value later.
    let Some(csrf_cookie) = cookiejar.get("csrf_state") else {
        return MissingCsrfState.into();
    };

    // Compare the state we received when creating the original
    // auth redirect TO azure with the state we just received in the request
    // FROM Azure.
    if *csrf_cookie.value() != query_state {
        return CsrfStateMismatch.into();
    }

    // Grab the pkce verifier cookie we stored when we generated the original auth redirect.
    // We'll proactively remove it after we grab the value later.
    let Some(pkce_cookie) = cookiejar.get("pkce_verifier") else {
        return MissingPkceVerifier.into();
    };

    let pkce_verifier = PkceCodeVerifier::new(pkce_cookie.value().to_owned());

    let token = match oauth2_layer
        .client
        .exchange_code(AuthorizationCode::new(query_code))
        .set_pkce_verifier(pkce_verifier)
        .request_async(&AsyncRequestHandlerWithTimeouts::new(
            &oauth2_layer.http_client,
        ))
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return BadAuthCodeTokenResponse(e.to_string()).into();
        }
    };

    let exp_secs = match token.expires_in() {
        Some(d) => d.as_secs(),
        _ => {
            return MissingExpiration.into();
        }
    };

    let secs: i64 = match exp_secs.try_into() {
        Ok(s) => s,
        _ => {
            return InvalidExpiration.into();
        }
    };

    let now_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("implausible future date")
        .as_secs();

    let user = match token.access_token().secret().split(".").nth(1) {
        None => {
            return MissingPayloadClaims.into();
        }
        Some(s) => {
            let data = match BASE64_URL_SAFE_NO_PAD.decode(s) {
                Ok(d) => d,
                Err(e) => {
                    return InvalidPayloadClaimsBase64(e).into();
                }
            };

            match serde_json::from_slice::<OauthUserData>(&data) {
                Ok(d) => d,
                Err(e) => {
                    return InvalidPayloadClaimsJson(e).into();
                }
            }
        }
    };

    // Parse will take care of any URL escaping/encoding
    let group_query_uri = match url::Url::parse(&format!(
        "{}/{}/transitiveMemberOf?$search={}",
        GRAPH_USER_GROUPS_ENDPOINT, user.oid, oauth2_layer.allowed_access_groups_filter,
    )) {
        Ok(u) => u,
        Err(e) => {
            return InvalidGroupQueryUri(e).into();
        }
    };

    // Grab the group memberships of the user with a filter to reduce the response payload.
    let request = http::Request::builder()
        .method(Method::GET)
        .uri(group_query_uri.as_str())
        .header(
            header::AUTHORIZATION,
            match HeaderValue::from_str(
                format!("Bearer {}", token.access_token().secret().to_owned()).as_str(),
            ) {
                Ok(h) => h,
                Err(e) => {
                    return CouldNotCreateAuthHeader(e.to_string()).into();
                }
            },
        )
        .header(
            "ConsistencyLevel",
            MS_GRAPH_CONSISTENCY_LEVEL_EVENTUAL.clone(),
        )
        .body(vec![]);

    let request = match request {
        Ok(r) => r,
        Err(e) => {
            return CouldNotCreateGroupDetailsRequest(e.to_string()).into();
        }
    };

    let result = AsyncRequestHandlerWithTimeouts::new(&oauth2_layer.http_client)
        .call(request)
        .await;

    let groups = match result {
        Ok(response) => match serde_json::from_slice::<OauthUserGroups>(&response.into_body()) {
            Ok(g) => g,
            Err(e) => {
                return InvalidUserGroupsResponse(e.to_string()).into();
            }
        },
        Err(e) => {
            return FailedToGetUserGroups(e.to_string()).into();
        }
    };

    // If no groups were found, then this user doesn't have
    // access.
    if groups.value.is_empty() {
        return (StatusCode::UNAUTHORIZED, "user not found in any groups").into();
    }

    // Otherwise, iterate through the groups they're in and see if any matches
    // the permitted list.
    // `groups` should be extremely small with the search filter applied and
    // id_list is likely only ever going to be one or two items, and it should
    // very likely be exactly one item after security cleans up how we use DLs,
    //
    // We're using the first group name.
    let Some(group_name) = groups
        .value
        .iter()
        .filter_map(|group| {
            oauth2_layer
                .allowed_access_groups_ids_to_name
                .get(&group.id)
        })
        .next()
    else {
        return (
            StatusCode::UNAUTHORIZED,
            "user not found in any permitted groups",
        )
            .into();
    };

    // Grab the previous page cookie so we can send the human back to the original
    // page they wanted.
    let requested_page = cookiejar
        .get("requested_page")
        .map(|v| format!("/admin{}", v.value()))
        .unwrap_or_else(|| "/admin/".to_string());

    // We're using a private cookie jar and really using the cookie similar to a simple JWT.
    // When someone tries to access carbide-web, we just need to see that they have the cookie
    // and that it's not expired and hasn't been tampered with, which we'll know when we decrypt it,
    // so we don't have a use for storing the actual token secret for later use at the moment.
    //
    // TODO: figure out what to do if no identity provider (e.g. when using admin + local dev password)
    let sid_cookie = Cookie::build(("sid", format!("{}", now_seconds + exp_secs)))
        .path("/")
        .secure(true)
        .http_only(true)
        .max_age(Duration::seconds(secs))
        .build();
    let name_cookie = Cookie::build(("name", user.name))
        .path("/")
        .secure(true)
        .http_only(true)
        .max_age(Duration::seconds(secs))
        .build();
    let group_cookie = Cookie::build(("group_name", group_name.to_string()))
        .path("/")
        .secure(true)
        .http_only(true)
        .max_age(Duration::seconds(secs))
        .build();
    // It appears NVIDIA fills this out with email in MS Entra.
    let unique_name = user.unique_name.clone();
    let unique_name_cookie = Cookie::build((
        "unique_name",
        unique_name
            .strip_suffix("@nvidia.com")
            .unwrap_or(&user.unique_name)
            .to_owned(),
    ))
    .path("/")
    .secure(true)
    .http_only(true)
    .max_age(Duration::seconds(secs))
    .build();

    (
        // Strip out any old cookies that might possibly exist,
        // add in the new sid cookie, and send it along.
        cookiejar
            .remove(pkce_cookie)
            .remove(csrf_cookie)
            .remove(sid_cookie.clone())
            .remove(name_cookie.clone())
            .remove(group_cookie.clone())
            .remove(unique_name_cookie.clone())
            .add(sid_cookie)
            .add(name_cookie)
            .add(group_cookie)
            .add(unique_name_cookie),
        Redirect::to(&requested_page),
    )
        .into_response()
        .into()
}

/// Use our own Response type so that the error message can be logged as well as placed in the
/// response body.
///
/// Note: Use the Error variant to return INTERNAL_SERVER_ERROR, don't use
/// (INTERNAL_SERVER_ERROR, "error string"), as the latter will not be logged properly.
pub(super) enum AuthCallbackResponse {
    Response(Response),
    Error(AuthCallbackError),
}

#[derive(thiserror::Error, Debug)]
pub(super) enum AuthCallbackError {
    #[error("expected oauth2 extension layer is empty")]
    EmptyOauth2Layer,
    #[error(
        "bad token response from external auth service when exchanging client credentials: {0}"
    )]
    BadClientCredentialsTokenResponse(String),
    #[error("unable to verify csrf state from external auth response")]
    MissingCsrfState,
    #[error("csrf state of auth request did not match state from external auth response")]
    CsrfStateMismatch,
    #[error("unable to extract pkce verifier from cookie")]
    MissingPkceVerifier,
    #[error(
        "bad token response from external auth service when exchanging authorization code: {0}"
    )]
    BadAuthCodeTokenResponse(String),
    #[error("failed to find expiration in auth token")]
    MissingExpiration,
    #[error("failed to convert auth expiration seconds between integer types")]
    InvalidExpiration,
    #[error("response token is missing payload claims section")]
    MissingPayloadClaims,
    #[error("invalid payload claims portion in oauth2 response token: {0}")]
    InvalidPayloadClaimsBase64(base64::DecodeError),
    #[error("invalid payload claims JSON in oauth2 response token: {0}")]
    InvalidPayloadClaimsJson(serde_json::Error),
    #[error("failed to parse group query uri: {0}")]
    InvalidGroupQueryUri(url::ParseError),
    #[error("unable to create authorization header for group details request: {0}")]
    CouldNotCreateAuthHeader(String),
    #[error("unable to create request to grab group details: {0}")]
    CouldNotCreateGroupDetailsRequest(String),
    #[error("failed to parse oauth2 user groups response: {0}")]
    InvalidUserGroupsResponse(String),
    #[error("failed to get oauth2 user groups: {0}")]
    FailedToGetUserGroups(String),
}

impl IntoResponse for AuthCallbackResponse {
    fn into_response(self) -> Response {
        match self {
            AuthCallbackResponse::Response(response) => response,
            AuthCallbackResponse::Error(error) => {
                tracing::error!(
                    error = %error,
                    "internal server error running auth_callback",
                );
                (StatusCode::INTERNAL_SERVER_ERROR, error.to_string()).into_response()
            }
        }
    }
}

/// Convert a (status, string) to a Response, logging unsuccessful responses to INFO. Not used for
/// INTERNAL_SERVER_ERROR, use AuthCallbackResponse::Error for that.
impl<S> From<(StatusCode, S)> for AuthCallbackResponse
where
    S: ToString,
{
    fn from(value: (StatusCode, S)) -> Self {
        if !value.0.is_success() {
            tracing::info!(
                http_status = value.0.as_u16(),
                response_detail = %value.1.to_string(),
                "auth_callback returned an unsuccessful response",
            );
        }
        AuthCallbackResponse::Response((value.0, value.1.to_string()).into_response())
    }
}

impl From<AuthCallbackError> for AuthCallbackResponse {
    fn from(error: AuthCallbackError) -> Self {
        AuthCallbackResponse::Error(error)
    }
}

impl From<Response> for AuthCallbackResponse {
    fn from(value: Response) -> Self {
        AuthCallbackResponse::Response(value)
    }
}

/// Used to grab the user ID from
/// the JWT of the access token we receive from
/// MS.
/// What's really being parsed in the claims portion
/// of the JWT, which holds a lot of user-data.
#[derive(Debug, Deserialize)]
struct OauthUserData {
    oid: String,
    name: String,
    unique_name: String,
}

/// A container for the list of groups return in
/// a graph response to a /transitiveMemberOf call
#[derive(Debug, Deserialize)]
struct OauthUserGroups {
    value: Vec<OauthUserGroup>,
}

/// Holds the data of an individual group
/// returned in a graph response to a
/// /transitiveMemberOf call
#[derive(Debug, Deserialize)]
struct OauthUserGroup {
    id: String,
}

/// Custom asynchronous HTTP request handler to use with oauth2 because
/// the default one supplied by oauth2 doesn't use any timeouts.
struct AsyncRequestHandlerWithTimeouts<'a> {
    client: &'a reqwest::Client,
}

impl<'a> AsyncRequestHandlerWithTimeouts<'a> {
    fn new(client: &'a reqwest::Client) -> Self {
        Self { client }
    }
}

impl<'c> oauth2::AsyncHttpClient<'c> for AsyncRequestHandlerWithTimeouts<'_> {
    type Error = reqwest::Error;
    type Future =
        Pin<Box<dyn Future<Output = Result<http::Response<Vec<u8>>, Self::Error>> + Send + 'c>>;

    fn call(&'c self, mut request: HttpRequest) -> Self::Future {
        // Inject the issuing span's W3C trace context into the outgoing OAuth2 request (#2438).
        trace_propagation::inject_current_context(request.headers_mut());
        Box::pin(async move {
            let response = self.client.execute(request.try_into()?).await?;
            let mut result = http::Response::builder().status(response.status());
            for (header_name, haeder_value) in response.headers() {
                result = result.header(header_name, haeder_value);
            }

            let status = response.status();
            let body = response.text().await?.into_bytes();

            if status.is_server_error() || status.is_client_error() {
                let body_str = std::str::from_utf8(&body).unwrap_or_default();
                tracing::error!(response_body = %body_str,"error response when making http request for oauth2 flow");
            }

            Ok(result.body(body).unwrap())
        })
    }
}
