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
use std::net::SocketAddr;
use std::time::Duration;

use eyre::{Context, ContextCompat};
use forge_tls::client_config::ClientCert;
use rand::prelude::IndexedRandom;
use rpc::forge_tls_client::{ForgeClientConfig, ForgeClientT, ForgeTlsClient};
use tonic::{Response, Status};

use crate::utils::LOCALHOST_CERTS;

const RPC_TIMEOUT: Duration = Duration::from_secs(60);

pub(crate) async fn call<T, F, Fut>(
    addrs: &[SocketAddr],
    rpc_name: &'static str,
    call_rpc: F,
) -> eyre::Result<T>
where
    F: FnOnce(ForgeClientT) -> Fut,
    Fut: Future<Output = Result<Response<T>, Status>>,
{
    tokio::time::timeout(RPC_TIMEOUT, async {
        let address = addrs
            .choose(&mut rand::rng())
            .context("no API servers configured")?;
        let client_cert = ClientCert {
            cert_path: LOCALHOST_CERTS.client_cert.to_string_lossy().into_owned(),
            key_path: LOCALHOST_CERTS.client_key.to_string_lossy().into_owned(),
        };
        let config = ForgeClientConfig::new(
            LOCALHOST_CERTS.ca_cert.to_string_lossy().into_owned(),
            Some(client_cert),
        );
        let client = ForgeTlsClient::new(&config)
            .build(format!("https://{address}"))
            .await
            .wrap_err_with(|| format!("building client for {rpc_name}"))?;

        call_rpc(client)
            .await
            .map(Response::into_inner)
            .wrap_err_with(|| format!("calling {rpc_name}"))
    })
    .await
    .map_err(|_| eyre::eyre!("{rpc_name} timed out after {RPC_TIMEOUT:?}"))?
}
