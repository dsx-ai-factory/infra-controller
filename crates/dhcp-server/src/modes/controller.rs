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
use std::net::{IpAddr, Ipv6Addr};
use std::str::FromStr;
use std::time::Duration;

use ::rpc::forge::{DhcpDiscovery, MessageKind};
use carbide_dhcp_common::VendorClass;
use lru::LruCache;
use rpc::forge::DhcpRecord;
use tokio::sync::Mutex;
use tokio::time::timeout;
use tonic::async_trait;

use super::{DhcpMode, V6Outcome, v6_message_kind};
use crate::Config;
use crate::cache::{self, CacheEntry};
use crate::errors::DhcpError;
use crate::rpc::client::discover_dhcp;

#[derive(Debug)]
pub struct Controller {}

const DHCPV6_API_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Bound Controller work by the time in which a DHCPv6 response is still useful.
async fn with_dhcpv6_api_deadline(
    future: impl Future<Output = Result<DhcpRecord, DhcpError>>,
) -> Result<DhcpRecord, DhcpError> {
    timeout(DHCPV6_API_REQUEST_TIMEOUT, future)
        .await
        .map_err(|_| DhcpError::DhcpV6ApiTimeout(DHCPV6_API_REQUEST_TIMEOUT))?
}

#[async_trait]
impl DhcpMode for Controller {
    async fn discover_dhcp(
        &self,
        discovery_request: DhcpDiscovery,
        config: &Config,
        machine_cache: &mut std::sync::Arc<Mutex<LruCache<String, CacheEntry>>>,
    ) -> Result<DhcpRecord, DhcpError> {
        // check if entry present in cache.
        let link_address = IpAddr::from_str(
            discovery_request
                .link_address
                .as_ref()
                .unwrap_or(&discovery_request.relay_address),
        )?;

        let vendor_class = if let Some(vendor_string) = &discovery_request.vendor_string {
            Some(VendorClass::from_str(vendor_string).map_err(|e| {
                DhcpError::VendorClassParseError(format!("Vendor string parse error: {e:?}"))
            })?)
        } else {
            None
        };

        let vendor_id = match &vendor_class {
            Some(vc) => vc.id.as_str(),
            None => "",
        };

        {
            let mut machine_cache = machine_cache.lock().await;
            if let Some(cache_entry) = cache::get(
                &discovery_request.mac_address,
                link_address,
                &discovery_request.circuit_id,
                &discovery_request.remote_id,
                vendor_id,
                &mut machine_cache,
            ) {
                tracing::info!(
                    mac_address = %discovery_request.mac_address,
                    %link_address,
                    circuit_id = ?discovery_request.circuit_id,
                    remote_id = ?discovery_request.remote_id,
                    %vendor_id,
                    "returning cached response"
                );

                return Ok(cache_entry.dhcp_record);
            }
        }

        let record = discover_dhcp(discovery_request.clone(), config).await?;
        let mut machine_cache = machine_cache.lock().await;
        cache::put(
            &discovery_request.mac_address,
            link_address,
            discovery_request.circuit_id,
            discovery_request.remote_id,
            vendor_id,
            record.clone(),
            &mut machine_cache,
        );

        Ok(record)
    }

    /// Resolve DHCPv6 through the authoritative Forge API and v6-local cache.
    async fn discover_dhcp_v6(
        &self,
        discovery_request: DhcpDiscovery,
        config: &Config,
        machine_cache: &mut std::sync::Arc<Mutex<LruCache<String, CacheEntry>>>,
    ) -> Result<V6Outcome, DhcpError> {
        let message_kind = v6_message_kind(&discovery_request)?;

        // Information-only and stateful API responses have different address
        // shapes, so do not let an options-only response poison the stateful cache.
        if message_kind == MessageKind::V6InfoRequest {
            let record = with_dhcpv6_api_deadline(discover_dhcp(discovery_request, config)).await?;
            // Information-only exchanges never gain IA_NA merely because the
            // API also knows a stateful address for this interface.
            return Ok(V6Outcome::OptionsOnly(record));
        }

        let record =
            with_dhcpv6_api_deadline(self.discover_dhcp(discovery_request, config, machine_cache))
                .await?;
        if record.address.is_empty() {
            return Ok(V6Outcome::NoAddress);
        }

        record.address.parse::<Ipv6Addr>()?;
        Ok(V6Outcome::Stateful(record))
    }
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use super::*;

    /// Verifies a stalled Controller dependency cannot retain a DHCPv6 permit indefinitely.
    #[tokio::test(start_paused = true)]
    async fn dhcpv6_api_work_has_a_transaction_deadline() {
        let error = with_dhcpv6_api_deadline(pending())
            .await
            .expect_err("a pending API operation must reach the DHCPv6 deadline");

        assert!(matches!(
            error,
            DhcpError::DhcpV6ApiTimeout(timeout) if timeout == DHCPV6_API_REQUEST_TIMEOUT
        ));
    }
}
