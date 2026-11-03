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

//! The nico-rest-api client of the state readers. GET is its only request
//! method: the readers never write. Every call passes the nico-rest-api gate.

use std::sync::Arc;

use eyre::eyre;
use reqwest::StatusCode;
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderMap};
use serde::de::DeserializeOwned;
use url::Url;

use crate::framework::ObservationRecorder;
use crate::gate::{CallError, CallGate, classify_http};
use crate::rest::auth::Auth;

/// A reply bigger than this is not a page of 100 records.
const REPLY_CAP: usize = 16 << 20;

/// The header nico-rest-api reports page totals in.
const PAGINATION_HEADER: &str = "X-Pagination";

/// An answered GET: a success or a client error. Pushback and server errors
/// never get here, they are call errors.
#[derive(Debug)]
pub(crate) struct RestReply {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl RestReply {
    /// This reply, or an error when it is not a success.
    pub(crate) fn success(&self) -> Result<&Self, CallError> {
        if self.status.is_success() {
            Ok(self)
        } else {
            Err(CallError::Status(format!("HTTP {}", self.status.as_u16())))
        }
    }

    /// The JSON body of a success.
    pub(crate) fn json<T: DeserializeOwned>(&self) -> Result<T, CallError> {
        serde_json::from_slice(&self.success()?.body)
            .map_err(|e| CallError::Decode(format!("reply body: {e}")))
    }
}

/// The `total` of a paged reply's `X-Pagination` header.
pub(crate) fn pagination_total(reply: &RestReply) -> Result<u64, CallError> {
    #[derive(serde::Deserialize)]
    struct Page {
        total: i64,
    }
    let header = reply
        .success()?
        .headers
        .get(PAGINATION_HEADER)
        .ok_or_else(|| CallError::Decode(format!("reply has no {PAGINATION_HEADER} header")))?;
    let page: Page = header
        .to_str()
        .ok()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .ok_or_else(|| CallError::Decode(format!("{PAGINATION_HEADER} header is not a page")))?;
    u64::try_from(page.total)
        .map_err(|_| CallError::Decode(format!("{PAGINATION_HEADER} total is negative")))
}

/// GETs `/v2/org/{org}/nico/...` on one nico-rest-api.
pub(crate) struct RestClient {
    http: reqwest::Client,
    base: Url,
    gate: Arc<CallGate>,
}

impl RestClient {
    pub(crate) fn new(
        http: reqwest::Client,
        target: &str,
        gate: Arc<CallGate>,
    ) -> eyre::Result<Self> {
        let base = Url::parse(target).map_err(|e| eyre!("REST target: {e}"))?;
        if base.cannot_be_a_base() {
            return Err(eyre!("REST target cannot be a base URL"));
        }
        Ok(Self { http, base, gate })
    }

    /// GETs `path` under the org with `query`, retrying per the gate's rule.
    /// A 401 also drops a cached token, so the next call re-authenticates.
    pub(crate) async fn get(
        &self,
        rec: &ObservationRecorder,
        call: &'static str,
        auth: &Auth,
        org: &str,
        path: &[&str],
        query: &[(&str, String)],
    ) -> Result<RestReply, CallError> {
        let mut url = self.base.clone();
        url.path_segments_mut()
            .map_err(|()| CallError::Transport("REST target cannot be a base URL".to_string()))?
            .pop_if_empty()
            .extend(["v2", "org", org, "nico"])
            .extend(path);
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        }
        self.gate
            .call_with_retries(rec, call, || async {
                // The token comes from Keycloak or a file, not this API, but
                // a token refresh is rare enough to ride inside the call.
                let token = auth.bearer().await?;
                let response = self
                    .http
                    .get(url.clone())
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .header(ACCEPT, "application/json")
                    .send()
                    .await
                    .map_err(|e| CallError::Transport(format!("GET {call}: {e}")))?;
                let status = response.status();
                if status == StatusCode::UNAUTHORIZED {
                    auth.rejected();
                }
                if let Some(pushback @ CallError::Pushback { .. }) =
                    classify_http(status, response.headers())
                {
                    return Err(pushback);
                }
                if status.is_server_error() {
                    return Err(CallError::Status(format!("HTTP {}", status.as_u16())));
                }
                let headers = response.headers().clone();
                let body = read_capped(response, REPLY_CAP + 1)
                    .await
                    .map_err(|e| CallError::Transport(format!("read {call} body: {e}")))?;
                if body.len() > REPLY_CAP {
                    return Err(CallError::Decode(format!(
                        "{call} reply is over {REPLY_CAP} bytes"
                    )));
                }
                Ok(RestReply {
                    status,
                    headers,
                    body,
                })
            })
            .await
    }
}

/// Reads at most `cap` bytes of the response body. The latency probe GET is
/// not capped — see the drain comment in its `run`.
pub(crate) async fn read_capped(
    mut response: reqwest::Response,
    cap: usize,
) -> reqwest::Result<Vec<u8>> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        let remaining = cap - body.len();
        if chunk.len() >= remaining {
            body.extend_from_slice(&chunk[..remaining]);
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// An HTTP client with full TLS verification; an optional extra CA bundle
/// supports cluster-internal issuers. Redirects are refused outright: the
/// token POST carries the client secret in its body (which clients re-send on
/// 307/308 redirects, to ANY host) and the API GET carries a bearer token —
/// both endpoints are fixed cluster-internal services that have no legitimate
/// reason to redirect, so following one could only ever hand credentials to
/// somewhere unexpected.
pub(crate) fn new_http_client(ca_file: &str) -> eyre::Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .min_tls_version(reqwest::tls::Version::TLS_1_2);
    if !ca_file.is_empty() {
        let pem = std::fs::read(ca_file).map_err(|e| eyre!("read REST CA: {e}"))?;
        let certs = reqwest::Certificate::from_pem_bundle(&pem)
            .map_err(|e| eyre!("no certificates parsed from {ca_file}: {e}"))?;
        if certs.is_empty() {
            return Err(eyre!("no certificates parsed from {ca_file}"));
        }
        for cert in certs {
            // Added to the default (system) roots, not replacing them.
            builder = builder.add_root_certificate(cert);
        }
    }
    builder.build().map_err(|e| eyre!("build HTTP client: {e}"))
}
