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

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use ::rpc::forge as rpc;
use carbide_utils::HostPortPair;
use tonic::{Request, Response, Status};

use crate::CarbideError;
use crate::api::{Api, log_request_data};

fn advertised_build_capabilities() -> Vec<i32> {
    vec![rpc::BuildCapability::VpcSlaac as i32]
}

pub(crate) async fn version(
    api: &Api,
    request: Request<rpc::VersionRequest>,
) -> Result<Response<rpc::BuildInfo>, Status> {
    log_request_data(&request);
    let version_request = request.into_inner();

    let runtime_config = if version_request.display_config {
        let config = api.runtime_config.redacted();
        let retained_operator_roots = if config.site_fabric_null_routes.is_none() {
            ::db::site_prefix::find_operator_managed_prefixes_with_retained_vpc_prefixes(
                &api.database_connection,
            )
            .await
            .map_err(CarbideError::from)?
        } else {
            vec![]
        };
        let effective_null_routes = config
            .resolved_site_fabric_null_routes(&retained_operator_roots)
            .into_iter()
            .map(|prefix| prefix.to_string())
            .collect();
        let mut runtime_config: rpc::RuntimeConfig = config.into();
        runtime_config.site_fabric_null_routes = Some(::rpc::common::StringList {
            items: effective_null_routes,
        });
        Some(runtime_config)
    } else {
        None
    };

    let v = rpc::BuildInfo {
        build_version: carbide_version::v!(build_version).to_string(),
        build_date: carbide_version::v!(build_date).to_string(),
        git_sha: carbide_version::v!(git_sha).to_string(),
        rust_version: carbide_version::v!(rust_version).to_string(),
        build_user: carbide_version::v!(build_user).to_string(),
        build_hostname: carbide_version::v!(build_hostname).to_string(),
        capabilities: advertised_build_capabilities(),

        runtime_config,
    };
    Ok(Response::new(v))
}

pub(crate) fn echo(
    _api: &Api,
    request: Request<rpc::EchoRequest>,
) -> Result<Response<rpc::EchoResponse>, Status> {
    log_request_data(&request);

    let reply = rpc::EchoResponse {
        message: request.into_inner().message,
    };

    Ok(Response::new(reply))
}

// Override RUST_LOG or site-explorer create_machines
pub(crate) fn set_dynamic_config(
    api: &Api,
    request: Request<rpc::SetDynamicConfigRequest>,
) -> Result<Response<()>, Status> {
    log_request_data(&request);

    let req = request.into_inner();
    let exp_str = req.expiry.as_deref().unwrap_or("1h");
    let expiry = duration_str::parse(exp_str).map_err(|err| {
        CarbideError::InvalidArgument(format!("invalid expiry string '{exp_str}'. {err}"))
    })?;
    const MAX_SET_INTERNAL_EXPIRY: Duration = Duration::from_secs(60 * 60 * 60); // 60 hours
    if MAX_SET_INTERNAL_EXPIRY < expiry {
        return Err(CarbideError::InvalidArgument(
            "expiry exceeds max allowed of 60 hours".to_string(),
        )
        .into());
    }
    let expire_at = chrono::Utc::now() + expiry;

    let Ok(requested_setting) = rpc::ConfigSetting::try_from(req.setting) else {
        return Err(CarbideError::InvalidArgument(format!(
            "not a supported dynamic config setting: {}",
            req.setting
        ))
        .into());
    };

    if req.value.is_empty() && !matches!(requested_setting, rpc::ConfigSetting::BmcProxy) {
        return Err(CarbideError::InvalidArgument("'value' cannot be empty".to_string()).into());
    }

    match requested_setting {
        rpc::ConfigSetting::LogFilter => {
            let level = &api.dynamic_settings.log_filter;
            level.update(&req.value, Some(expire_at)).map_err(|err| {
                CarbideError::InvalidArgument(format!(
                    "invalid log filter string '{}'. {err}",
                    req.value
                ))
            })?;
            tracing::info!(
                log_filter = %req.value,
                configured_log_level = %tracing_subscriber::filter::LevelFilter::current(),
                "Log filter updated",
            );
        }
        rpc::ConfigSetting::CreateMachines => {
            let is_enabled = req.value.parse::<bool>().map_err(|err| {
                CarbideError::InvalidArgument(format!(
                    "invalid create_machines string '{}'. {err}",
                    req.value
                ))
            })?;
            api.dynamic_settings
                .create_machines
                .store(is_enabled, Ordering::Relaxed);
            tracing::info!(
                create_machines = is_enabled,
                "site-explorer create_machines setting updated",
            );
        }
        rpc::ConfigSetting::SiteExplorerEnabled => {
            let is_enabled = req.value.parse::<bool>().map_err(|err| {
                CarbideError::InvalidArgument(format!(
                    "invalid site_explorer_enabled string '{}'. {err}",
                    req.value
                ))
            })?;
            api.dynamic_settings
                .site_explorer_enabled
                .store(is_enabled, Ordering::Relaxed);
            tracing::info!(
                site_explorer_enabled = is_enabled,
                "site-explorer enabled setting updated",
            );
        }
        rpc::ConfigSetting::BmcProxy => {
            let Some(true) = api.runtime_config.site_explorer.allow_changing_bmc_proxy else {
                return Err(CarbideError::PermissionDeniedError(
                    "site-explorer.bmc_proxy is not allowed to be changed on this server".into(),
                )
                .into());
            };

            if req.value.is_empty() {
                api.dynamic_settings.bmc_proxy.store(Arc::new(None))
            } else {
                let host_port_pair = req.value.parse::<HostPortPair>().map_err(|err| {
                    CarbideError::InvalidArgument(format!(
                        "invalid bmc_proxy string '{}': {err}",
                        req.value
                    ))
                })?;

                api.dynamic_settings
                    .bmc_proxy
                    .store(Arc::new(Some(host_port_pair)));
            }
            tracing::info!(
                bmc_proxy = %req.value,
                "BMC proxy setting updated",
            );
        }
        rpc::ConfigSetting::TracingEnabled => {
            if !api.runtime_config.tracing.allow_runtime_changes {
                return Err(CarbideError::PermissionDeniedError(
                    "this server does not allow runtime changes to tracing configuration"
                        .to_string(),
                )
                .into());
            }
            let enable = req.value.parse().map_err(|_| {
                CarbideError::InvalidArgument(format!(
                    "expected bool for TracingEnabled, got {}",
                    req.value
                ))
            })?;
            api.dynamic_settings
                .tracing_enabled
                .store(enable, Ordering::Relaxed);
        }
    }
    Ok(Response::new(()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advertised_build_capabilities_include_vpc_slaac() {
        assert!(advertised_build_capabilities().contains(&(rpc::BuildCapability::VpcSlaac as i32)));
    }
}
