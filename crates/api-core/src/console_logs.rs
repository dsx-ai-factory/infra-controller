/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use async_trait::async_trait;
use futures::Stream;
use futures_util::StreamExt;
use rpc::protos::console_log::console_log_service_client::ConsoleLogServiceClient;
use rpc::protos::console_log::{ConsoleLogLine, StreamConsoleLogsRequest};
use tokio::task::JoinSet;
use tokio_util::sync::{CancellationToken, DropGuard};
use tonic::transport::{Certificate, Channel, ClientTlsConfig, Endpoint, Identity};
use tonic::{Request, Status};
use url::Url;

use crate::cfg::file::TlsConfig;

pub type ConsoleLogStream =
    Pin<Box<dyn Stream<Item = Result<ConsoleLogLine, Status>> + Send + 'static>>;

#[async_trait]
pub trait ConsoleLogSource: Send + Sync {
    async fn stream(&self, request: StreamConsoleLogsRequest) -> Result<ConsoleLogStream, Status>;
}

pub(crate) struct UnavailableSource;

#[async_trait]
impl ConsoleLogSource for UnavailableSource {
    async fn stream(&self, _request: StreamConsoleLogsRequest) -> Result<ConsoleLogStream, Status> {
        Err(Status::failed_precondition(
            "ssh-console log streaming is not configured",
        ))
    }
}

pub(crate) struct GrpcSource {
    url: Url,
    tls: TlsConfig,
    client: Arc<ArcSwap<ConsoleLogServiceClient<Channel>>>,
    _drop_guard: DropGuard,
}

impl GrpcSource {
    pub(crate) async fn connect(
        url: &Url,
        tls: &TlsConfig,
        join_set: &mut JoinSet<()>,
        cancel_token: CancellationToken,
    ) -> eyre::Result<Self> {
        static REFRESH_INTERVAL: Duration = Duration::from_mins(5);
        static REFRESH_RETRY_DELAY: Duration = Duration::from_secs(15);

        let initial = build_client(url, tls).await?;
        let (cancel_token, _drop_guard) = {
            let child = cancel_token.child_token();
            (child.clone(), child.drop_guard())
        };
        let source = Self {
            url: url.clone(),
            tls: tls.clone(),
            client: Arc::new(ArcSwap::from_pointee(initial)),
            _drop_guard,
        };
        let refresh_url = source.url.clone();
        let refresh_tls = source.tls.clone();
        let refresh_client = source.client.clone();
        join_set.spawn(async move {
            let mut delay = REFRESH_INTERVAL;
            while let Some(()) = cancel_token
                .run_until_cancelled(tokio::time::sleep(delay))
                .await
            {
                match build_client(&refresh_url, &refresh_tls).await {
                    Ok(client) => {
                        refresh_client.store(Arc::new(client));
                        delay = REFRESH_INTERVAL;
                    }
                    Err(error) => {
                        tracing::error!(%error, "could not refresh ssh-console TLS transport; retaining last good transport");
                        delay = REFRESH_RETRY_DELAY;
                    }
                }
            }
            tracing::debug!("ssh-console reconnection loop shutting down");
        });
        Ok(source)
    }

    async fn rebuild(&self) -> Result<ConsoleLogServiceClient<Channel>, Status> {
        let client = build_client(&self.url, &self.tls)
            .await
            .map_err(|error| Status::unavailable(error.to_string()))?;
        self.client.store(Arc::new(client.clone()));
        Ok(client)
    }
}

async fn build_client(
    url: &url::Url,
    tls: &TlsConfig,
) -> eyre::Result<ConsoleLogServiceClient<Channel>> {
    let domain = url
        .host()
        .ok_or_else(|| eyre::eyre!("ssh_console_url has no DNS host"))?;
    let ca = tokio::fs::read(&tls.root_cafile_path).await?;
    let cert = tokio::fs::read(&tls.identity_pemfile_path).await?;
    let key = tokio::fs::read(&tls.identity_keyfile_path).await?;
    let tls = ClientTlsConfig::new()
        .ca_certificate(Certificate::from_pem(ca))
        .identity(Identity::from_pem(cert, key))
        .domain_name(domain.to_string());
    let channel = Endpoint::from_shared(url.to_string())?
        .tls_config(tls)?
        .connect_lazy();
    Ok(ConsoleLogServiceClient::new(channel))
}

#[async_trait]
impl ConsoleLogSource for GrpcSource {
    async fn stream(&self, request: StreamConsoleLogsRequest) -> Result<ConsoleLogStream, Status> {
        let is_transport_error = |status: &Status| {
            matches!(
                status.code(),
                tonic::Code::Unavailable | tonic::Code::Unknown
            )
        };

        let mut client = self.client.load_full().as_ref().clone();
        let response = match client
            .stream_console_logs(Request::new(request.clone()))
            .await
        {
            Ok(response) => response,
            Err(status) if is_transport_error(&status) => self
                .rebuild()
                .await?
                .stream_console_logs(Request::new(request))
                .await
                .map_err(|e| {
                    if is_transport_error(&e) {
                        Status::unavailable(e.message().to_owned())
                    } else {
                        e
                    }
                })?,
            Err(status) => return Err(status),
        };
        Ok(response.into_inner().boxed())
    }
}

pub(crate) async fn build_source(
    url: Option<&Url>,
    tls: Option<&TlsConfig>,
    join_set: &mut JoinSet<()>,
    cancel_token: CancellationToken,
) -> eyre::Result<Arc<dyn ConsoleLogSource>> {
    match url {
        None => Ok(Arc::new(UnavailableSource)),
        Some(url) => {
            let tls =
                tls.ok_or_else(|| eyre::eyre!("ssh_console_url requires TLS configuration"))?;
            Ok(Arc::new(
                GrpcSource::connect(url, tls, join_set, cancel_token).await?,
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unconfigured_source_returns_failed_precondition() {
        let error = UnavailableSource
            .stream(StreamConsoleLogsRequest::default())
            .await
            .err()
            .expect("unconfigured source must fail");
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    }

    #[tokio::test]
    async fn configured_source_requires_tls_configuration() {
        let mut join_set = JoinSet::new();
        let cancel_token = CancellationToken::new();
        let error = build_source(
            Some(&"https://ssh-console.example:1079".parse().unwrap()),
            None,
            &mut join_set,
            cancel_token.clone(),
        )
        .await
        .err()
        .expect("TLS configuration is required");
        assert!(error.to_string().contains("requires TLS configuration"));
    }
}
