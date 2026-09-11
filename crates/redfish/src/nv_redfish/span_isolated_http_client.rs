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

use http::HeaderMap;
use nv_redfish::bmc_http::reqwest::{BmcError, Client};
use nv_redfish::bmc_http::{BmcCredentials, HttpClient, MultipartUpdateRequest};
use nv_redfish::core::{
    BoxTryStream, ModificationResponse, ODataETag, SessionCreateResponse, UploadReader,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::Instrument;
use url::Url;

/// An nv-redfish Reqwest client whose transport work is detached from application spans.
#[derive(Clone)]
pub struct SpanIsolatedHttpClient {
    inner: Client,
}

impl SpanIsolatedHttpClient {
    pub(super) fn new(inner: Client) -> Self {
        Self { inner }
    }

    fn span() -> tracing::Span {
        // hyper-util's TokioExecutor instruments connection driver tasks with
        // the current span. A pooled connection can outlive the request, so it
        // must retain only this detached span rather than the caller's span.
        tracing::info_span!(
            parent: None,
            "nv_redfish_http",
            logfmt.suppress = true,
        )
    }
}

impl HttpClient for SpanIsolatedHttpClient {
    type Error = BmcError;

    async fn get<T>(
        &self,
        url: Url,
        credentials: &BmcCredentials,
        etag: Option<ODataETag>,
        custom_headers: &HeaderMap,
    ) -> Result<T, Self::Error>
    where
        T: DeserializeOwned + Send + Sync,
    {
        self.inner
            .get(url, credentials, etag, custom_headers)
            .instrument(Self::span())
            .await
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
        self.inner
            .post(url, body, credentials, custom_headers)
            .instrument(Self::span())
            .await
    }

    async fn post_session<B, T>(
        &self,
        url: Url,
        body: &B,
        custom_headers: &HeaderMap,
    ) -> Result<SessionCreateResponse<T>, Self::Error>
    where
        B: Serialize + Send + Sync,
        T: DeserializeOwned + Send + Sync,
    {
        self.inner
            .post_session(url, body, custom_headers)
            .instrument(Self::span())
            .await
    }

    async fn post_multipart_update<U, V, T>(
        &self,
        url: Url,
        request: MultipartUpdateRequest<'_, U, V>,
        credentials: &BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<ModificationResponse<T>, Self::Error>
    where
        U: UploadReader,
        T: DeserializeOwned + Send + Sync,
        V: Serialize + Send + Sync,
    {
        self.inner
            .post_multipart_update(url, request, credentials, custom_headers)
            .instrument(Self::span())
            .await
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
        self.inner
            .patch(url, etag, body, credentials, custom_headers)
            .instrument(Self::span())
            .await
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
        self.inner
            .delete(url, credentials, custom_headers)
            .instrument(Self::span())
            .await
    }

    async fn sse<T>(
        &self,
        url: Url,
        credentials: &BmcCredentials,
        custom_headers: &HeaderMap,
    ) -> Result<BoxTryStream<T, Self::Error>, Self::Error>
    where
        T: Sized + for<'de> serde::Deserialize<'de> + Send,
    {
        self.inner
            .sse(url, credentials, custom_headers)
            .instrument(Self::span())
            .await
    }
}
