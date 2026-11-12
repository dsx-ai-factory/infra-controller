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

use ::rpc::carbide_tls_client::{self, ApiConfig, CarbideClientConfig};
use forge_tls::client_config::ClientCert;
use scout::{CarbideClientError, CarbideClientResult};

use crate::Options;

/// The client TLS config every scout connection to the API uses. Callers that
/// hold a long-lived client of their own build it from here so none of them
/// silently drops the node-auth JWT.
pub(super) fn carbide_client_config(config: &Options) -> CarbideClientConfig {
    CarbideClientConfig::new(
        config.root_ca.clone(),
        Some(ClientCert {
            cert_path: config.client_cert.clone(),
            key_path: config.client_key.clone(),
        }),
    )
    // Node-auth (#355): also present a self-signed bearer JWT minted from the
    // client cert's key. Ignored by the API unless [node_auth] is enabled.
    .with_node_jwt()
}

pub(super) async fn create_forge_client(
    config: &Options,
) -> CarbideClientResult<carbide_tls_client::CarbideClientT> {
    let client_config = carbide_client_config(config);
    let api_config = ApiConfig::new(&config.api, &client_config);

    let client = carbide_tls_client::CarbideTlsClient::retry_build(&api_config)
        .await
        .map_err(|err| CarbideClientError::TransportError(err.to_string()))?;
    Ok(client)
}

/// Capture the actual Scout transport settings for one admitted SiteCache job.
/// Relative paths retain Scout's working-directory meaning while certificate
/// symlinks remain unresolved so later requests can read rotated material.
pub(super) fn firmware_download_config(
    options: &Options,
) -> Result<
    libmlx::firmware::core_download::CoreFirmwareDownloadConfig,
    libmlx::firmware::operation::SupervisorLaunchError,
> {
    use libmlx::firmware::core_download::CoreFirmwareDownloadConfig;
    use libmlx::firmware::operation::SupervisorLaunchError;
    let config = carbide_client_config(options);
    let invalid = || {
        SupervisorLaunchError::InvalidConfiguration("core firmware transport settings are invalid")
    };
    let directory = std::env::current_dir();
    let absolute = |path: String| {
        let path = std::path::PathBuf::from(path);
        let path = if path.is_absolute() {
            path
        } else {
            directory.as_ref().map_err(|_| invalid())?.join(path)
        };
        path.into_os_string().into_string().map_err(|_| invalid())
    };
    let certificate = config.client_cert.ok_or_else(invalid)?;
    let snapshot = CoreFirmwareDownloadConfig {
        core_url: options.api.clone(),
        root_ca_path: absolute(config.root_ca_path)?,
        client_cert_path: absolute(certificate.cert_path)?,
        client_key_path: absolute(certificate.key_path)?,
        enforce_tls: config.enforce_tls,
        require_client_cert: config.require_client_cert,
        use_mgmt_vrf: config.use_mgmt_vrf,
        request_timeout: config.request_timeout,
        socks_proxy: config.socks_proxy,
        connect_retries_max: config.connect_retries_max,
        connect_retries_interval: config.connect_retries_interval,
    };
    snapshot.validate().map_err(|_| invalid())?;
    Ok(snapshot)
}
