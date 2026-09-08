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

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use carbide_instrument::red;
use reqwest::header::{ACCEPT, HeaderMap, HeaderValue};
use reqwest::{Client, ClientBuilder, Method, Response, Url};
pub use serde_json::Value as JsonValue;

use crate::config::{NvueConfig, NvueConfigWithHeader, NvueRevision};
use crate::types::bgp::{BgpNeighbors, BgpVrfInfo};
use crate::types::revision::{
    RevisionApplyStatus, RevisionConfigDiff, RevisionData, RevisionIssueSummary,
};

/// Repeated NVUE field-selection query parameters.
///
/// Filter values are JSON Pointer-style paths that start with `/` and may
/// use Unix shell-style wildcards to match dynamic object keys. For example,
/// `/neighbor/*/state` matches the `state` field for every BGP neighbor. Values
/// are passed to NVUE without local validation.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldFilter {
    include: Vec<String>,
    omit: Vec<String>,
}

impl FieldFilter {
    /// Create an empty filter.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a filter from `include` field patterns.
    pub fn with_includes<I, S>(fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            include: fields.into_iter().map(Into::into).collect(),
            omit: Vec::new(),
        }
    }

    /// Create a filter from `omit` field patterns.
    pub fn with_omits<I, S>(fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            include: Vec::new(),
            omit: fields.into_iter().map(Into::into).collect(),
        }
    }

    /// Add an `include` field pattern.
    pub fn include(mut self, field: impl Into<String>) -> Self {
        self.include.push(field.into());
        self
    }

    /// Add an `omit` field pattern.
    pub fn omit(mut self, field: impl Into<String>) -> Self {
        self.omit.push(field.into());
        self
    }

    fn is_empty(&self) -> bool {
        self.include.is_empty() && self.omit.is_empty()
    }

    fn query_pairs(&self) -> Vec<(&str, &str)> {
        self.include
            .iter()
            .map(|field| ("include", field.as_str()))
            .chain(self.omit.iter().map(|field| ("omit", field.as_str())))
            .collect()
    }
}

#[derive(Debug)]
pub struct NvueClient {
    server_address: NvueServerAddress,
    client: Client,
}

impl NvueClient {
    // In the past, we've seen calls to `nv config apply` take a long time, to
    // the point where the timeout for that code path (outside this crate) was
    // raised to 45s. We don't know for sure that we need the same budget here,
    // but let's assume we do. -drew
    const APPLY_CONFIG_REVISION_TIMEOUT: Duration = Duration::from_secs(45);
    const APPLY_CONFIG_REVISION_POLL_INTERVAL: Duration = Duration::from_secs(1);

    pub fn new(server_address: NvueServerAddress) -> Result<Self, NvueClientError> {
        build_client(&server_address).map(|client| Self {
            server_address,
            client,
        })
    }

    pub fn new_https_from_env() -> Result<Self, NvueClientError> {
        NvueServerAddress::https_from_env().and_then(Self::new)
    }

    // Construct a URL string using the internal server address and the
    // specified path.
    fn construct_url_string(&self, path: &str) -> String {
        let (scheme, address) = match &self.server_address {
            NvueServerAddress::UnixSocket { .. } => ("http", "localhost"),
            NvueServerAddress::TcpTls { address, .. } => ("https", address.as_str()),
        };
        format!("{scheme}://{address}{path}")
    }

    // Get the auth creds, if applicable.
    fn auth_creds(&self) -> Option<&NvueAuth> {
        match &self.server_address {
            NvueServerAddress::UnixSocket { .. } => None,
            NvueServerAddress::TcpTls { auth, .. } => auth.as_ref(),
        }
    }

    // Helper for constructing a request.
    fn request(
        &self,
        method: Method,
        path: &str,
    ) -> Result<reqwest::RequestBuilder, NvueClientError> {
        let url = self.construct_url_string(path);
        let builder = self.client.request(method, url);
        // TODO: Make this timeout configurable.
        let builder = builder.timeout(std::time::Duration::from_secs(60));
        let builder = match self.auth_creds() {
            Some(creds) => builder.basic_auth(&creds.username, Some(&creds.password)),
            None => builder,
        };
        Ok(builder)
    }

    async fn execute(
        &self,
        operation: &'static str,
        request: reqwest::Request,
    ) -> Result<Response, NvueClientError> {
        let method = request.method().clone();
        let url = request.url().clone();
        let body = request
            .body()
            .and_then(|b| b.as_bytes())
            .map(|b| String::from_utf8_lossy(b).into_owned());
        red::instrumented("nvue", operation, async move {
            self.client
                .execute(request)
                .await
                .and_then(|response| response.error_for_status())
        })
        .await
        .map_err(|source| {
            NvueClientError::RequestFailed(Box::new(RequestFailed {
                method,
                url,
                body,
                source,
            }))
        })
    }

    pub async fn get_api(&self) -> Result<Response, NvueClientError> {
        const PATH: &str = "/nvue_v1/system/api?rev=applied";
        let request = self.request(Method::GET, PATH)?.build()?;
        self.execute("get_api", request).await
    }

    /// Return the config that is tagged as "applied" (in other words, the one
    /// that is currently running on the system).
    pub async fn get_applied_config(&self) -> Result<NvueConfigWithHeader, NvueClientError> {
        const PATH: &str = "/nvue_v1/?rev=applied&filled=false";
        let request = self.request(Method::GET, PATH)?.build()?;
        let response = self.execute("get_applied_config", request).await?;
        let nvue_config = response.json().await?;
        Ok(nvue_config)
    }

    /// Get the config diff between two NVUE revisions. The order of the
    /// arguments is significant; NVUE will return the set of changes necessary
    /// to turn `base_revision` into `target_revision`.
    pub async fn get_revision_config_diff(
        &self,
        base_revision: &str,
        target_revision: &str,
    ) -> Result<RevisionConfigDiff, NvueClientError> {
        let mut request = self.request(Method::GET, "/nvue_v1/")?.build()?;
        request
            .url_mut()
            .query_pairs_mut()
            .append_pair("diff", base_revision)
            .append_pair("rev", target_revision)
            .append_pair("filled", "false");

        let response = self.execute("get_revision_config_diff", request).await?;
        let diff = response.json().await?;
        Ok(diff)
    }

    /// Return BGP data for a VRF.
    ///
    /// This calls `GET /nvue_v1/vrf/{vrf-id}/router/bgp` without field filters.
    /// The `vrf_id` path segment is URL-encoded before the request is built.
    pub async fn get_bgp_vrf_info(&self, vrf_id: &str) -> Result<BgpVrfInfo, NvueClientError> {
        self.get_bgp_vrf_info_filtered(vrf_id, FieldFilter::new())
            .await
    }

    /// Return BGP data for a VRF, applying NVUE field-selection filters.
    ///
    /// Non-empty filters are encoded as repeated `include` and `omit` query
    /// parameters. Field patterns are passed through without local validation.
    pub async fn get_bgp_vrf_info_filtered(
        &self,
        vrf_id: &str,
        filter: FieldFilter,
    ) -> Result<BgpVrfInfo, NvueClientError> {
        let path = format!(
            "/nvue_v1/vrf/{encoded_vrf_id}/router/bgp",
            encoded_vrf_id = urlencoding::encode(vrf_id),
        );
        let mut request = self.request(Method::GET, &path)?.build()?;

        append_filter_query_pairs(&mut request, &filter);

        let response = self.execute("get_bgp_vrf_info", request).await?;
        let bgp_vrf_info = response.json().await?;
        Ok(bgp_vrf_info)
    }

    /// Return BGP neighbor data for a VRF.
    ///
    /// This calls `GET /nvue_v1/vrf/{vrf-id}/router/bgp/neighbor` without field filters.
    /// The `vrf_id` path segment is URL-encoded before the request is built.
    pub async fn get_bgp_neighbors(
        &self,
        vrf_id: &str,
    ) -> Result<Option<BgpNeighbors>, NvueClientError> {
        self.get_bgp_neighbors_filtered(vrf_id, FieldFilter::new())
            .await
    }

    /// Return BGP neighbor data for a VRF, applying NVUE field-selection filters.
    ///
    /// This calls `GET /nvue_v1/vrf/{vrf-id}/router/bgp/neighbor`. Non-empty
    /// filters are encoded as repeated `include` and `omit` query parameters.
    /// The `vrf_id` path segment is URL-encoded before the request is built.
    pub async fn get_bgp_neighbors_filtered(
        &self,
        vrf_id: &str,
        filter: FieldFilter,
    ) -> Result<Option<BgpNeighbors>, NvueClientError> {
        let path = format!(
            "/nvue_v1/vrf/{encoded_vrf_id}/router/bgp/neighbor",
            encoded_vrf_id = urlencoding::encode(vrf_id),
        );
        let mut request = self.request(Method::GET, &path)?.build()?;

        append_filter_query_pairs(&mut request, &filter);

        let response = self.execute("get_bgp_neighbors_filtered", request).await?;
        let bgp_neighbors = response.json().await?;
        Ok(bgp_neighbors)
    }

    /// Create a new NVUE config revision, returning the revision ID.
    pub async fn create_config_revision(&self) -> Result<String, NvueClientError> {
        const PATH: &str = "/nvue_v1/revision";
        let request = self.request(Method::POST, PATH)?.build()?;
        let response = self.execute("create_config_revision", request).await?;
        let revision: NvueRevision = response.json().await?;
        let revision_id = revision
            .get_revision_id()
            .ok_or(NvueClientError::SchemaMismatch("Missing revision id"))?;
        Ok(revision_id)
    }

    /// Return data about the specified revision.
    pub async fn get_revision(&self, revision_id: &str) -> Result<RevisionData, NvueClientError> {
        let revision_path = format!("/nvue_v1/revision/{revision_id}");
        let request = self.request(Method::GET, &revision_path)?.build()?;
        let response = self.execute("get_revision", request).await?;

        // For some reason, the NVUE schema allows the response to be nulled,
        // but as far as we're concerned that's an error.
        let revision_data: Option<_> = response.json().await?;
        revision_data.ok_or(NvueClientError::SchemaMismatch(
            "revision response was null",
        ))
    }

    /// Replace the specified config revision. Under the hood, this is a
    /// two-stage operation where the configuration is deleted and then new
    /// values are inserted.
    pub async fn replace_config_revision(
        &self,
        revision_id: &str,
        config: &NvueConfig,
    ) -> Result<(), NvueClientError> {
        let revision_path = format!("/nvue_v1/?rev={revision_id}");

        let builder = self.request(Method::DELETE, &revision_path)?;
        let empty_config: HashMap<String, String> = HashMap::new();
        let builder = builder.json(&empty_config);
        let request = builder.build()?;
        let _response = self.execute("replace_config.delete", request).await?;

        let builder = self.request(Method::PATCH, &revision_path)?;
        let builder = builder.json(&config);
        let request = builder.build()?;
        let _response = self.execute("replace_config.patch", request).await?;
        Ok(())
    }

    pub async fn apply_config_revision(&self, revision_id: &str) -> Result<(), NvueClientError> {
        let revision_path = format!("/nvue_v1/revision/{revision_id}");
        let builder = self.request(Method::PATCH, &revision_path)?;
        let body = NvueApplyData::force_apply();
        let builder = builder.json(&body);
        let request = builder.build()?;
        let _response = self.execute("apply_config_revision", request).await?;

        let started = tokio::time::Instant::now();
        let deadline = started + Self::APPLY_CONFIG_REVISION_TIMEOUT;

        loop {
            let revision = self.get_revision(revision_id).await?;

            let now = tokio::time::Instant::now();
            let remaining = deadline.checked_duration_since(now);

            match (revision.apply_status(), remaining) {
                (RevisionApplyStatus::Applied, _) => break Ok(()),
                (RevisionApplyStatus::Failed(error_issues), _) => {
                    break Err(NvueClientError::RevisionApplyFailed {
                        revision_id: revision_id.to_owned(),
                        reason: RevisionApplyFailureReason::Error,
                        last_state: revision.state.clone(),
                        progress: revision.transition_progress().map(String::from),
                        error_issues,
                    });
                }
                (RevisionApplyStatus::Pending, Some(remaining)) => {
                    tokio::time::sleep(remaining.min(Self::APPLY_CONFIG_REVISION_POLL_INTERVAL))
                        .await;
                }
                (RevisionApplyStatus::Pending, None) => {
                    let elapsed = now - started;
                    break Err(NvueClientError::RevisionApplyFailed {
                        revision_id: revision_id.to_owned(),
                        reason: RevisionApplyFailureReason::Timeout { waited: elapsed },
                        last_state: revision.state.clone(),
                        progress: revision.transition_progress().map(String::from),
                        error_issues: Vec::new(),
                    });
                }
            }
        }
    }

    /// Perform all of the NVUE operations required to apply a new config. Under
    /// the hood, we create a new revision, patch it with this config, diff it
    /// against the "applied" revision, and then apply it if it differed.
    ///
    /// Returns a `Some(revision_id)` if we applied the new revision, and `None`
    /// if NVUE indicated no change compared to the "applied" revision.
    pub async fn push_config(
        &self,
        config: &NvueConfig,
    ) -> Result<Option<String>, NvueClientError> {
        let revision_id = self.create_config_revision().await?;
        self.replace_config_revision(&revision_id, config).await?;

        // NVUE will not actually apply a revision if it's unchanged from what's
        // currently applied, we need to check this before we try.
        let diff = self
            .get_revision_config_diff("applied", &revision_id)
            .await?;
        if diff.is_empty() {
            // Note that we're leaving the old revision sitting around unused!
            // NVUE provides a `DELETE` operation on a revision ID, but HBN
            // patches this out in its OpenAPI spec For Some Reason(tm) so it's
            // unclear if it's safe to try it.
            return Ok(None);
        }

        self.apply_config_revision(&revision_id).await?;
        Ok(Some(revision_id))
    }

    // Retrieve the system information from the NVUE server. The fields returned
    // seem to vary depending on which platform NVUE is running on, so we just
    // return a JSON value.
    pub async fn system_info(&self) -> Result<JsonValue, NvueClientError> {
        let path = "/nvue_v1/system";
        let builder = self.request(Method::GET, path)?;
        let request = builder.build()?;
        let response = self.execute("system_info", request).await?;
        let resonse_body = response.json().await?;
        Ok(resonse_body)
    }

    /// Using the system_info() method, try to extract the value of the "build"
    /// key from the system info.
    pub async fn system_build_info(&self) -> Result<String, NvueClientError> {
        let system = self.system_info().await?;
        let system_object = match system {
            JsonValue::Object(map) => Ok(map),
            _ => {
                let msg = "System info is not a JSON object";
                Err(NvueClientError::SchemaMismatch(msg))
            }
        }?;
        let build = system_object.get("build").ok_or({
            let msg = "System info object has no \"build\" key";
            NvueClientError::SchemaMismatch(msg)
        })?;
        let build_value = match build {
            JsonValue::String(value) => Ok(value),
            _ => {
                let msg = "System info \"build\" value was not a string";
                Err(NvueClientError::SchemaMismatch(msg))
            }
        }?;
        Ok(build_value.into())
    }

    /// Get the MAC table for a bridge.
    pub async fn bridge_mac_table(
        &self,
        bridge_domain: &str,
    ) -> Result<Vec<crate::types::MacTableEntry>, NvueClientError> {
        let path = format!("/nvue_v1/bridge/domain/{bridge_domain}/mac-table");
        let builder = self.request(Method::GET, &path)?;
        let request = builder.build()?;
        let response = self.execute("bridge_mac_table", request).await?;
        let resonse_body: BTreeMap<String, _> = response.json().await?;
        let response = resonse_body.into_values().collect();
        Ok(response)
    }
}

fn append_filter_query_pairs(request: &mut reqwest::Request, filter: &FieldFilter) {
    if filter.is_empty() {
        return;
    }

    let mut query_pairs = request.url_mut().query_pairs_mut();
    for (key, value) in filter.query_pairs() {
        query_pairs.append_pair(key, value);
    }
}

fn build_client(server_address: &NvueServerAddress) -> Result<Client, NvueClientError> {
    let builder = ClientBuilder::new().default_headers(default_nvue_headers());
    let builder = match server_address {
        NvueServerAddress::UnixSocket { socket_path } => builder.unix_socket(socket_path.as_path()),
        NvueServerAddress::TcpTls { .. } => {
            // NVUE uses a self-signed cert out of the box.
            builder.danger_accept_invalid_certs(true)
        }
    };
    builder.build().map_err(NvueClientError::from)
}

#[derive(Debug, serde::Serialize)]
struct NvueApplyData {
    state: String,
    #[serde(rename = "auto-prompt")]
    auto_prompt: NvueAutoPrompt,
}

impl NvueApplyData {
    fn force_apply() -> Self {
        let state = "apply".into();
        let auto_prompt = NvueAutoPrompt::ays_yes();
        Self { state, auto_prompt }
    }
}

#[derive(Debug, serde::Serialize)]
// This controls what NVUE does with configurations where the validator produced
// warnings or errors.
struct NvueAutoPrompt {
    ays: String,
}

impl NvueAutoPrompt {
    fn ays_yes() -> Self {
        let ays = "ays_yes".into();
        Self { ays }
    }
}

fn default_nvue_headers() -> HeaderMap {
    HeaderMap::from_iter([(ACCEPT, HeaderValue::from_static("application/json"))])
}

#[derive(Debug)]
pub enum NvueServerAddress {
    UnixSocket {
        socket_path: PathBuf,
    },
    TcpTls {
        address: String,
        auth: Option<NvueAuth>,
    },
}

impl NvueServerAddress {
    /// Construct the server address using an undocumented internal Unix socket,
    /// which sidesteps authentication entirely but may not be available unless
    /// you're on the same host as the server.
    pub fn default_unix_socket() -> Self {
        let socket_path = "/run/nvue/nvue.sock".into();
        Self::UnixSocket { socket_path }
    }

    /// Construct the server address using values from the environment.
    /// `NVUE_HTTPS_ADDRESS` should just be the address part of the URL, so
    /// something like `localhost:8765`. `NVUE_USERNAME` and `NVUE_PASSWORD`
    /// should contain the username and password used during HTTP basic auth to
    /// the API.
    pub fn https_from_env() -> Result<Self, NvueClientError> {
        let address = get_nvue_envvar("NVUE_HTTPS_ADDRESS")?;
        let username = get_nvue_envvar("NVUE_USERNAME")?;
        let password = get_nvue_envvar("NVUE_PASSWORD")?;
        let auth = Some(NvueAuth { username, password });
        Ok(Self::TcpTls { address, auth })
    }
}

fn get_nvue_envvar(var: &'static str) -> Result<String, NvueClientError> {
    std::env::var(var).map_err(|e| NvueClientError::EnvVarError(var, e))
}

pub struct NvueAuth {
    username: String,
    password: String,
}

impl std::fmt::Debug for NvueAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NvueAuth")
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

#[derive(thiserror::Error, Debug)]
pub enum NvueClientError {
    #[error("reqwest client error: {0}")]
    ReqwestError(#[from] reqwest::Error),

    #[error(transparent)]
    RequestFailed(Box<RequestFailed>),

    #[error("environment variable error ({0}): {1}")]
    EnvVarError(&'static str, std::env::VarError),

    #[error("schema mismatch between NVUE client and server: {0}")]
    SchemaMismatch(&'static str),

    #[error(
        "NVUE revision apply failed: revision_id={revision_id}, reason={reason}, last_state={last_state:?}, progress={progress:?}, error_issues={error_issues:?}"
    )]
    RevisionApplyFailed {
        revision_id: String,
        reason: RevisionApplyFailureReason,
        last_state: Option<String>,
        progress: Option<String>,
        error_issues: Vec<RevisionIssueSummary>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionApplyFailureReason {
    Error,
    Timeout { waited: Duration },
}

impl std::fmt::Display for RevisionApplyFailureReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Error => f.write_str("error"),
            Self::Timeout { waited } => write!(f, "timeout after {waited:?}"),
        }
    }
}

#[derive(thiserror::Error, Debug)]
#[error("NVUE request failed ({method} {url}{}): {source}",
    body.as_deref().map(|b| format!(" body={b}")).unwrap_or_default())]
pub struct RequestFailed {
    pub method: Method,
    pub url: Url,
    pub body: Option<String>,
    #[source]
    pub source: reqwest::Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_filter_empty_has_no_query_pairs() {
        let filter = FieldFilter::new();

        assert!(filter.is_empty());
        assert!(filter.query_pairs().is_empty());
    }

    #[test]
    fn field_filter_preserves_include_pairs() {
        let filter = FieldFilter::new()
            .include("/neighbor/*/state")
            .include("/neighbor/*/peer-group");

        assert_eq!(
            filter.query_pairs(),
            vec![
                ("include", "/neighbor/*/state"),
                ("include", "/neighbor/*/peer-group"),
            ]
        );
    }

    #[test]
    fn field_filter_with_includes_builds_include_pairs() {
        let filter = FieldFilter::with_includes(["/neighbor/*/state", "/neighbor/*/peer-group"]);

        assert_eq!(
            filter.query_pairs(),
            vec![
                ("include", "/neighbor/*/state"),
                ("include", "/neighbor/*/peer-group"),
            ]
        );
    }

    #[test]
    fn field_filter_preserves_omit_pairs() {
        let filter = FieldFilter::new()
            .omit("/peer-group")
            .omit("/address-family");

        assert_eq!(
            filter.query_pairs(),
            vec![("omit", "/peer-group"), ("omit", "/address-family")]
        );
    }

    #[test]
    fn field_filter_with_omits_builds_omit_pairs() {
        let filter = FieldFilter::with_omits(["/peer-group", "/address-family"]);

        assert_eq!(
            filter.query_pairs(),
            vec![("omit", "/peer-group"), ("omit", "/address-family")]
        );
    }

    #[test]
    fn field_filter_combines_include_and_omit_pairs() {
        let filter = FieldFilter::new()
            .include("/neighbor/*/state")
            .omit("/peer-group")
            .include("/configured-neighbors")
            .omit("/address-family");

        assert_eq!(
            filter.query_pairs(),
            vec![
                ("include", "/neighbor/*/state"),
                ("include", "/configured-neighbors"),
                ("omit", "/peer-group"),
                ("omit", "/address-family"),
            ]
        );
    }
}
