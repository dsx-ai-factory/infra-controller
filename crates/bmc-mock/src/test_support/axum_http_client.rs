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

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::header::{CONTENT_TYPE, IF_MATCH, LOCATION, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode};
use http_body_util::BodyExt;
use nv_redfish::bmc_http::{
    BmcCredentials, CacheableError, HttpClient, RejectedUriReferenceError, RequestError,
};
use nv_redfish::core::odata::ODataType;
use nv_redfish::core::upload::{HttpPushUriUpdateRequest, MultipartUpdateRequest, UploadReader};
use nv_redfish::core::{
    ActionError, AsyncTask, BmcError, BmcErrorClass, BoxTryStream, ModificationResponse, ODataETag,
    ODataId, Repair, SessionCreateResponse,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tower::ServiceExt;
use url::Url;

#[derive(Debug)]
pub enum Error {
    InvalidResponse {
        url: Url,
        status: StatusCode,
        text: String,
    },
    Json(serde_json::Error),
    Http(axum::http::Error),
    Cache(String),
    RejectedUriReference(String),
    NotSupported(&'static str),
    ActionNotSupported,
}

/// Test transports classify exactly like the production reqwest transport.
impl bmc_platform::ClassifyBmcError for Error {
    fn classify(self) -> bmc_platform::PlatformError {
        match self {
            Self::InvalidResponse { status, text, .. } => {
                bmc_platform::PlatformError::from_http_response(status.as_u16(), &text)
            }
            Self::ActionNotSupported => bmc_platform::PlatformError::Unsupported,
            other => bmc_platform::PlatformError::InvalidResponse {
                message: other.to_string(),
            },
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidResponse { url, status, text } => {
                write!(f, "invalid response {} {}: {}", status, url, text)
            }
            Self::Json(err) => write!(f, "json error: {err}"),
            Self::Http(err) => write!(f, "http build error: {err}"),
            Self::Cache(reason) => write!(f, "cache error: {reason}"),
            Self::RejectedUriReference(reason) => {
                write!(f, "rejected URI reference: {reason}")
            }
            Self::NotSupported(what) => write!(f, "not supported in test client: {what}"),
            Self::ActionNotSupported => write!(f, "action is not supported by the service"),
        }
    }
}

impl std::error::Error for Error {}

impl BmcError for Error {
    fn error_class(&self) -> BmcErrorClass {
        match self {
            Self::InvalidResponse { status, .. } => BmcErrorClass::HttpResponse {
                status: status.as_u16(),
            },
            Self::Json(_) => BmcErrorClass::ResponseParse,
            _ => BmcErrorClass::Other,
        }
    }
}

impl ActionError for Error {
    fn not_supported() -> Self {
        Self::ActionNotSupported
    }
}

impl RequestError for Error {
    fn rejected_uri_reference(error: RejectedUriReferenceError) -> Self {
        Self::RejectedUriReference(error.reason)
    }
}

impl CacheableError for Error {
    fn is_cached(&self) -> bool {
        matches!(
            self,
            Self::InvalidResponse {
                status: StatusCode::NOT_MODIFIED,
                ..
            }
        )
    }

    fn cache_miss() -> Self {
        Self::NotSupported("cache miss")
    }

    fn cache_error(reason: String) -> Self {
        Self::Cache(reason)
    }
}

#[derive(Clone)]
pub struct AxumRouterHttpClient {
    router: Router,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

/// One request the client forwarded to the router.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub method: Method,
    pub uri: String,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl AxumRouterHttpClient {
    pub fn new(router: Router) -> Self {
        Self {
            router,
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns and clears every request made so far.
    pub fn take_requests(&self) -> Vec<RecordedRequest> {
        std::mem::take(
            &mut *self
                .requests
                .lock()
                .expect("request recorder mutex poisoned"),
        )
    }

    fn request_builder(
        method: Method,
        url: &Url,
        _credentials: &BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> axum::http::request::Builder {
        let mut builder = Request::builder().method(method).uri(url.to_string());
        for (name, value) in custom_headers {
            builder = builder.header(name, value);
        }
        builder
    }

    async fn call(&self, request: Request<Body>) -> Result<axum::response::Response, Error> {
        let (parts, body) = request.into_parts();
        let bytes = body
            .collect()
            .await
            .map_err(|_| Error::NotSupported("request body collect error"))?
            .to_bytes();
        self.requests
            .lock()
            .expect("request recorder mutex poisoned")
            .push(RecordedRequest {
                method: parts.method.clone(),
                uri: parts.uri.to_string(),
                headers: parts.headers.clone(),
                body: bytes.to_vec(),
            });
        let request = Request::from_parts(parts, Body::from(bytes));
        let response = self
            .router
            .clone()
            .oneshot(request)
            .await
            .map_err(|_| Error::NotSupported("router service error"))?;
        Ok(response)
    }

    async fn response_bytes(
        response: axum::response::Response,
    ) -> Result<(StatusCode, HeaderMap, axum::body::Bytes), Error> {
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response
            .into_body()
            .collect()
            .await
            .map_err(|_| Error::NotSupported("response body collect error"))?
            .to_bytes();
        Ok((status, headers, bytes))
    }

    async fn modification_response<T>(
        url: Url,
        response: axum::response::Response,
    ) -> Result<ModificationResponse<T>, Error>
    where
        T: DeserializeOwned + Send + Sync,
    {
        let (status, headers, bytes) = Self::response_bytes(response).await?;
        if !status.is_success() {
            return Err(Error::InvalidResponse {
                url,
                status,
                text: String::from_utf8_lossy(&bytes).to_string(),
            });
        }
        if status == StatusCode::ACCEPTED {
            let location = headers
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| Error::InvalidResponse {
                    url: url.clone(),
                    status,
                    text: "202 Accepted without Location header".to_string(),
                })?;
            return Ok(ModificationResponse::Task(AsyncTask {
                location: ODataId::from(location.to_string()).into(),
                retry_after: retry_after(&headers),
                task: task_from_body(&bytes),
            }));
        }
        if bytes.is_empty() {
            return Ok(ModificationResponse::Empty);
        }
        let value = serde_json::from_slice::<serde_json::Value>(&bytes).map_err(Error::Json)?;
        match serde_json::from_value(value.clone()) {
            Ok(entity) => Ok(ModificationResponse::Entity(entity)),
            Err(_) if value.as_object().is_some_and(serde_json::Map::is_empty) => {
                Ok(ModificationResponse::Empty)
            }
            Err(error) => Err(Error::Json(error)),
        }
    }
}

/// The `Task` resource a `202 Accepted` body represents, as the reqwest
/// client reads it.
fn task_from_body(body: &[u8]) -> Option<ODataId> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let odata_type = ODataType::parse_from(&value)?;
    if odata_type.namespace.first() != Some(&"Task") || odata_type.type_name != "Task" {
        return None;
    }
    value
        .get("@odata.id")
        .and_then(serde_json::Value::as_str)
        .map(|id| ODataId::from(id.to_string()))
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_secs)
}

impl HttpClient for AxumRouterHttpClient {
    type Error = Error;
    async fn get<T>(
        &self,
        url: Url,
        credentials: &BmcCredentials,
        _: Option<ODataETag>,
        custom_headers: &HeaderMap,
        repair: &impl Repair,
    ) -> Result<T, Self::Error>
    where
        T: DeserializeOwned,
    {
        let builder = Self::request_builder(Method::GET, &url, credentials, custom_headers);
        let request = builder.body(Body::empty()).map_err(Error::Http)?;
        let response = self.call(request).await?;
        let (status, _, bytes) = Self::response_bytes(response).await?;
        if !status.is_success() {
            return Err(Error::InvalidResponse {
                url,
                status,
                text: String::from_utf8_lossy(&bytes).to_string(),
            });
        }
        let mut value: serde_json::Value = serde_json::from_slice(&bytes).map_err(Error::Json)?;
        repair.repair(&mut value);
        serde_json::from_value(value).map_err(Error::Json)
    }

    /// Pending responses without `Location` keep the polled URI, and a `204`
    /// naming a `Location` is read as a reference to it.
    async fn poll<T>(
        &self,
        url: Url,
        credentials: &BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        T: DeserializeOwned + Send + Sync,
    {
        let request = Self::request_builder(Method::GET, &url, credentials, custom_headers)
            .body(Body::empty())
            .map_err(Error::Http)?;
        let response = self.call(request).await?;
        let location = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        match response.status() {
            StatusCode::ACCEPTED => {
                let (_, headers, bytes) = Self::response_bytes(response).await?;
                Ok(ModificationResponse::Task(AsyncTask {
                    location: ODataId::from(location.unwrap_or_else(|| url.path().to_string()))
                        .into(),
                    retry_after: retry_after(&headers),
                    task: task_from_body(&bytes),
                }))
            }
            StatusCode::NO_CONTENT => {
                location.map_or(Ok(ModificationResponse::Empty), |location| {
                    serde_json::from_value(serde_json::json!({"@odata.id": location}))
                        .map(ModificationResponse::Entity)
                        .map_err(Error::Json)
                })
            }
            _ => Self::modification_response(url, response).await,
        }
    }

    async fn post<B, T>(
        &self,
        url: Url,
        body: &B,
        credentials: &BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        B: Serialize + Send + Sync,
        T: DeserializeOwned + Send + Sync,
    {
        let bytes = serde_json::to_vec(body).map_err(Error::Json)?;
        let request = Self::request_builder(Method::POST, &url, credentials, custom_headers)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .body(Body::from(bytes))
            .map_err(Error::Http)?;
        Self::modification_response(url, self.call(request).await?).await
    }

    async fn patch<B, T>(
        &self,
        url: Url,
        etag: ODataETag,
        body: &B,
        credentials: &BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        B: Serialize + Send + Sync,
        T: DeserializeOwned + Send + Sync,
    {
        let bytes = serde_json::to_vec(body).map_err(Error::Json)?;
        let request = Self::request_builder(Method::PATCH, &url, credentials, custom_headers)
            .header(CONTENT_TYPE, HeaderValue::from_static("application/json"))
            .header(IF_MATCH, etag.to_string())
            .body(Body::from(bytes))
            .map_err(Error::Http)?;
        Self::modification_response(url, self.call(request).await?).await
    }

    async fn delete<T>(
        &self,
        url: Url,
        credentials: &BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        T: DeserializeOwned + Send + Sync,
    {
        let request = Self::request_builder(Method::DELETE, &url, credentials, custom_headers)
            .body(Body::empty())
            .map_err(Error::Http)?;
        Self::modification_response(url, self.call(request).await?).await
    }

    async fn sse<T: Send + Sized + for<'a> serde::Deserialize<'a>>(
        &self,
        _url: Url,
        _credentials: &BmcCredentials,
        _custom_headers: &HeaderMap,
    ) -> Result<BoxTryStream<T, Self::Error>, Self::Error> {
        Err(Error::NotSupported("SSE stream is not supported yet"))
    }

    async fn post_session<B, T>(
        &self,
        _: Url,
        _: &B,
        _: &HeaderMap,
    ) -> Result<SessionCreateResponse<T>, Self::Error>
    where
        B: Serialize + Send + Sync,
        T: DeserializeOwned + Send + Sync,
    {
        Err(Error::NotSupported("POST for Session is not supported yet"))
    }

    async fn post_multipart_update<U, V, T>(
        &self,
        _: Url,
        _: MultipartUpdateRequest<'_, U, V>,
        _: &BmcCredentials,
        _: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        U: UploadReader,
        T: DeserializeOwned + Send + Sync,
        V: Serialize + Send + Sync,
    {
        Err(Error::NotSupported("Multipart update is not supported yet"))
    }

    async fn post_http_push_uri_update<U, T>(
        &self,
        _: Url,
        _: HttpPushUriUpdateRequest<U>,
        _: &BmcCredentials,
        _: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        U: UploadReader,
        T: DeserializeOwned + Send + Sync,
    {
        Err(Error::NotSupported(
            "HttpPushUri update is not supported yet",
        ))
    }
}

#[cfg(test)]
mod tests {
    use axum::routing::{patch, post};
    use serde_json::json;

    use super::*;

    #[test]
    fn bmc_error_classification_distinguishes_http_and_parse_failures() {
        let cases = [
            (
                "server failure must not be skipped as an unclassified account slot",
                Error::InvalidResponse {
                    url: Url::parse("http://bmc.example/redfish/v1/AccountService/Accounts/1")
                        .unwrap(),
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    text: String::new(),
                },
                BmcErrorClass::HttpResponse { status: 503 },
            ),
            (
                "malformed response",
                Error::Json(serde_json::from_str::<bool>("invalid").unwrap_err()),
                BmcErrorClass::ResponseParse,
            ),
        ];

        for (scenario, error, expected) in cases {
            let actual = error.error_class();
            assert_eq!(actual, expected, "{scenario}");
        }
    }

    fn credentials() -> BmcCredentials {
        BmcCredentials::new("root".to_string(), "password".to_string())
    }

    #[tokio::test]
    async fn mutations_are_forwarded_and_recorded() {
        let router = Router::new()
            .route(
                "/redfish/v1/Systems/1",
                patch(|| async { StatusCode::NO_CONTENT }),
            )
            .route(
                "/redfish/v1/UpdateService/Actions/Update",
                post(|| async {
                    (
                        StatusCode::ACCEPTED,
                        [
                            (LOCATION, "/redfish/v1/TaskService/Tasks/42"),
                            (RETRY_AFTER, "7"),
                        ],
                    )
                }),
            );
        let client = AxumRouterHttpClient::new(router);

        let patched: ModificationResponse<serde_json::Value> = client
            .patch(
                Url::parse("https://bmc.test/redfish/v1/Systems/1").expect("valid URL"),
                ODataETag::from("*".to_string()),
                &json!({"Boot": {"BootSourceOverrideTarget": "Pxe"}}),
                &credentials(),
                &HeaderMap::new(),
            )
            .await
            .expect("PATCH succeeds");
        assert!(matches!(patched, ModificationResponse::Empty));

        let posted: ModificationResponse<serde_json::Value> = client
            .post(
                Url::parse("https://bmc.test/redfish/v1/UpdateService/Actions/Update")
                    .expect("valid URL"),
                &json!({"ImageURI": "https://example.test/image.bin"}),
                &credentials(),
                &HeaderMap::new(),
            )
            .await
            .expect("POST succeeds");
        let ModificationResponse::Task(task) = posted else {
            panic!("expected asynchronous task")
        };
        assert_eq!(
            task.location.0.to_string(),
            "/redfish/v1/TaskService/Tasks/42"
        );
        assert_eq!(task.retry_after, Some(Duration::from_secs(7)));

        let requests = client.take_requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, Method::PATCH);
        assert_eq!(
            requests[0]
                .headers
                .get(IF_MATCH)
                .and_then(|v| v.to_str().ok()),
            Some("*")
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&requests[1].body).expect("JSON body"),
            json!({"ImageURI": "https://example.test/image.bin"})
        );
        assert!(client.take_requests().is_empty());
    }
}
