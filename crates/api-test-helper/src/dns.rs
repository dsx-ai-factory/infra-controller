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

//! gRPC helpers for DNS lookup and DHCP publication.

use std::net::{Ipv4Addr, SocketAddr};

use rpc::dns::{DnsResourceRecordLookupRequest, DnsResourceRecordLookupResponse};
use rpc::forge::{DhcpDiscovery, DhcpRecord};

use crate::api_client;

/// Resolves `qname` the way the DNS server's remote backend does.
///
/// `qtype` is forwarded unchanged; the API accepts only uppercase `SOA`, `NS`,
/// `A`, `AAAA`, `CNAME`, `MX`, `TXT`, and `PTR` values and rejects other values.
/// One API address is selected from `carbide_api_addrs`; an empty list returns
/// a `no API servers configured` error. Connection and lookup share a 60-second
/// deadline; connection errors, gRPC failures, and expiry are returned.
pub async fn lookup(
    carbide_api_addrs: &[SocketAddr],
    qname: &str,
    qtype: &str,
) -> eyre::Result<DnsResourceRecordLookupResponse> {
    let request = DnsResourceRecordLookupRequest {
        qname: qname.to_string(),
        qtype: qtype.to_string(),
        // The query name drives lookup; the legacy zone ID is unused.
        zone_id: "-1".to_string(),
        local: None,
        remote: None,
        real_remote: None,
    };
    api_client::call(carbide_api_addrs, "LookupRecord", |mut client| async move {
        client.lookup_record(request).await
    })
    .await
}

/// Discovers a DHCP address and returns its address and forward FQDN.
///
/// `mac_address` is forwarded unchanged and must use syntax accepted by
/// `mac_address::MacAddress`; invalid input returns an error from the API.
/// The relay address must select a configured segment. One API address is
/// selected from `carbide_api_addrs`; an empty list returns a
/// `no API servers configured` error.
/// Connection and discovery share a 60-second deadline, and any failure is
/// returned to the caller.
pub async fn discover_dhcp(
    carbide_api_addrs: &[SocketAddr],
    mac_address: &str,
    relay_address: Ipv4Addr,
) -> eyre::Result<DhcpRecord> {
    let request = DhcpDiscovery {
        mac_address: mac_address.to_string(),
        relay_address: relay_address.to_string(),
        ..Default::default()
    };
    api_client::call(carbide_api_addrs, "DiscoverDhcp", |mut client| async move {
        client.discover_dhcp(request).await
    })
    .await
}
