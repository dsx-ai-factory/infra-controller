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

use std::convert::Infallible;
use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use std::{env, fs};

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderName, HeaderValue, Response, header};
use axum::response::IntoResponse;
use axum_server::tls_rustls::RustlsConfig;
use http_body::{Body as HttpBody, Frame};
use rustls::ServerConfig;
use rustls_pemfile::Item;
use rustls_pki_types::PrivateKeyDer;
use tempfile::{NamedTempFile, TempDir};

use crate::Options;

const TLS_CERT: &[u8] = include_bytes!("../../../test-certs/tls.crt");
const TLS_KEY: &[u8] = include_bytes!("../../../test-certs/tls.key");

// TODO: Add settings to config file and switch this to true
// Then assert that it works
const AGENT_CONFIG: &str = r#"
[forge-system]
api-server = "https://$API_SERVER"
pxe-server = "http://127.0.0.1:8080"
root-ca = "$ROOT_DIR/dev/certs/forge_root.pem"

[machine]
is-fake-dpu = true
interface-id = "f377ed72-d912-4879-958a-8d1f82a50d62"
mac-address = "11:22:33:44:55:66"
hostname = "abc.forge.example.com"

[hbn]
root-dir = "$HBN_ROOT"
skip-reload = true

[period]
main-loop-active-secs = 1
network-config-fetch-secs = 1
main-loop-idle-secs = 30
version-check-secs = 600
inventory-update-secs = 3600
discovery-retry-secs = 1
discovery-retries-max = 1000
"#;

pub(super) fn setup_agent_run_env(
    addr: &SocketAddr,
    td: &TempDir,
    acf: &NamedTempFile,
    test_metadata_service: bool,
) -> eyre::Result<Option<Options>> {
    let Ok(repo_root) = env::var("REPO_ROOT").or_else(|_| env::var("CONTAINER_REPO_ROOT")) else {
        tracing::warn!(
            "Either REPO_ROOT or CONTAINER_REPO_ROOT need to be set to run this test. Skipping."
        );
        return Ok(None);
    };
    let root_dir = PathBuf::from(repo_root);

    // SAFETY: Initial lint enablement: these test settings are installed before the
    // in-process agent starts, but the parallel test harness and Tokio runtimes may
    // already have other threads. Unix process-wide exclusion from environment readers
    // is not proven; this needs owner review.
    unsafe {
        env::set_var("DISABLE_TLS_ENFORCEMENT", "true");
        env::set_var("IGNORE_MGMT_VRF", "true");
        env::set_var("NO_DPU_CONTAINERS", "true");

        // Put our fake `crictl` on front of path so that HBN health checks succeed
        let dev_bin = root_dir.join("dev/bin");
        if let Some(path) = env::var_os("PATH") {
            let mut paths = env::split_paths(&path).collect::<Vec<_>>();
            paths.insert(0, dev_bin);
            let new_path = env::join_paths(paths)?;
            env::set_var("PATH", new_path);
        }
    }

    let hbn_root = td.path();
    tracing::info!(?hbn_root, "Using HBN root");
    fs::create_dir_all(hbn_root.join("etc/frr"))?;
    fs::create_dir_all(hbn_root.join("etc/network"))?;
    fs::create_dir_all(hbn_root.join("etc/supervisor/conf.d"))?;
    fs::create_dir_all(hbn_root.join("etc/cumulus/acl/policy.d"))?;
    fs::create_dir_all(hbn_root.join("var/support"))?;

    let cfg = AGENT_CONFIG
        .replace("$ROOT_DIR", &root_dir.display().to_string())
        .replace("$HBN_ROOT", &hbn_root.display().to_string())
        .replace("$API_SERVER", &addr.to_string());

    fs::write(acf.path(), cfg)?;
    let opts = crate::Options {
        version: false,
        config_path: Some(acf.path().to_path_buf()),
        cmd: Some(crate::AgentCommand::Run(Box::new(crate::RunOptions {
            enable_metadata_service: test_metadata_service,
            override_machine_id: None,
            override_network_virtualization_type: None,
            skip_upgrade_check: false,
            dhcp_grpc_server: None,
            fmds_grpc_server: None,
            fmds_connect_timeout_secs: 3,
            hbn_config_mode: crate::command_line::HbnConfigMode::ContainerExec,
            agent_platform_type: crate::command_line::AgentPlatformType::DpuOs,
            dhcp_server_interface_prepend: None,
        }))),
    };

    Ok(Some(opts))
}

pub(super) async fn run_grpc_server(
    app: axum::Router<()>,
) -> eyre::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind("127.0.0.1:0")?; // 0 let OS choose available port
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let server_config = make_rustls_server_config()?;
    let join_handle = tokio::spawn(async move {
        let config = RustlsConfig::from_config(Arc::new(server_config));
        axum_server::from_tcp_rustls(listener, config)
            // Safety: This only happens if the socket is nonblocking, and we already did set_nonblocking above.
            .expect("BUG: Could not bind to listener")
            .serve(app.into_make_service())
            .await
            .unwrap();
    });
    wait_for_server_to_start(addr).await?;

    Ok((addr, join_handle))
}

// Note: Axum has a simple RustlsConfig::from_pem we could use, but it constructs a rustls
// ServerConfig without a default crypto provider. So we have to make our own rustls::ServerConfig
// and pass that to RustlsConfig::from.
fn make_rustls_server_config() -> eyre::Result<ServerConfig> {
    let certs =
        rustls_pemfile::certs(&mut TLS_CERT.to_vec().as_ref()).collect::<Result<Vec<_>, _>>()?;

    // Check the entire PEM file for the key in case it is not first section
    let key = rustls_pemfile::read_all(&mut TLS_KEY.to_vec().as_ref())
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter_map(|i| match i {
            Item::Sec1Key(key) => Some(key.secret_sec1_der().to_vec()),
            Item::Pkcs1Key(key) => Some(key.secret_pkcs1_der().to_vec()),
            Item::Pkcs8Key(key) => Some(key.secret_pkcs8_der().to_vec()),
            _ => None,
        })
        .map(|data| PrivateKeyDer::try_from(data).map_err(|s| eyre::eyre!("{s}")))
        .next()
        .ok_or(eyre::eyre!("no keys in key file"))??;

    let mut server_config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(certs, key)?;
    // This is what axum is normally doing for you
    server_config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(server_config)
}

async fn wait_for_server_to_start(addr: SocketAddr) -> eyre::Result<()> {
    let url = format!("https://{addr}/up");
    let deadline = Instant::now() + Duration::from_secs(2);
    let client = reqwest::ClientBuilder::new()
        .danger_accept_invalid_certs(true)
        .build()?;
    while Instant::now() < deadline {
        match client.get(&url).send().await {
            Ok(resp) if resp.status() == reqwest::StatusCode::OK => {
                break;
            }
            Ok(resp) => {
                eyre::bail!(
                    "invalid status code from /up on mock grpc server: {}",
                    resp.status(),
                );
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    if Instant::now() >= deadline {
        eyre::bail!("timed out waiting for mock grpc server to start");
    }
    Ok(())
}

struct GrpcBody {
    data: Option<Bytes>,
    trailers: Option<HeaderMap>,
}

impl GrpcBody {
    fn new(data: Vec<u8>) -> Self {
        let mut trailers = HeaderMap::new();
        trailers.insert(
            HeaderName::from_static("grpc-status"),
            HeaderValue::from_static("0"),
        );

        Self {
            data: Some(Bytes::from(data)),
            trailers: Some(trailers),
        }
    }
}

impl HttpBody for GrpcBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        let this = self.get_mut();
        if let Some(data) = this.data.take() {
            return Poll::Ready(Some(Ok(Frame::data(data))));
        }
        if let Some(trailers) = this.trailers.take() {
            return Poll::Ready(Some(Ok(Frame::trailers(trailers))));
        }

        Poll::Ready(None)
    }

    fn is_end_stream(&self) -> bool {
        self.data.is_none() && self.trailers.is_none()
    }
}

/// Takes an rpc object (built from rpc/proto/forge.proto) and turns into into a gRPC axum response
pub(super) fn respond(out: impl prost::Message) -> impl IntoResponse {
    let msg_len = out.encoded_len() as u32;
    let mut body = Vec::with_capacity(1 + 4 + msg_len as usize);
    // first byte is compression: 0 means none
    body.push(0u8);
    // next four bytes are length as bigendian u32
    body.extend_from_slice(&msg_len.to_be_bytes());
    // and finally the message
    out.encode(&mut body).unwrap();

    Response::builder()
        .header(header::CONTENT_TYPE, "application/grpc+tonic")
        .body(GrpcBody::new(body))
        .unwrap()
}
