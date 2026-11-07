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
mod nmxm_api;
pub mod nmxm_model;

use std::collections::HashMap;
use std::string::String;
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderValue, USER_AGENT};
use reqwest::{ClientBuilder, Method, StatusCode};
use reqwest_middleware::ClientWithMiddleware as HttpClient;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tracing::debug;

use crate::nmxm_api::NmxmApi;
use crate::nmxm_model::*;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(thiserror::Error, Debug)]
pub enum NmxmApiError {
    #[error("network error talking to NMX-M server at {url}. {source}")]
    NetworkError {
        url: String,
        source: reqwest_middleware::Error,
    },

    #[error("HTTP {status_code} at {url}: {response_body}")]
    HTTPErrorCode {
        url: String,
        status_code: StatusCode,
        response_body: String,
    },

    #[error("API error {status}: {message} at {url}")]
    APIError {
        url: String,
        status: StatusCode,
        message: String,
    },

    #[error("API error {status}: no response at {url}")]
    APINoResponseError { url: String, status: StatusCode },

    #[error("could not deserialize response from {url}. body: {body}. {source}")]
    JsonDeserializeError {
        url: String,
        body: String,
        source: serde_json::Error,
    },

    #[error("could not serialize request body for {url}. obj: {object_debug}. {source}")]
    JsonSerializeError {
        url: String,
        object_debug: String,
        source: serde_json::Error,
    },

    #[error("remote returned empty body at {url}, {source}")]
    NoContent { url: String, source: reqwest::Error },

    #[error("HTTP client not initialized")]
    Uninitialized,

    #[error("login failure")]
    LoginFailure,

    #[error("logout failure")]
    LogoutFailure,

    #[error("reqwest error: '{0}'")]
    ReqwestError(#[from] reqwest::Error),

    #[error("invalid arguments")]
    InvalidArguments,
}

#[derive(Clone, PartialEq, Eq)] // WARN: Do not derive Debug: Endpoint may contain credentials and must not be logged accidentally.
pub struct Endpoint {
    pub host: String,
    pub username: Option<String>,
    pub password: Option<String>,
}

impl Default for Endpoint {
    fn default() -> Self {
        Endpoint {
            host: "".to_string(),
            username: None,
            password: None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct NmxmClientPoolBuilder {
    pub timeout: Duration,
    pub accept_invalid_certs: bool,
}

impl NmxmClientPoolBuilder {
    pub fn build(&self) -> Result<NmxmClientPool, NmxmApiError> {
        let builder = ClientBuilder::new();

        let client = builder
            .cookie_store(true)
            .danger_accept_invalid_certs(self.accept_invalid_certs)
            .timeout(self.timeout)
            .build()?;

        // The `reqwest-tracing` middleware injects the current span's W3C trace context into every
        // outgoing request (#2438).
        let client = reqwest_middleware::ClientBuilder::new(client)
            .with(reqwest_tracing::TracingMiddleware::default())
            .build();

        let pool = NmxmClientPool { client };

        Ok(pool)
    }
}

#[derive(Debug, Clone)]
pub struct NmxmClientPool {
    client: HttpClient,
}

impl NmxmClientPool {
    pub fn builder(allow_insecure: bool) -> NmxmClientPoolBuilder {
        NmxmClientPoolBuilder {
            timeout: DEFAULT_TIMEOUT,
            //nmx-m probably has self-signed certs, need this set to true
            accept_invalid_certs: allow_insecure,
        }
    }

    pub async fn create_client(&self, endpoint: Endpoint) -> Result<Box<dyn Nmxm>, NmxmApiError> {
        let api = NmxmApiClient::new(self.client.clone(), endpoint.clone());
        let nmxm = NmxmApi::new(&api);
        nmxm.create(endpoint).await
    }
}

#[derive(Clone)]
pub struct NmxmApiClient {
    endpoint: Endpoint,
    client: HttpClient,
}

impl NmxmApiClient {
    pub fn new(client: HttpClient, endpoint: Endpoint) -> Self {
        Self { client, endpoint }
    }

    pub async fn get_raw(&self, api: &str) -> Result<RawResponse, NmxmApiError> {
        let url = format!("{}/{}", self.endpoint.host, api);

        let http_client = self.client.clone();
        let mut req_b = http_client.get(&url);
        req_b = req_b.header(ACCEPT, HeaderValue::from_static("*/*"));
        req_b = req_b.header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        req_b = req_b.header(USER_AGENT, HeaderValue::from_static("libnmxm/0.1"));
        if let Some(username) = self.endpoint.username.as_ref() {
            req_b = req_b.basic_auth(username, self.endpoint.password.as_ref());
        }

        let response = req_b.send().await.map_err(|e| NmxmApiError::NetworkError {
            url: url.clone(),
            source: e,
        })?;
        let status_code = response.status();
        let headers = response.headers().clone();
        let text = response.text().await.map_err(|_| NmxmApiError::APIError {
            url,
            status: status_code,
            message: "error reading response".to_string(),
        })?;

        Ok(RawResponse {
            body: text,
            code: status_code.as_u16(),
            headers,
        })
    }

    pub async fn get<T>(&self, api: &str) -> Result<(StatusCode, T), NmxmApiError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
    {
        let (status_code, resp_opt) = self.req::<T, String>(Method::GET, api, None, None).await?;
        match resp_opt {
            Some(response_body) => Ok((status_code, response_body)),
            None => Err(NmxmApiError::APINoResponseError {
                url: api.to_string(),
                status: status_code,
            }),
        }
    }

    pub async fn post<T, B>(&self, api: &str, data: B) -> Result<(StatusCode, T), NmxmApiError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
        B: Serialize + ::std::fmt::Debug,
    {
        let (status_code, resp_opt) = self
            .req::<T, B>(Method::POST, api, Some(data), None)
            .await?;
        match resp_opt {
            Some(response_body) => Ok((status_code, response_body)),
            None => Err(NmxmApiError::APINoResponseError {
                url: api.to_string(),
                status: status_code,
            }),
        }
    }

    pub async fn put<T, B>(&self, api: &str, data: B) -> Result<(StatusCode, T), NmxmApiError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
        B: Serialize + ::std::fmt::Debug,
    {
        let _empty_hash: HashMap<String, serde_json::Value> = HashMap::new();
        let (status_code, resp_opt) = self.req::<T, B>(Method::PUT, api, Some(data), None).await?;
        match resp_opt {
            Some(response_body) => Ok((status_code, response_body)),
            None => Err(NmxmApiError::APINoResponseError {
                url: api.to_string(),
                status: status_code,
            }),
        }
    }

    pub async fn delete<T>(&self, api: &str) -> Result<(StatusCode, T), NmxmApiError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
    {
        let (status_code, resp_opt) = self
            .req::<T, String>(Method::DELETE, api, None, None)
            .await?;
        match resp_opt {
            Some(response_body) => Ok((status_code, response_body)),
            None => Err(NmxmApiError::APINoResponseError {
                url: api.to_string(),
                status: status_code,
            }),
        }
    }

    async fn req<T, B>(
        &self,
        method: Method,
        api: &str,
        body: Option<B>,
        override_timeout: Option<Duration>,
    ) -> Result<(StatusCode, Option<T>), NmxmApiError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
        B: Serialize + ::std::fmt::Debug,
    {
        match self._req(&method, api, &body, override_timeout).await {
            Ok(x) => Ok(x),
            Err(NmxmApiError::NetworkError { .. }) => {
                debug!("Network error, retrying");
                self._req(&method, api, &body, override_timeout).await
            }
            Err(e) => Err(e),
        }
    }

    async fn _req<T, B>(
        &self,
        method: &Method,
        api: &str,
        body: &Option<B>,
        override_timeout: Option<Duration>,
    ) -> Result<(StatusCode, Option<T>), NmxmApiError>
    where
        T: DeserializeOwned + ::std::fmt::Debug,
        B: Serialize + ::std::fmt::Debug,
    {
        let url = format!("{}/{}", self.endpoint.host, api);

        let body_enc = match body {
            Some(b) => {
                let url: String = url.clone();
                let body_enc =
                    serde_json::to_string(&b).map_err(|e| NmxmApiError::JsonSerializeError {
                        url,
                        object_debug: format!("{b:?}"),
                        source: e,
                    })?;
                Some(body_enc)
            }
            None => None,
        };

        let http_client = self.client.clone();
        let mut req_b = match *method {
            Method::GET => http_client.get(&url),
            Method::POST => http_client.post(&url),
            Method::PATCH => http_client.patch(&url),
            Method::DELETE => http_client.delete(&url),
            Method::PUT => http_client.put(&url),
            _ => unreachable!("Only GET, POST, PATCH, DELETE and PUT http methods are used."),
        };
        req_b = req_b.header(ACCEPT, HeaderValue::from_static("*/*"));
        req_b = req_b.header(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        req_b = req_b.header(USER_AGENT, HeaderValue::from_static("libnmxm/0.1"));
        if let Some(username) = self.endpoint.username.as_ref() {
            req_b = req_b.basic_auth(username, self.endpoint.password.as_ref());
        }

        if let Some(t) = override_timeout {
            req_b = req_b.timeout(t);
        }
        if let Some(b) = body_enc {
            req_b = req_b.body(b);
        }
        let response = req_b.send().await.map_err(|e| NmxmApiError::NetworkError {
            url: url.clone(),
            source: e,
        })?;
        let status_code = response.status();
        // check content length in case of junk responses
        if let Some(len) = response.content_length()
            && len > (20 * 1024 * 1024)
        {
            return Err(NmxmApiError::APIError {
                url,
                status: status_code,
                message: format!("Content length {len} exceeds 20MB limit"),
            });
        }
        // get entire response body in bytes into a buffer, then try to convert to utf8 string
        let response_buffer = response
            .bytes()
            .await
            .map_err(|e| NmxmApiError::NoContent {
                url: url.clone(),
                source: e,
            })?;
        let response_body = String::from_utf8_lossy(&response_buffer).to_string();
        debug!(
            http_status = status_code.as_u16(),
            response_body = truncate(&response_body, 1500),
            "Received NMX-M response"
        );

        if !status_code.is_success() {
            return Err(NmxmApiError::HTTPErrorCode {
                url,
                status_code,
                response_body,
            });
        }

        let mut res = None;
        if !response_body.is_empty() {
            match serde_json::from_str(&response_body) {
                Ok(v) => res.insert(v),
                Err(e) => {
                    return Err(NmxmApiError::JsonDeserializeError {
                        url,
                        body: response_body,
                        source: e,
                    });
                }
            };
        }
        Ok((status_code, res))
    }
}

fn truncate(s: &str, len: usize) -> &str {
    &s[..len.min(s.len())]
}

#[async_trait::async_trait]
pub trait Nmxm: Send + Sync + 'static {
    async fn create(&self, endpoint: Endpoint) -> Result<Box<dyn Nmxm>, NmxmApiError>;
    async fn raw_get(&self, api: &str) -> Result<RawResponse, NmxmApiError>;
    async fn get_chassis(&self, id: String) -> Result<Vec<Chassis>, NmxmApiError>;
    async fn get_chassis_count(&self, domain: Option<Vec<uuid::Uuid>>)
    -> Result<i64, NmxmApiError>;
    //async fn get_chassis_list( &self, domain: Option<Vec<uuid::Uuid>>) -> Result<Vec<Chassis>, NmxmApiError>;
    async fn get_compute_node(&self, id: Option<String>) -> Result<Vec<ComputeNode>, NmxmApiError>;
    async fn get_compute_nodes_count(
        &self,
        domain: Option<Vec<uuid::Uuid>>,
    ) -> Result<i64, NmxmApiError>;
    //async fn get_compute_nodes_list( &self, domain: Option<Vec<uuid::Uuid>>) -> Result<Vec<ComputeNode>, NmxmApiError>;
    async fn get_gpu(&self, id: Option<String>) -> Result<Vec<Gpu>, NmxmApiError>;
    async fn get_gpu_count(&self, _domain: Option<Vec<uuid::Uuid>>) -> Result<i64, NmxmApiError>;
    async fn get_port(&self, id: Option<String>) -> Result<Vec<Port>, NmxmApiError>;
    async fn get_ports_count(&self, domain: Option<Vec<uuid::Uuid>>) -> Result<i64, NmxmApiError>;
    async fn get_switch_node(&self, id: Option<String>) -> Result<Vec<SwitchNode>, NmxmApiError>;
    async fn get_switch_nodes_count(
        &self,
        domain: Option<Vec<uuid::Uuid>>,
    ) -> Result<i64, NmxmApiError>;

    async fn get_partition(&self, id: String) -> Result<Partition, NmxmApiError>;
    async fn get_partitions_list(&self) -> Result<Vec<Partition>, NmxmApiError>;
    async fn create_partition(
        &self,
        req: Option<CreatePartitionRequest>,
    ) -> Result<AsyncResponse, NmxmApiError>;
    async fn delete_partition(&self, id: String) -> Result<AsyncResponse, NmxmApiError>;
    async fn update_partition(
        &self,
        id: String,
        req: UpdatePartitionRequest,
    ) -> Result<AsyncResponse, NmxmApiError>;

    async fn get_operation(&self, id: String) -> Result<Operation, NmxmApiError>;
    async fn get_operations_list(&self) -> Result<Vec<Operation>, NmxmApiError>;
    async fn cancel_operation(&self, id: String) -> Result<AsyncResponse, NmxmApiError>;
}
