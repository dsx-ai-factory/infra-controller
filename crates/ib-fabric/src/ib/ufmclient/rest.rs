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
use std::fmt::{Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use forge_tls::dummy_tls_verifier::DummyTlsVerifier;
use http_body_util::BodyExt;
use hyper::header::{AUTHORIZATION, CONTENT_TYPE, USER_AGENT};
use hyper::http::StatusCode;
use hyper::{Method, Uri};
use hyper_rustls::HttpsConnector;
use hyper_timeout::TimeoutConnector;
use hyper_util::client::legacy::Client as HyperClient;
use hyper_util::client::legacy::connect::HttpConnector;
use hyper_util::rt::TokioExecutor;
use rustls::{ClientConfig, ConfigBuilder, RootCertStore, WantsVerifier};
use thiserror::Error;

use crate::ib::ufmclient::UFMCert;

#[derive(Error, Debug)]
pub(super) enum RestError {
    #[error("invalid configuration: '{0}'")]
    InvalidConfig(String),
    #[error("response body can not be deserialized: {body}")]
    MalformedResponse {
        status_code: u16,
        body: String,
        headers: Box<http::HeaderMap>,
    },
    #[error("failed to execute HTTP request: {0}")]
    HttpConnectionError(String),
    #[error("HTTP error code {status_code}")]
    HttpError {
        status_code: u16,
        body: String,
        headers: Box<http::HeaderMap>,
    },
    /// This error type is just needed because UFM in some cases does not return a 404 status
    /// code but a 200 status code with a body containing {}
    #[error(
        "resource at path {path} was not found. UFM returned: '{body}'. status code: {status_code}"
    )]
    NotFound {
        path: String,
        status_code: u16,
        body: String,
        headers: Box<http::HeaderMap>,
    },
}

impl From<hyper::Error> for RestError {
    fn from(value: hyper::Error) -> Self {
        RestError::HttpConnectionError(value.to_string())
    }
}

const REST_TIME_OUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub(super) enum RestScheme {
    Http,
    Https,
}

impl From<String> for RestScheme {
    fn from(value: String) -> Self {
        match value.to_uppercase().as_str() {
            "HTTP" => RestScheme::Http,
            "HTTPS" => RestScheme::Https,
            _ => RestScheme::Http,
        }
    }
}

impl Display for RestScheme {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            RestScheme::Http => write!(f, "http"),
            RestScheme::Https => write!(f, "https"),
        }
    }
}

pub(super) struct RestClientConfig {
    pub(super) address: String,
    pub(super) port: Option<u16>,
    pub(super) scheme: RestScheme,
    pub(super) auth_info: String,
    pub(super) base_path: String,
}

pub(super) struct RestClient {
    base_url: String,
    auth_info: String,
    scheme: RestScheme,
    http_client: HyperClient<TimeoutConnector<HttpConnector>, String>,
    https_client: HyperClient<TimeoutConnector<HttpsConnector<HttpConnector>>, String>,
}

impl RestClient {
    pub(super) fn new(conf: &RestClientConfig) -> Result<RestClient, RestError> {
        let mut auth_info = conf.auth_info.clone().trim().to_string();
        let mut auto_cert: Option<UFMCert> = None;

        if auth_info.chars().filter(|c| *c == '\n').count() == 2 {
            let mut v = auth_info.split('\n');
            auto_cert = Some(UFMCert {
                ca_crt: v.next().unwrap_or("").to_string(),
                tls_key: v.next().unwrap_or("").to_string(),
                tls_crt: v.next().unwrap_or("").to_string(),
            });
            auth_info = "".to_string();
        } else {
            auth_info = format!("Basic {}", conf.auth_info.clone().trim());
        }

        let base_url = match &conf.port {
            None => format!(
                "{}://{}/{}",
                conf.scheme,
                conf.address,
                conf.base_path.trim_matches('/')
            ),
            Some(p) => format!(
                "{}://{}:{}/{}",
                conf.scheme,
                conf.address,
                p,
                conf.base_path.trim_matches('/')
            ),
        };

        let _ = base_url
            .parse::<Uri>()
            .map_err(|_| RestError::InvalidConfig("invalid rest address".to_string()))?;

        let mut http_connector = TimeoutConnector::new(HttpConnector::new());
        http_connector.set_connect_timeout(Some(REST_TIME_OUT));
        http_connector.set_read_timeout(Some(REST_TIME_OUT));
        http_connector.set_write_timeout(Some(REST_TIME_OUT));

        let config = if let Some(auto_cert) = &auto_cert {
            // Get CA root
            let mut roots = RootCertStore::empty();
            let fd = match std::fs::File::open(auto_cert.ca_crt.clone()) {
                Ok(fd) => fd,
                Err(_) => {
                    return Err(RestError::InvalidConfig(format!(
                        "Root CA file not found at '{}'",
                        auto_cert.ca_crt.clone()
                    )));
                }
            };
            let mut buf = std::io::BufReader::new(&fd);
            let root_ca_certs = rustls_pemfile::certs(&mut buf)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| {
                    RestError::InvalidConfig(format!(
                        "Error reading Root CA file at '{}': {}",
                        auto_cert.tls_crt.clone(),
                        e
                    ))
                })?;
            let (_added, _ignored) = roots.add_parsable_certificates(root_ca_certs);

            // Get client certificate
            let certs = std::fs::File::open(auto_cert.tls_crt.clone())
                .and_then(|fd| {
                    let mut buf = std::io::BufReader::new(&fd);
                    rustls_pemfile::certs(&mut buf).collect::<Result<Vec<_>, _>>()
                })
                .map_err(|e| {
                    RestError::InvalidConfig(format!(
                        "Error reading client cert at '{}': {}",
                        auto_cert.tls_crt.clone(),
                        e
                    ))
                })?;

            // Get client private key
            let key = {
                let fd = match std::fs::File::open(auto_cert.tls_key.clone()) {
                    Ok(fd) => fd,
                    Err(_) => {
                        return Err(RestError::InvalidConfig(format!(
                            "Client Private Key file not found at '{}'",
                            auto_cert.tls_key.clone()
                        )));
                    }
                };
                let mut buf = std::io::BufReader::new(&fd);
                use rustls_pemfile::Item;
                match rustls_pemfile::read_one(&mut buf) {
                    Ok(Some(item)) => match item {
                        Item::Pkcs1Key(key) => Some(key.into()),
                        Item::Pkcs8Key(key) => Some(key.into()),
                        Item::Sec1Key(key) => Some(key.into()),
                        Item::X509Certificate(_) => {
                            return Err(RestError::InvalidConfig(format!(
                                "Expected Client Private Key but certificate is found '{}'",
                                auto_cert.tls_key.clone()
                            )));
                        }
                        Item::Crl(_) => {
                            return Err(RestError::InvalidConfig(format!(
                                "Expected Client Private Key but certificate revocation list is found '{}'",
                                auto_cert.tls_key
                            )));
                        }
                        _ => {
                            return Err(RestError::InvalidConfig(format!(
                                "Client Private Key is corrupted '{}'",
                                auto_cert.tls_key.clone()
                            )));
                        }
                    },
                    _ => {
                        return Err(RestError::InvalidConfig(format!(
                            "Client Private Key file not found at '{}'",
                            auto_cert.tls_key.clone()
                        )));
                    }
                }
            };

            let build_no_client_auth_config = || {
                rustls_client_builder()
                    .with_root_certificates(roots.clone())
                    .with_no_client_auth()
            };

            if !certs.is_empty()
                && let Some(key) = key
            {
                if let Ok(config) = rustls_client_builder()
                    .with_root_certificates(roots.clone())
                    .with_client_auth_cert(certs, key)
                {
                    // Use TLS flow with client authentication
                    config
                } else {
                    // Client creation failure
                    build_no_client_auth_config()
                }
            } else {
                // Unable to use client cert/key pair
                build_no_client_auth_config()
            }
        } else {
            rustls_client_builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(DummyTlsVerifier::new_for_prod()))
                .with_no_client_auth()
        };

        let mut https_connector = TimeoutConnector::new(
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_tls_config(config)
                .https_or_http()
                .enable_http1()
                .enable_http2()
                .build(),
        );
        https_connector.set_connect_timeout(Some(REST_TIME_OUT));
        https_connector.set_read_timeout(Some(REST_TIME_OUT));
        https_connector.set_write_timeout(Some(REST_TIME_OUT));

        Ok(Self {
            base_url,
            auth_info,
            scheme: conf.scheme.clone(),
            http_client: HyperClient::builder(TokioExecutor::new()).build(http_connector),
            https_client: HyperClient::builder(TokioExecutor::new()).build(https_connector),
        })
    }

    pub(super) async fn get<'a, T: serde::de::DeserializeOwned>(
        &'a self,
        path: &'a str,
    ) -> Result<(T, ResponseDetails), RestError> {
        let resp = self.execute_request(Method::GET, path, None).await?;
        // UFM uses {} with a 200 status code to indicate "Not Found"
        if resp.body == "{}" {
            return Err(RestError::NotFound {
                path: path.to_string(),
                status_code: resp.details.status_code,
                body: resp.body,
                headers: Box::new(resp.details.headers),
            });
        }

        let data = match serde_json::from_str(&resp.body) {
            Ok(data) => data,
            Err(_) => {
                return Err(RestError::MalformedResponse {
                    status_code: resp.details.status_code,
                    headers: Box::new(resp.details.headers),
                    body: resp.body,
                });
            }
        };

        Ok((data, resp.details))
    }

    /// Performs a HTTP GET request anad returns the response directly without trying to deserialize it
    pub(super) async fn get_raw<'a>(
        &'a self,
        path: &'a str,
    ) -> Result<(String, ResponseDetails), RestError> {
        let resp = self.execute_request(Method::GET, path, None).await?;
        Ok((resp.body, resp.details))
    }

    pub(super) async fn list<'a, T: serde::de::DeserializeOwned>(
        &'a self,
        path: &'a str,
    ) -> Result<(T, ResponseDetails), RestError> {
        let resp = self.execute_request(Method::GET, path, None).await?;

        let data = match serde_json::from_str(&resp.body) {
            Ok(data) => data,
            Err(_) => {
                return Err(RestError::MalformedResponse {
                    status_code: resp.details.status_code,
                    headers: Box::new(resp.details.headers),
                    body: resp.body,
                });
            }
        };

        Ok((data, resp.details))
    }

    pub(super) async fn post(
        &self,
        path: &str,
        data: String,
    ) -> Result<ResponseDetails, RestError> {
        let resp = self.execute_request(Method::POST, path, Some(data)).await?;

        Ok(resp.details)
    }

    pub(super) async fn put(&self, path: &str, data: String) -> Result<ResponseDetails, RestError> {
        let resp = self.execute_request(Method::PUT, path, Some(data)).await?;

        Ok(resp.details)
    }

    async fn execute_request(
        &self,
        method: Method,
        path: &str,
        data: Option<String>,
    ) -> Result<ExecuteRequestResult, RestError> {
        let url = format!("{}/{}", self.base_url, path.trim_matches('/'));
        let uri = url
            .parse::<Uri>()
            .map_err(|_| RestError::InvalidConfig("invalid path".to_string()))?;

        let body = data.unwrap_or_default();

        let req = hyper::Request::builder()
            .method(method)
            .uri(uri)
            .header(USER_AGENT, env!("CARGO_PKG_NAME"))
            .header(CONTENT_TYPE, "application/json")
            .header(AUTHORIZATION, self.auth_info.to_string())
            .body(body)
            .map_err(|_| RestError::InvalidConfig("invalid rest request".to_string()))?;

        let response = match &self.scheme {
            RestScheme::Http => self.http_client.request(req).await.map_err(|e| {
                RestError::HttpConnectionError(format!("Rest request failure: {e:?}"))
            })?,
            RestScheme::Https => self.https_client.request(req).await.map_err(|e| {
                RestError::HttpConnectionError(format!("Rest request failure: {e:?}"))
            })?,
        };

        let status = response.status();
        let headers = response.headers().clone();
        let body =
            String::from_utf8(response.into_body().collect().await?.to_bytes().into()).unwrap();

        match status {
            StatusCode::OK | StatusCode::CREATED => Ok(ExecuteRequestResult {
                details: ResponseDetails {
                    status_code: status.as_u16(),
                    headers,
                },
                body,
            }),
            status => Err(RestError::HttpError {
                status_code: status.as_u16(),
                body,
                headers: Box::new(headers),
            }),
        }
    }
}

pub(in crate::ib) struct ResponseDetails {
    pub(in crate::ib) status_code: u16,
    pub(in crate::ib) headers: http::HeaderMap,
}

struct ExecuteRequestResult {
    body: String,
    details: ResponseDetails,
}

// Wrap ClientConfig::builder_with_provider() with defaults
fn rustls_client_builder() -> ConfigBuilder<ClientConfig, WantsVerifier> {
    ClientConfig::builder_with_provider(Arc::new(rustls::crypto::aws_lc_rs::default_provider()))
        .with_safe_default_protocol_versions()
        // unwrap safety: the error only comes if the configured protocol versions are
        // invalid, which should never happen with the safe defaults.
        .unwrap()
}
