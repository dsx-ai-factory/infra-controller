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
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ::rpc::forge as rpc;
use ::rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig};
use dhcproto::v6::MessageType;
use ipnetwork::IpNetwork;

use crate::discovery::Discovery;
use crate::discovery_v6::{
    RelayContext, V6DecodeError, V6Discovery, decode_with_relay_context, message_kind_for,
};
use crate::machine::Machine;
use crate::metrics::set_service_healthy;
use crate::{CONFIG, CarbideDhcpContext, cache, tls};

/// Result values returned to the C++ DHCPv6 hook callouts.
#[repr(C)]
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum V6HookResult {
    Success = 0,
    Ignore = 1,
    ConfirmNotOnLink = 2,
    InvalidPacket = 3,
    NestedRelay = 4,
    NoMacNoOption79 = 5,
    UnsupportedDuid = 6,
    UnsupportedMessage = 7,
    InvalidMachinePointer = 8,
    FetchMachineError = 9,
    TooManyFailuresError = 10,
}

/// Return a stable C string for a DHCPv6 hook result.
#[unsafe(no_mangle)]
pub extern "C" fn carbide_v6_hook_result_as_str(result: V6HookResult) -> *const libc::c_char {
    // If you add a variant here, keep the string null-terminated for C.
    match result {
        V6HookResult::Success => "success\0",
        V6HookResult::Ignore => "ignore\0",
        V6HookResult::ConfirmNotOnLink => "confirm_not_on_link\0",
        V6HookResult::InvalidPacket => "invalid_packet\0",
        V6HookResult::NestedRelay => "nested_relay\0",
        V6HookResult::NoMacNoOption79 => "no_mac_no_option79\0",
        V6HookResult::UnsupportedDuid => "unsupported_duid\0",
        V6HookResult::UnsupportedMessage => "unsupported_message\0",
        V6HookResult::InvalidMachinePointer => "invalid_machine_pointer\0",
        V6HookResult::FetchMachineError => "fetch_machine_error\0",
        V6HookResult::TooManyFailuresError => "too_many_failures_error\0",
    }
    .as_ptr()
    .cast()
}

/// Build the Carbide DHCP discovery request for a decoded DHCPv6 packet.
fn build_discovery(v6: &V6Discovery) -> rpc::DhcpDiscovery {
    let rapid_commit_enabled = crate::hook_get_config_rapid_commit_v6();
    // An opted-in stateful SOLICIT completes allocation in one exchange, so
    // the API must apply request semantics before Kea persists the lease.
    let message_kind = if rapid_commit_enabled
        && v6.message_type == MessageType::Solicit
        && v6.has_ia_na
        && v6.rapid_commit_requested
    {
        Some(rpc::MessageKind::V6Request)
    } else {
        message_kind_for(v6.message_type, v6.has_ia_na)
    };

    rpc::DhcpDiscovery {
        mac_address: v6.selected_mac.to_string(),
        relay_address: v6
            .relay_link
            .map(|addr| addr.to_string())
            .unwrap_or_default(),
        vendor_string: v6.vendor_class.clone(),
        link_address: v6.relay_link.map(|addr| addr.to_string()),
        circuit_id: v6.interface_id.as_deref().map(hex_encode),
        remote_id: v6.remote_id.as_deref().map(hex_encode),
        desired_address: v6.desired_addr.map(|addr| addr.to_string()),
        address_family: Some(rpc::AddressFamily::V6 as i32),
        message_kind: message_kind.map(|kind| kind as i32),
        duid: Some(v6.duid.clone()),
    }
}

impl Machine {
    /// Fetch a DHCPv6 machine record from Carbide using the decoded v6 discovery.
    async fn try_fetch_v6(
        discovery: V6Discovery,
        carbide_api_url: &str,
        client_config: &ForgeClientConfig,
    ) -> Result<Self, String> {
        let api_config = ApiConfig::new(carbide_api_url, client_config);
        match forge_tls_client::ForgeTlsClient::retry_build(&api_config).await {
            Ok(mut client) => {
                let request = tonic::Request::new(build_discovery(&discovery));
                client
                    .discover_dhcp(request)
                    .await
                    .map(|response| Machine {
                        inner: response.into_inner(),
                        // Machine is shared with the DHCPv4 path and still
                        // stores the legacy Discovery shape for option/cache
                        // helpers; the RPC above carries the v6-only fields.
                        discovery_info: legacy_discovery(&discovery),
                        vendor_class: None,
                    })
                    .map_err(|error| {
                        format!("unable to discover DHCPv6 machine via Carbide: {error:?}")
                    })
            }
            Err(err) => Err(format!("unable to connect to Carbide API: {err:?}")),
        }
    }
}

/// Build the legacy IPv4-shaped discovery snapshot kept on Machine handles.
fn legacy_discovery(discovery: &V6Discovery) -> Discovery {
    Discovery {
        relay_address: Ipv4Addr::UNSPECIFIED,
        mac_address: discovery.selected_mac,
        _client_system: None,
        vendor_class: discovery.vendor_class.clone(),
        link_select_address: None,
        circuit_id: discovery.interface_id.as_deref().map(hex_encode),
        remote_id: discovery.remote_id.as_deref().map(hex_encode),
        desired_address: None,
    }
}

/// Encode opaque relay identifiers as lower-case hexadecimal strings.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Decode a DHCPv6 packet, call Carbide when appropriate, and return a Machine handle.
///
/// # Safety
///
/// All pointer/length pairs must either be null with length 0 or point to valid readable memory
/// for the given length. `machine_ptr_out` must point to writable memory for a `Machine *`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_pkt6_receive(
    packet_ptr: *const u8,
    packet_len: usize,
    relay_count: usize,
    relay_hop_count: u8,
    relay_link_ptr: *const u8,
    relay_link_len: usize,
    interface_id_ptr: *const u8,
    interface_id_len: usize,
    remote_id_ptr: *const u8,
    remote_id_len: usize,
    client_link_layer_ptr: *const u8,
    client_link_layer_len: usize,
    machine_ptr_out: *mut *mut Machine,
) -> V6HookResult {
    // SAFETY: The C ABI contract keeps every non-null input buffer readable for
    // its paired length and the non-null output slot writable for this call.
    // Relay bytes are copied before return, and successful Box ownership moves
    // to Kea through the output slot.
    unsafe {
        if machine_ptr_out.is_null() {
            return V6HookResult::InvalidMachinePointer;
        }
        *machine_ptr_out = std::ptr::null_mut();

        let packet = match slice_from_ffi(packet_ptr, packet_len) {
            Some(packet) => packet,
            None => return V6HookResult::InvalidPacket,
        };
        let relay_context = match relay_context_from_ffi(
            relay_count,
            relay_hop_count,
            relay_link_ptr,
            relay_link_len,
            interface_id_ptr,
            interface_id_len,
            remote_id_ptr,
            remote_id_len,
            client_link_layer_ptr,
            client_link_layer_len,
        ) {
            Some(context) => context,
            None => return V6HookResult::InvalidPacket,
        };
        if relay_context.relay_count == 0 {
            log::warn!("dropping non-relayed DHCPv6 packet");
            return V6HookResult::InvalidPacket;
        }

        let discovery_result = decode_with_relay_context(packet, &relay_context);
        let discovery = match discovery_result {
            Ok(discovery) => discovery,
            Err(err) => {
                log_v6_decode_error(&err, relay_context.link_address);
                return decode_error_to_result(err);
            }
        };

        match discovery.message_type {
            // RELEASE/DECLINE are receive-side validation only. Kea owns the
            // immediate reply, and Carbide/API synchronization happens, if
            // enabled, through lease-end hooks instead of discovery.
            MessageType::Release | MessageType::Decline => V6HookResult::Ignore,
            MessageType::Confirm => fetch_machine_from_cache(&discovery, machine_ptr_out)
                .unwrap_or(V6HookResult::ConfirmNotOnLink),
            _ => fetch_machine(discovery, machine_ptr_out),
        }
    }
}

/// Emit a focused log message for DHCPv6 decode failures.
fn log_v6_decode_error(err: &V6DecodeError, relay_link: Option<Ipv6Addr>) {
    match err {
        V6DecodeError::RelayHopCountExceeded(hop_count) => {
            log::warn!("dropping DHCPv6 relay packet with unsupported hop_count={hop_count}");
        }
        V6DecodeError::NestedRelay => {
            log::warn!("dropping nested DHCPv6 relay packet");
        }
        V6DecodeError::NoMacNoOption79 => match relay_link {
            Some(relay_link) => log::warn!(
                "dropping DHCPv6 packet from relay {relay_link} with non-MAC DUID and no RFC 6939 option 79"
            ),
            None => log::warn!(
                "dropping DHCPv6 packet with non-MAC DUID and no RFC 6939 option 79; relay unknown"
            ),
        },
        V6DecodeError::UnsupportedDuid => {
            log::warn!("dropping DHCPv6 packet with malformed or unsupported DUID");
        }
        other => {
            log::debug!("dropping unsupported DHCPv6 packet: {other:?}");
        }
    }
}

/// Translate a decode error into the FFI result understood by C++ callouts.
fn decode_error_to_result(err: V6DecodeError) -> V6HookResult {
    match err {
        V6DecodeError::NestedRelay | V6DecodeError::RelayHopCountExceeded(_) => {
            V6HookResult::NestedRelay
        }
        V6DecodeError::NoMacNoOption79 => V6HookResult::NoMacNoOption79,
        V6DecodeError::UnsupportedDuid | V6DecodeError::MissingDuid => {
            V6HookResult::UnsupportedDuid
        }
        V6DecodeError::UnsupportedMessage(_) => V6HookResult::UnsupportedMessage,
        V6DecodeError::MalformedPacket => V6HookResult::InvalidPacket,
    }
}

/// Return a cached DHCPv6 Machine for CONFIRM when the segment still matches.
fn fetch_machine_from_cache(
    discovery: &V6Discovery,
    machine_ptr_out: *mut *mut Machine,
) -> Option<V6HookResult> {
    let link_address = IpAddr::V6(discovery.relay_link.unwrap_or(Ipv6Addr::UNSPECIFIED));
    let circuit_id = discovery.interface_id.as_deref().map(hex_encode);
    let remote_id = discovery.remote_id.as_deref().map(hex_encode);
    let vendor_id = discovery.vendor_class.as_deref().unwrap_or("");

    // Prefer the exact cache key when CONFIRM repeats the original vendor
    // class from SOLICIT/REQUEST.
    match cache::get_classed(
        rpc::AddressFamily::V6,
        cache::CacheClass::Lease,
        discovery.selected_mac,
        link_address,
        &circuit_id,
        &remote_id,
        vendor_id,
    ) {
        Some(cache::CacheEntry {
            status: cache::CacheEntryStatus::ValidEntry(machine),
            ..
        }) => {
            if confirm_addresses_on_link(discovery, &machine) {
                // SAFETY: Initial lint enablement: this call-graph invariant
                // needs owner review. The validated FFI entrypoint supplies the
                // still-live writable output slot used by this private helper.
                unsafe {
                    *machine_ptr_out = Box::into_raw(machine);
                }
                Some(V6HookResult::Success)
            } else {
                log::warn!(
                    "DHCPv6 CONFIRM for {:?} is not on cached link prefix {}",
                    discovery.ia_addrs,
                    machine.inner.prefix
                );
                None
            }
        }
        // CONFIRM often omits vendor class. When the exact key misses, search
        // otherwise-identical lease entries across vendor variants and accept
        // only if they resolve to the same on-link API lease context.
        _ => fetch_machine_from_cache_any_vendor(
            discovery,
            machine_ptr_out,
            link_address,
            &circuit_id,
            &remote_id,
        ),
    }
}

fn fetch_machine_from_cache_any_vendor(
    discovery: &V6Discovery,
    machine_ptr_out: *mut *mut Machine,
    link_address: IpAddr,
    circuit_id: &Option<String>,
    remote_id: &Option<String>,
) -> Option<V6HookResult> {
    let matches = cache::get_classed_any_vendor(
        rpc::AddressFamily::V6,
        cache::CacheClass::Lease,
        discovery.selected_mac,
        link_address,
        circuit_id,
        remote_id,
    );
    let mut candidate: Option<Box<Machine>> = None;
    for entry in matches {
        let cache::CacheEntryStatus::ValidEntry(machine) = entry.status else {
            continue;
        };
        if !confirm_addresses_on_link(discovery, &machine) {
            log::warn!("DHCPv6 CONFIRM matched a vendor-class cache entry that is not on-link");
            return None;
        }

        // Vendor class is optional on CONFIRM. Multiple cache variants are
        // equivalent only when they still point at the same API lease context.
        if let Some(existing) = &candidate {
            if machine.inner.address != existing.inner.address
                || machine.inner.prefix != existing.inner.prefix
                || machine.inner.machine_interface_id != existing.inner.machine_interface_id
                || machine.inner.segment_id != existing.inner.segment_id
            {
                log::warn!(
                    "DHCPv6 CONFIRM matched conflicting vendor-class cache entries; failing closed"
                );
                return None;
            }
        } else {
            candidate = Some(machine);
        }
    }

    let machine = candidate?;
    // SAFETY: Initial lint enablement: this call-graph invariant needs owner
    // review. The validated FFI entrypoint supplies the still-live writable
    // output slot used by this private helper.
    unsafe {
        *machine_ptr_out = Box::into_raw(machine);
    }
    Some(V6HookResult::Success)
}

/// Fetch and cache a DHCPv6 Machine from Carbide for request-like messages.
fn fetch_machine(discovery: V6Discovery, machine_ptr_out: *mut *mut Machine) -> V6HookResult {
    let url = &CONFIG.read().unwrap().api_endpoint;
    let link_address = IpAddr::V6(discovery.relay_link.unwrap_or(Ipv6Addr::UNSPECIFIED));
    let cache_class = cache_class_for(&discovery);
    let circuit_id = discovery.interface_id.as_deref().map(hex_encode);
    let remote_id = discovery.remote_id.as_deref().map(hex_encode);
    let vendor_id = discovery.vendor_class.as_deref().unwrap_or("").to_string();
    let selected_mac = discovery.selected_mac;
    let mut cache_entry_status = cache::CacheEntryStatus::DiscoveryFailing(0);

    if let Some(cache_entry) = cache::get_classed(
        rpc::AddressFamily::V6,
        cache_class,
        selected_mac,
        link_address,
        &circuit_id,
        &remote_id,
        &vendor_id,
    ) {
        match cache_entry.status {
            cache::CacheEntryStatus::ValidEntry(machine) => {
                log::info!(
                    "returning cached DHCPv6 response for ({selected_mac}, {link_address}, {circuit_id:?}, {vendor_id})."
                );
                // SAFETY: Initial lint enablement: this call-graph invariant
                // needs owner review. The validated FFI entrypoint supplies the
                // still-live writable output slot used by this private helper.
                unsafe {
                    *machine_ptr_out = Box::into_raw(machine);
                }
                return V6HookResult::Success;
            }
            cache::CacheEntryStatus::DiscoveryFailing(count) => {
                log::info!(
                    "retrying carbide-api DHCPv6 for ({selected_mac}, {link_address}, {circuit_id:?}, {vendor_id}). failure count: {count}."
                );
                cache_entry_status = cache_entry.status;
            }
            cache::CacheEntryStatus::DiscoveryFailed => {
                log::info!(
                    "too many DHCPv6 failures for ({selected_mac}, {link_address}, {circuit_id:?}, {vendor_id})."
                );
                return V6HookResult::TooManyFailuresError;
            }
        }
    }

    // Tonic is async, but Kea calls us synchronously from its hook thread.
    let runtime = CarbideDhcpContext::get_tokio_runtime();
    let forge_client_config = tls::build_forge_client_config();
    match runtime.block_on(Machine::try_fetch_v6(discovery, url, &forge_client_config)) {
        Ok(machine) => {
            // Expiry tombstones protect against stale hook-cache reuse. A fresh
            // API response is authoritative, even if the allocator chooses the
            // same address again for the same MAC.
            if let Ok(IpAddr::V6(address)) = machine.inner.address.parse::<IpAddr>()
                && cache::clear_v6_lease_invalidation(address, selected_mac)
            {
                log::info!(
                    "accepting fresh DHCPv6 API response for recently expired lease: mac={} address={}",
                    selected_mac,
                    machine.inner.address
                );
            }
            handle_api_invalidation(&machine);
            cache::put_classed(
                cache::CacheScope {
                    address_family: rpc::AddressFamily::V6,
                    cache_class,
                },
                selected_mac,
                link_address,
                circuit_id,
                remote_id,
                &vendor_id,
                cache::CacheEntryStatus::ValidEntry(Box::new(machine.clone())),
            );
            // SAFETY: Initial lint enablement: this call-graph invariant needs
            // owner review. The validated FFI entrypoint supplies the still-live
            // writable output slot used by this private helper.
            unsafe {
                *machine_ptr_out = Box::into_raw(Box::new(machine));
            }
            V6HookResult::Success
        }
        Err(error) => {
            log::error!(
                "Error getting DHCPv6 info from machine discovery: mac={selected_mac} addr={link_address} err={error} api_url={url}"
            );
            cache::put_classed(
                cache::CacheScope {
                    address_family: rpc::AddressFamily::V6,
                    cache_class,
                },
                selected_mac,
                link_address,
                circuit_id,
                remote_id,
                &vendor_id,
                cache_entry_status.increment_fails(),
            );
            V6HookResult::FetchMachineError
        }
    }
}

/// Restart Kea when the API reports a DHCP record invalidated after startup.
fn handle_api_invalidation(machine: &Machine) {
    // Match the v4 path: if API says Kea's cache may be stale, mark the
    // service unhealthy and ask Kea to restart gracefully.
    let Some(last_invalidation) = machine.inner.last_invalidation_time.as_ref() else {
        return;
    };
    let startup_time = CONFIG.read().unwrap().startup_time;
    if let Ok(last_invalidation) = chrono::DateTime::<chrono::Utc>::try_from(*last_invalidation)
        && last_invalidation >= startup_time
    {
        log::error!(
            "Restarting KEA since invalidation was reported by Carbide. Startup: {}. Last_Invalidation: {}",
            startup_time.to_rfc3339(),
            last_invalidation.to_rfc3339()
        );
        set_service_healthy(false);
        // SAFETY: `getpid` returns the current process and SIGTERM is a valid
        // signal value; no borrowed memory crosses this syscall boundary.
        unsafe {
            libc::kill(libc::getpid(), libc::SIGTERM);
        }
    }
}

fn cache_class_for(discovery: &V6Discovery) -> cache::CacheClass {
    match message_kind_for(discovery.message_type, discovery.has_ia_na) {
        Some(rpc::MessageKind::V6InfoRequest) => cache::CacheClass::OptionsOnly,
        _ => cache::CacheClass::Lease,
    }
}

fn confirm_addresses_on_link(discovery: &V6Discovery, machine: &Machine) -> bool {
    if discovery.ia_addrs.is_empty() {
        return false;
    }

    match machine.inner.prefix.parse::<IpNetwork>() {
        Ok(IpNetwork::V6(prefix)) => discovery.ia_addrs.iter().all(|addr| prefix.contains(*addr)),
        Ok(IpNetwork::V4(prefix)) => {
            log::warn!("DHCPv6 CONFIRM cache entry has IPv4 prefix {prefix}");
            false
        }
        Err(error) => {
            log::warn!(
                "DHCPv6 CONFIRM cache entry has invalid prefix {}: {error}",
                machine.inner.prefix
            );
            false
        }
    }
}

/// Borrow a byte slice from a C pointer/length pair.
unsafe fn slice_from_ffi<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if ptr.is_null() {
        (len == 0).then_some(&[])
    } else {
        // SAFETY: The unsafe caller guarantees this non-null pointer is readable
        // for `len` initialized bytes in one allocation for the returned borrow.
        Some(unsafe { std::slice::from_raw_parts(ptr, len) })
    }
}

/// Build RelayContext from Kea-owned C pointer/length fields.
#[allow(clippy::too_many_arguments)]
unsafe fn relay_context_from_ffi(
    relay_count: usize,
    hop_count: u8,
    relay_link_ptr: *const u8,
    relay_link_len: usize,
    interface_id_ptr: *const u8,
    interface_id_len: usize,
    remote_id_ptr: *const u8,
    remote_id_len: usize,
    client_link_layer_ptr: *const u8,
    client_link_layer_len: usize,
) -> Option<RelayContext> {
    // SAFETY: The outer FFI contract covers this pointer/length pair, and the
    // bytes are copied into the returned context before the C++ buffer expires.
    let relay_link = match unsafe { slice_from_ffi(relay_link_ptr, relay_link_len) }? {
        [] => None,
        bytes if bytes.len() == 16 => Some(Ipv6Addr::from(<[u8; 16]>::try_from(bytes).ok()?)),
        _ => return None,
    };

    Some(RelayContext {
        relay_count,
        hop_count,
        link_address: relay_link,
        interface_id: non_empty_vec(
            // SAFETY: The outer FFI contract keeps this option buffer readable
            // until it is copied into the returned context.
            unsafe { slice_from_ffi(interface_id_ptr, interface_id_len) }?,
        ),
        remote_id: non_empty_vec(
            // SAFETY: The outer FFI contract keeps this option buffer readable
            // until it is copied into the returned context.
            unsafe { slice_from_ffi(remote_id_ptr, remote_id_len) }?,
        ),
        client_link_layer: non_empty_vec(
            // SAFETY: The outer FFI contract keeps this option buffer readable
            // until it is copied into the returned context.
            unsafe { slice_from_ffi(client_link_layer_ptr, client_link_layer_len) }?,
        ),
    })
}

/// Copy non-empty option payload bytes into an owned Vec.
fn non_empty_vec(bytes: &[u8]) -> Option<Vec<u8>> {
    (!bytes.is_empty()).then(|| bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use dhcproto::v6::MessageType;

    use super::*;
    use crate::discovery_v6::V6Discovery;

    fn discovery(message_type: MessageType, has_ia_na: bool) -> V6Discovery {
        V6Discovery {
            selected_mac: "02:00:00:00:00:01".parse().unwrap(),
            duid_mac: Some("02:00:00:00:00:01".parse().unwrap()),
            duid: vec![0, 3, 0, 1, 2, 0, 0, 0, 0, 1],
            message_type,
            relay_link: Some("2001:db8::1".parse().unwrap()),
            vendor_class: Some("HTTPClient::7::".to_string()),
            interface_id: Some(b"eth0".to_vec()),
            remote_id: Some(b"rack-a".to_vec()),
            desired_addr: Some("2001:db8::42".parse().unwrap()),
            ia_addrs: vec!["2001:db8::42".parse().unwrap()],
            has_ia_na,
            rapid_commit_requested: false,
            client_link_layer: None,
        }
    }

    #[test]
    fn builds_stateful_v6_discovery_request() {
        // The hook owns the v6 transport fields before calling the API.
        let request = build_discovery(&discovery(MessageType::Solicit, true));

        assert_eq!(request.mac_address, "02:00:00:00:00:01");
        assert_eq!(request.relay_address, "2001:db8::1");
        assert_eq!(request.link_address.as_deref(), Some("2001:db8::1"));
        assert_eq!(request.circuit_id.as_deref(), Some("65746830"));
        assert_eq!(request.remote_id.as_deref(), Some("7261636b2d61"));
        assert_eq!(request.desired_address.as_deref(), Some("2001:db8::42"));
        assert_eq!(request.address_family, Some(rpc::AddressFamily::V6 as i32));
        assert_eq!(
            request.message_kind,
            Some(rpc::MessageKind::V6Solicit as i32)
        );
        assert_eq!(request.duid, Some(vec![0, 3, 0, 1, 2, 0, 0, 0, 0, 1]));
    }

    #[test]
    fn maps_request_like_messages_to_existing_v6_request_kind() {
        // The current proto intentionally collapses REQUEST/RENEW/REBIND.
        for message_type in [
            MessageType::Request,
            MessageType::Renew,
            MessageType::Rebind,
        ] {
            let request = build_discovery(&discovery(message_type, true));

            assert_eq!(
                request.message_kind,
                Some(rpc::MessageKind::V6Request as i32)
            );
        }
    }
}
