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

use std::ffi::CStr;

use ::rpc::forge as rpc;
use ::rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig};
use libc::c_char;

use crate::{CONFIG, CarbideDhcpContext, tls};

/// Result codes for the lease expiration FFI call.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LeaseExpirationResult {
    Success = 0,
    FeatureDisabled = 1,
    InvalidAddress = 2,
    ApiError = 3,
}

/// Called from the C++ lease4_expire / lease6_expire callouts to release
/// an IP allocation from the carbide database when Kea expires a lease.
///
/// # Safety
///
/// `ip_address` must be a valid, null-terminated C string.
/// `mac_address`, if non-null, must be a valid, null-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_expire_lease(
    ip_address: *const c_char,
    mac_address: *const c_char,
) -> LeaseExpirationResult {
    // SAFETY: Kea keeps the required IP C string readable and NUL-terminated
    // through this synchronous call, including its nested `block_on`.
    let ip_str = unsafe {
        match CStr::from_ptr(ip_address).to_str() {
            Ok(s) => s,
            Err(_) => return LeaseExpirationResult::InvalidAddress,
        }
    };

    let mac_str = if mac_address.is_null() {
        None
    } else {
        // SAFETY: A non-null optional MAC is a NUL-terminated Kea string that
        // remains readable through this synchronous call.
        unsafe {
            match CStr::from_ptr(mac_address).to_str() {
                Ok(s) if !s.is_empty() => Some(s),
                Ok(_) => None,
                Err(_) => return LeaseExpirationResult::InvalidAddress,
            }
        }
    };

    let url = &CONFIG.read().unwrap().api_endpoint;
    let forge_client_config = tls::build_forge_client_config();
    expire_lease_at(ip_str, mac_str, url, &forge_client_config)
}

fn expire_lease_at(
    ip_str: &str,
    mac_str: Option<&str>,
    url: &str,
    client_config: &ForgeClientConfig,
) -> LeaseExpirationResult {
    let runtime = CarbideDhcpContext::get_tokio_runtime();

    let result = runtime.block_on(async {
        let api_config = ApiConfig::new(url, client_config);
        let mut client = forge_tls_client::ForgeTlsClient::retry_build(&api_config)
            .await
            .map_err(|e| format!("unable to connect to Carbide API: {e:?}"))?;
        client
            .expire_dhcp_lease(tonic::Request::new(rpc::ExpireDhcpLeaseRequest {
                ip_address: ip_str.to_string(),
                mac_address: mac_str.map(|m| m.to_string()),
            }))
            .await
            .map_err(|e| format!("expire_dhcp_lease RPC failed: {e:?}"))
    });

    match result {
        Ok(response) => {
            let resp = response.into_inner();
            let status = rpc::ExpireDhcpLeaseStatus::try_from(resp.status)
                .unwrap_or(rpc::ExpireDhcpLeaseStatus::NotFound);
            match status {
                rpc::ExpireDhcpLeaseStatus::Released => {
                    log::info!("Released expired lease for {ip_str}");
                }
                rpc::ExpireDhcpLeaseStatus::NotFound => {
                    log::info!("No allocation found for expired lease {ip_str}");
                }
                rpc::ExpireDhcpLeaseStatus::FeatureDisabled => {
                    log::info!("Feature is disabled at NICo");
                    return LeaseExpirationResult::FeatureDisabled;
                }
            }
            LeaseExpirationResult::Success
        }
        Err(e) => {
            log::error!("Failed to release expired lease for {ip_str}: {e}");
            LeaseExpirationResult::ApiError
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mock_api_server;

    #[test]
    fn test_expire_lease_success() {
        let rt = CarbideDhcpContext::get_tokio_runtime();
        let api_server = rt.block_on(mock_api_server::MockAPIServer::start());
        let client_config = tls::build_forge_client_config();

        let result = expire_lease_at(
            "10.0.0.1",
            None,
            api_server.local_http_addr(),
            &client_config,
        );

        assert_eq!(result, LeaseExpirationResult::Success);
        assert_eq!(
            api_server.calls_for(mock_api_server::ENDPOINT_EXPIRE_DHCP_LEASE),
            1
        );
    }

    #[test]
    fn test_expire_lease_idempotent() {
        let rt = CarbideDhcpContext::get_tokio_runtime();
        let api_server = rt.block_on(mock_api_server::MockAPIServer::start());
        let client_config = tls::build_forge_client_config();

        let result1 = expire_lease_at(
            "10.0.0.1",
            None,
            api_server.local_http_addr(),
            &client_config,
        );
        let result2 = expire_lease_at(
            "10.0.0.1",
            None,
            api_server.local_http_addr(),
            &client_config,
        );

        assert_eq!(result1, LeaseExpirationResult::Success);
        assert_eq!(result2, LeaseExpirationResult::Success);
        assert_eq!(
            api_server.calls_for(mock_api_server::ENDPOINT_EXPIRE_DHCP_LEASE),
            2
        );
    }

    #[test]
    fn test_expire_lease_ipv6() {
        let rt = CarbideDhcpContext::get_tokio_runtime();
        let api_server = rt.block_on(mock_api_server::MockAPIServer::start());
        let client_config = tls::build_forge_client_config();

        let result = expire_lease_at(
            "fd00::42",
            None,
            api_server.local_http_addr(),
            &client_config,
        );

        assert_eq!(result, LeaseExpirationResult::Success);
    }

    #[test]
    fn test_expire_lease_with_mac() {
        let rt = CarbideDhcpContext::get_tokio_runtime();
        let api_server = rt.block_on(mock_api_server::MockAPIServer::start());
        let client_config = tls::build_forge_client_config();

        let result = expire_lease_at(
            "10.0.0.1",
            Some("aa:bb:cc:dd:ee:ff"),
            api_server.local_http_addr(),
            &client_config,
        );

        assert_eq!(result, LeaseExpirationResult::Success);
    }
}
