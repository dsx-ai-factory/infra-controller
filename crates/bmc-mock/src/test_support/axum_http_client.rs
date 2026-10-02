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

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use http_body_util::BodyExt;
use nv_redfish::bmc_http::{
    BmcCredentials, CacheableError, HttpClient, RejectedUriReferenceError, RequestError,
};
use nv_redfish::core::upload::{MultipartUpdateRequest, UploadReader};
use nv_redfish::core::{
    BmcError, BmcErrorClass, BoxTryStream, ModificationResponse, ODataETag, SessionCreateResponse,
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
}

impl AxumRouterHttpClient {
    pub fn new(router: Router) -> Self {
        Self { router }
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
}

impl HttpClient for AxumRouterHttpClient {
    type Error = Error;
    async fn get<T>(
        &self,
        url: Url,
        credentials: &BmcCredentials,
        _: Option<ODataETag>,
        custom_headers: &HeaderMap,
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
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(Error::Json)?;
        serde_json::from_value(value).map_err(Error::Json)
    }

    async fn poll<T>(
        &self,
        _: Url,
        _: &BmcCredentials,
        _: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        T: DeserializeOwned + Send + Sync,
    {
        Err(Error::NotSupported("Task polling is not supported yet"))
    }

    async fn post<B, T>(
        &self,
        _: Url,
        _: &B,
        _: &BmcCredentials,
        _: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        B: Serialize + Send + Sync,
        T: DeserializeOwned + Send + Sync,
    {
        Err(Error::NotSupported("POST is not supported yet"))
    }

    async fn patch<B, T>(
        &self,
        _: Url,
        _: ODataETag,
        _: &B,
        _: &BmcCredentials,
        _: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        B: Serialize + Send + Sync,
        T: DeserializeOwned + Send + Sync,
    {
        Err(Error::NotSupported("PATCH is not supported yet"))
    }

    async fn delete<T>(
        &self,
        _: Url,
        _: &BmcCredentials,
        _: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        T: DeserializeOwned + Send + Sync,
    {
        Err(Error::NotSupported("DELETE is not supported yet"))
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
}

#[cfg(test)]
mod tests {
    use carbide_test_support::{Check, check_values};

    use super::*;

    #[test]
    fn bmc_error_classification_distinguishes_http_and_parse_failures() {
        let cases = [
            Check {
                scenario: "server failure must not be skipped as an unclassified account slot",
                input: Error::InvalidResponse {
                    url: Url::parse("http://bmc.example/redfish/v1/AccountService/Accounts/1")
                        .unwrap(),
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    text: String::new(),
                },
                expect: BmcErrorClass::HttpResponse { status: 503 },
            },
            Check {
                scenario: "malformed response",
                input: Error::Json(serde_json::from_str::<bool>("invalid").unwrap_err()),
                expect: BmcErrorClass::ResponseParse,
            },
        ];

        check_values(cases, |error| error.error_class());
    }
}
