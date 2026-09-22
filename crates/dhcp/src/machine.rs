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
use std::ffi::CString;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::ptr;

use ::rpc::forge as rpc;
use ::rpc::forge_tls_client::{self, ApiConfig, ForgeClientConfig};
use carbide_dhcp_common::MachineArchitecture::*;
use carbide_dhcp_common::VendorClass;
use ipnetwork::IpNetwork;

use crate::discovery::Discovery;
use crate::{CONFIG, cache};

/// Rust-owned byte buffer returned across the C ABI for DHCP option payloads.
#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct DhcpByteBuffer {
    pub ptr: *mut u8,
    pub len: usize,
}

impl DhcpByteBuffer {
    /// Return an empty buffer for omitted DHCPv6 options.
    fn empty() -> Self {
        Self {
            ptr: ptr::null_mut(),
            len: 0,
        }
    }

    /// Move option payload bytes into an FFI-safe buffer owned by Rust.
    fn from_vec(bytes: Vec<u8>) -> Self {
        if bytes.is_empty() {
            return Self::empty();
        }

        let mut bytes = bytes.into_boxed_slice();
        let ptr = bytes.as_mut_ptr();
        let len = bytes.len();
        std::mem::forget(bytes);
        Self { ptr, len }
    }
}

/// Machine: a machine that's currently trying to boot something
///
/// This just stores the protobuf DHCP record and the discovery info the client used so we can add
/// additional constraints (options) to and from the client.
#[derive(Debug, Clone)]
pub struct Machine {
    pub(super) inner: rpc::DhcpRecord,
    pub(super) discovery_info: Discovery,
    pub(super) vendor_class: Option<VendorClass>,
}

impl Machine {
    pub(super) async fn try_fetch(
        discovery: Discovery,
        carbide_api_url: &str,
        vendor_class: Option<VendorClass>,
        client_config: &ForgeClientConfig,
    ) -> Result<Self, String> {
        let api_config = ApiConfig::new(carbide_api_url, client_config);
        match forge_tls_client::ForgeTlsClient::retry_build(&api_config).await {
            Ok(mut client) => {
                let request = tonic::Request::new(rpc::DhcpDiscovery {
                    mac_address: discovery.mac_address.to_string(),
                    relay_address: discovery.relay_address.to_string(),
                    link_address: discovery.link_select_address.map(|addr| addr.to_string()),
                    vendor_string: discovery.vendor_class.clone(),
                    circuit_id: discovery.circuit_id.clone(),
                    remote_id: discovery.remote_id.clone(),
                    desired_address: discovery.desired_address.map(|addr| addr.to_string()),
                    address_family: None,
                    message_kind: None,
                    duid: None,
                });

                client
                    .discover_dhcp(request)
                    .await
                    .map(|response| Machine {
                        inner: response.into_inner(),
                        discovery_info: discovery,
                        vendor_class,
                    })
                    .map_err(|error| format!("unable to discover machine via Carbide: {error:?}"))
            }
            Err(err) => Err(format!("unable to connect to Carbide API: {err:?}")),
        }
    }

    fn booturl(&self) -> Option<&str> {
        self.inner.booturl.as_deref()
    }
}

/// Get the router address.
/// This is, and will always be, specific to DHCPv4, as router addresses
/// in DHCPv6 will come from RAs (Router Advertisements).
///
/// # Safety
///
/// This function dereferences a pointer to a Machine object which is an opaque pointer
/// consumed in C code.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_interface_router(ctx: *mut Machine) -> u32 {
    assert!(!ctx.is_null());
    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };

    // todo(ajf): I guess??
    let default_router = "0.0.0.0".to_string();

    let maybe_gateway = machine
        .inner
        .gateway
        .as_ref()
        .unwrap_or_else(|| {
            log::warn!(
                "No gateway provided for machine interface: {:?}",
                machine.inner.machine_interface_id
            );
            &default_router
        })
        .parse::<IpAddr>();

    match maybe_gateway {
        Ok(gateway) => match gateway {
            IpAddr::V4(gateway) => return u32::from_be_bytes(gateway.octets()),
            IpAddr::V6(gateway) => {
                log::error!("Gateway ({gateway}) is an IPv6 address, which is not supported.");
            }
        },
        Err(error) => {
            log::error!("Gateway value in deserialized protobuf is not an IP Network: {error}");
        }
    };

    0
}

/// Invoke the discovery process.
/// This function will be specific to IPv4 interface addresses, as this
/// returns a u32. DHCPv6 integration will need a separate function for
/// stateful/managed allocations.
///
/// # Safety
/// This function dereferences a pointer to a Machine object which is an opaque pointer
/// consumed in C code.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_interface_address(ctx: *mut Machine) -> u32 {
    assert!(!ctx.is_null());
    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };

    let maybe_address = machine.inner.address.parse::<IpAddr>();

    match maybe_address {
        Ok(address) => match address {
            IpAddr::V4(address) => return u32::from_be_bytes(address.octets()),
            IpAddr::V6(address) => {
                log::error!("Address ({address}) is an IPv6 address, which is not supported.");
            }
        },
        Err(error) => {
            log::error!("Address value in deserialized protobuf is not an IP Network: {error}");
        }
    };

    0
}

/// Write the machine's assigned IPv6 address into a caller-provided 16-byte buffer.
///
/// Returns false when Carbide did not return a parseable IPv6 address.
///
/// # Safety
/// This function dereferences a pointer to a Machine object and writes to `addr_out`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn machine_get_interface_address_ipv6(
    ctx: *mut Machine,
    addr_out: *mut u8,
    addr_out_len: usize,
) -> bool {
    if ctx.is_null() || addr_out.is_null() || addr_out_len != 16 {
        return false;
    }

    // SAFETY: The null checks above and the caller's C ABI contract guarantee a
    // live Rust-allocated Machine for this synchronous read. The output buffer
    // is disjoint from the Machine allocation.
    let machine = unsafe { &*ctx };
    match machine.inner.address.parse::<IpAddr>() {
        Ok(IpAddr::V6(address)) => {
            // SAFETY: `addr_out` is caller-provided writable storage for exactly
            // 16 bytes and cannot overlap the local address octets.
            unsafe {
                std::ptr::copy_nonoverlapping(address.octets().as_ptr(), addr_out, 16);
            }
            true
        }
        Ok(IpAddr::V4(address)) => {
            log::error!("Address ({address}) is an IPv4 address, which is not supported here.");
            false
        }
        Err(error) => {
            log::error!("Address value in deserialized protobuf is not an IP address: {error}");
            false
        }
    }
}

/// Get the machine fqdn
///
/// # Safety
/// This function checks for null pointer and unboxes into a machine object
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_interface_hostname(ctx: *mut Machine) -> *mut libc::c_char {
    assert!(!ctx.is_null());
    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };

    let fqdn = CString::new(&machine.inner.fqdn[..]).unwrap();

    fqdn.into_raw()
}

/// Get the machine fqdn
///
/// # Safety
/// This function checks for null pointer and unboxes into a machine object
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_filename(ctx: *mut Machine) -> *const libc::c_char {
    assert!(!ctx.is_null());
    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };

    // If the API sent us the URL we should boot from, just use it.
    let url = if let Some(url) = machine.booturl() {
        url.to_string()
    } else {
        let arch = match &machine.vendor_class {
            None => {
                return ptr::null();
            }
            Some(v) if !v.is_netboot() => {
                return ptr::null();
            }
            Some(VendorClass { arch, .. }) => arch,
        };

        let base_url = if let Some(next_server) = CONFIG
            .read()
            .unwrap() // TODO(ajf): don't unwrap
            .provisioning_server_ipv4
        {
            next_server.to_string()
        } else {
            log::warn!("Could not retrieve provisioning-server-ipv4 configuration from Kea");
            return ptr::null();
        };

        match arch {
            EfiX64 => format!("http://{base_url}:8080/public/blobs/internal/x86_64/ipxe.efi"),
            Arm64 => format!("http://{base_url}:8080/public/blobs/internal/aarch64/ipxe.efi"),
            BiosX86 => {
                log::error!(
                    "Matched an HTTP client on a Legacy BIOS client, cannot provide HTTP boot URL {:?}",
                    machine
                );
                return ptr::null();
            }
            Unknown => {
                log::error!(
                    "Matched an unknown architecture, cannot provide HTTP boot URL {:?}",
                    machine
                );
                return ptr::null();
            }
        }
    };

    CString::new(url).unwrap().into_raw()
}

// IPv4 address of next-server (siaddr) as big endian int 32.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_next_server(ctx: *mut Machine) -> u32 {
    assert!(!ctx.is_null());
    let ip_addr = if let Some(next_server) = CONFIG
        .read()
        .unwrap() // TODO(ajf): don't unwrap
        .provisioning_server_ipv4
    {
        next_server.octets()
    } else {
        "127.0.0.1"
            .to_string()
            .parse::<Ipv4Addr>()
            .unwrap()
            .octets()
    };

    u32::from_be_bytes(ip_addr)
}

#[unsafe(no_mangle)]
pub extern "C" fn machine_get_nameservers(ctx: *mut Machine) -> *mut libc::c_char {
    assert!(!ctx.is_null());

    let nameservers =
        CString::new(crate::format_ipv4_list(&CONFIG.read().unwrap().nameservers)).unwrap();
    log::debug!("Nameservers are {nameservers:?}");

    nameservers.into_raw()
}

#[unsafe(no_mangle)]
pub extern "C" fn machine_get_ntpservers(ctx: *mut Machine) -> *mut libc::c_char {
    assert!(!ctx.is_null());

    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };

    let ntp_csv = if !machine.inner.ntp_servers.is_empty() {
        machine.inner.ntp_servers.join(",")
    } else {
        crate::format_ipv4_list(&CONFIG.read().unwrap().ntpservers)
    };

    let ntpservers = CString::new(ntp_csv).unwrap();
    log::debug!("Ntp servers are {ntpservers:?}");

    ntpservers.into_raw()
}

/// Return DHCPv6 option 23 payload bytes for configured DNS servers.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_dns_servers_ipv6(ctx: *mut Machine) -> DhcpByteBuffer {
    assert!(!ctx.is_null());

    let servers = CONFIG.read().unwrap().dns_servers_ipv6.clone();
    DhcpByteBuffer::from_vec(flatten_ipv6_addresses(&servers))
}

/// Return DHCPv6 option 56 payload bytes for NTP server suboptions.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_ntp_servers_ipv6(ctx: *mut Machine) -> DhcpByteBuffer {
    assert!(!ctx.is_null());

    // DHCPv6 option 56 is hook-context sourced; DhcpRecord NTP stays v4-only.
    let servers = CONFIG.read().unwrap().ntp_servers_ipv6.clone();
    DhcpByteBuffer::from_vec(ntp_server_option_payload(&servers))
}

/// Return DHCPv6 option 24 payload bytes derived from the machine FQDN.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_domain_search_ipv6(ctx: *mut Machine) -> DhcpByteBuffer {
    assert!(!ctx.is_null());

    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };
    match parent_domain(&machine.inner.fqdn).and_then(encode_domain_name) {
        Some(bytes) => DhcpByteBuffer::from_vec(bytes),
        None => DhcpByteBuffer::empty(),
    }
}

/// Return DHCPv6 option 39 payload bytes derived from the machine FQDN.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_client_fqdn_ipv6(ctx: *mut Machine) -> DhcpByteBuffer {
    assert!(!ctx.is_null());

    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };
    match encode_domain_name(&machine.inner.fqdn) {
        Some(mut bytes) => {
            // RFC 4704 option 39 starts with a flags byte; C++ replaces this
            // placeholder with the client request's negotiation flags.
            bytes.insert(0, 0);
            DhcpByteBuffer::from_vec(bytes)
        }
        None => DhcpByteBuffer::empty(),
    }
}

/// Write the configured DHCPv6 provisioning-server address into a caller-provided 16-byte buffer.
///
/// Returns false when the hook parameter is unset.
///
/// # Safety
/// This function writes to `addr_out` when a provisioning-server address is configured.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn machine_get_provisioning_server_ipv6(
    addr_out: *mut u8,
    addr_out_len: usize,
) -> bool {
    if addr_out.is_null() || addr_out_len != 16 {
        return false;
    }

    let Some(provisioning_server) = CONFIG.read().unwrap().provisioning_server_ipv6 else {
        return false;
    };

    // SAFETY: The caller guarantees that `addr_out` identifies writable storage
    // for the validated 16-byte length and cannot overlap the local octets.
    unsafe {
        std::ptr::copy_nonoverlapping(provisioning_server.octets().as_ptr(), addr_out, 16);
    }
    true
}

#[unsafe(no_mangle)]
pub extern "C" fn machine_get_mqtt_server(ctx: *mut Machine) -> *mut libc::c_char {
    assert!(!ctx.is_null());

    match CONFIG.read().unwrap().mqtt_server.clone() {
        Some(mqtt_server) => {
            log::debug!("MQTT server is {mqtt_server:?}");
            CString::new(mqtt_server).unwrap().into_raw()
        }
        None => {
            log::debug!("MQTT server is unset");
            ptr::null_mut()
        }
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn machine_get_client_type(ctx: *mut Machine) -> *mut libc::c_char {
    assert!(!ctx.is_null());
    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };
    let vendor_class = match &machine.vendor_class {
        None => CString::new("").unwrap(),
        Some(vc) => CString::new(vc.id.clone()).unwrap(),
    };
    vendor_class.into_raw()
}

/// Return the hook-selected discovery MAC associated with this Machine.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_discovery_mac(ctx: *mut Machine) -> *mut libc::c_char {
    if ctx.is_null() {
        return ptr::null_mut();
    }

    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };
    CString::new(machine.discovery_info.mac_address.to_string())
        .unwrap()
        .into_raw()
}

/// Return whether this Machine points at a recently expired DHCPv6 lease.
#[unsafe(no_mangle)]
pub extern "C" fn machine_is_invalidated_v6_lease(ctx: *mut Machine) -> bool {
    if ctx.is_null() {
        return false;
    }

    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };
    cache::machine_matches_invalidated_v6_lease(machine)
}

/// Get the broadcast address.
///
/// This is, and will always be, specific to DHCPv4, as broadcast
/// in DHCPv6 has been completely replaced by multicast.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_broadcast_address(ctx: *mut Machine) -> u32 {
    assert!(!ctx.is_null());
    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };

    let maybe_prefix = machine.inner.prefix.parse::<IpNetwork>();

    match maybe_prefix {
        Ok(prefix) => match prefix {
            IpNetwork::V4(prefix) => return u32::from_be_bytes(prefix.broadcast().octets()),
            IpNetwork::V6(prefix) => {
                log::error!("Prefix ({prefix}) is an IPv6 network, which is not supported.");
            }
        },
        Err(error) => {
            log::error!("prefix value in deserialized protobuf is not an IP Network: {error}");
        }
    };

    0
}

#[unsafe(no_mangle)]
pub extern "C" fn machine_free_filename(filename: *const libc::c_char) {
    // SAFETY: Initial lint enablement: this ownership contract needs owner
    // review. A non-null pointer is the unreclaimed `CString::into_raw` result
    // from `machine_get_filename`, returned to the same Rust allocator once.
    unsafe {
        if filename.is_null() {
            return;
        }

        drop(CString::from_raw(filename as *mut _))
    };
}

#[unsafe(no_mangle)]
pub extern "C" fn machine_free_client_type(client_type: *mut libc::c_char) {
    // SAFETY: Initial lint enablement: this ownership contract needs owner
    // review. A non-null pointer is the unreclaimed `CString::into_raw` result
    // from `machine_get_client_type`, returned to the same Rust allocator once.
    unsafe {
        if client_type.is_null() {
            return;
        }

        drop(CString::from_raw(client_type))
    };
}

/// Free a discovery MAC string returned by [`machine_get_discovery_mac`].
#[unsafe(no_mangle)]
pub extern "C" fn machine_free_discovery_mac(mac_address: *mut libc::c_char) {
    // SAFETY: Initial lint enablement: this ownership contract needs owner
    // review. A non-null pointer is the unreclaimed `CString::into_raw` result
    // from `machine_get_discovery_mac`, returned to the Rust allocator once.
    unsafe {
        if mac_address.is_null() {
            return;
        }

        drop(CString::from_raw(mac_address))
    };
}

#[unsafe(no_mangle)]
pub extern "C" fn machine_free_fqdn(fqdn: *mut libc::c_char) {
    // SAFETY: Initial lint enablement: this ownership contract needs owner
    // review. A non-null pointer is the unreclaimed `CString::into_raw` result
    // from `machine_get_interface_hostname`, returned to the allocator once.
    unsafe {
        if fqdn.is_null() {
            return;
        }

        drop(CString::from_raw(fqdn))
    };
}

#[unsafe(no_mangle)]
pub extern "C" fn machine_free_nameservers(nameservers: *mut libc::c_char) {
    // SAFETY: Initial lint enablement: this ownership contract needs owner
    // review. A non-null pointer is an unreclaimed `CString::into_raw` result
    // from a matching string getter, returned to the Rust allocator once.
    unsafe {
        if nameservers.is_null() {
            return;
        }

        drop(CString::from_raw(nameservers))
    };
}

/// Free a byte buffer returned by a DHCPv6 option getter.
///
/// # Safety
/// The buffer must have been returned by this crate and not freed before.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn machine_free_dhcp_byte_buffer(buffer: DhcpByteBuffer) {
    if buffer.ptr.is_null() || buffer.len == 0 {
        return;
    }

    // SAFETY: The caller returns the unchanged pointer and length produced from
    // one nonempty boxed slice, exactly once and after its last C++ borrow.
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(
            buffer.ptr, buffer.len,
        )));
    }
}

/// Invoke the discovery process
/// This is, and will always be, specific to DHCPv4, as the subnet
/// mask in DHCPv6 is now learned via RAs as a prefix.
///
/// # Safety
///
/// This function dereferences a pointer to a Machine object which is an opaque pointer
/// consumed in C code.
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_interface_subnet_mask(ctx: *mut Machine) -> u32 {
    assert!(!ctx.is_null());
    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea retains this Rust-allocated Machine for the synchronous getter call.
    let machine = unsafe { &*ctx };

    let maybe_prefix = machine.inner.prefix.parse::<IpNetwork>();

    match maybe_prefix {
        Ok(prefix) => match prefix {
            IpNetwork::V4(prefix) => return u32::from_be_bytes(prefix.mask().octets()),
            IpNetwork::V6(prefix) => {
                log::error!("Prefix ({prefix}) is an IPv6 network, which is not supported.");
            }
        },
        Err(error) => {
            log::error!("prefix value in deserialized protobuf is not an IP Network: {error}");
        }
    };

    0
}

/// Extract MTU from Machine object. We got it in the grpc response in discovery_fetch_machine.
/// https://github.com/NVIDIA/infra-controller/issues/5559
#[unsafe(no_mangle)]
pub extern "C" fn machine_get_interface_mtu(ctx: *mut Machine) -> u16 {
    // SAFETY: Initial lint enablement: this C ABI contract needs owner review.
    // Kea passes a non-null, live Rust-allocated Machine for this synchronous
    // getter, but that requirement is not encoded in the safe Rust signature.
    unsafe { (*ctx).inner.mtu as u16 }
}

/// Encode a DHCPv6 flat list of IPv6 addresses.
fn flatten_ipv6_addresses(addresses: &[Ipv6Addr]) -> Vec<u8> {
    addresses
        .iter()
        .flat_map(|address| address.octets())
        .collect()
}

/// Encode DHCPv6 option 56 NTP server suboptions.
fn ntp_server_option_payload(addresses: &[Ipv6Addr]) -> Vec<u8> {
    addresses
        .iter()
        .flat_map(|address| {
            let mut payload = Vec::with_capacity(20);
            // RFC 5908 separates unicast server addresses from multicast
            // addresses even though both payloads are IPv6 addresses.
            let suboption = if address.is_multicast() { 2u16 } else { 1u16 };
            payload.extend_from_slice(&suboption.to_be_bytes());
            payload.extend_from_slice(&16u16.to_be_bytes());
            payload.extend_from_slice(&address.octets());
            payload
        })
        .collect()
}

/// Return the parent domain of an FQDN for DHCPv6 domain-search.
fn parent_domain(fqdn: &str) -> Option<&str> {
    let fqdn = fqdn.trim_end_matches('.');
    fqdn.split_once('.').map(|(_, domain)| domain)
}

/// Encode a DNS name in uncompressed RFC 1035 wire format.
fn encode_domain_name(name: &str) -> Option<Vec<u8>> {
    let name = name.trim_end_matches('.');
    if name.is_empty() {
        return None;
    }

    let mut out = Vec::new();
    for label in name.split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);

    (out.len() <= 255).then_some(out)
}

/// Free the Machine object.
///
/// # Safety
///
/// This function dereferences a pointer to a Machine object which is an opaque pointer
/// consumed in C code.
///
/// This does not forget the memory afterwards, so the opaque pointer in the C code is now
/// unusable.
#[unsafe(no_mangle)]
pub extern "C" fn machine_free(ctx: *mut Machine) {
    if ctx.is_null() {
        return;
    }

    // SAFETY: Initial lint enablement: this ownership contract needs owner
    // review. `ctx` is the unreclaimed Box-produced Machine pointer returned by
    // discovery and is returned to the Rust allocator exactly once after all
    // uses and borrows end.
    unsafe {
        drop(Box::from_raw(ctx));
    }
}

#[cfg(test)]
mod test {
    use std::ffi::CString;
    use std::net::{Ipv4Addr, Ipv6Addr};
    use std::str::FromStr;

    use carbide_dhcp_common::VendorClass;
    use rpc::forge as rpc;

    use crate::carbide_set_config_ntp;
    use crate::discovery::Discovery;
    use crate::machine::{
        Machine, encode_domain_name, flatten_ipv6_addresses, machine_get_filename,
        machine_get_ntpservers, ntp_server_option_payload, parent_domain,
    };

    #[test]
    fn test_use_booturl_internal() {
        crate::carbide_set_config_next_server_ipv4("127.0.0.1".parse::<Ipv4Addr>().unwrap().into());

        let mut machine = Box::new(Machine {
            inner: rpc::DhcpRecord::default(),
            discovery_info: Discovery {
                relay_address: "127.0.0.1".parse().unwrap(),
                mac_address: "00:00:00:00:00:00".parse().unwrap(),
                _client_system: None,
                vendor_class: None,
                link_select_address: "127.0.0.1".parse().ok(),
                circuit_id: None,
                remote_id: None,
                desired_address: None,
            },
            vendor_class: VendorClass::from_str("HTTPClient:Arch:00011:UNDI:003000")
                .unwrap()
                .into(),
        });

        let out = machine_get_filename(&mut *machine);

        assert_ne!(out, std::ptr::null());

        // SAFETY: `out` is the unreclaimed pointer returned by this test's
        // `machine_get_filename` call and is converted back exactly once.
        let cstr = unsafe { CString::from_raw(out as *mut _) };

        assert_eq!(
            cstr,
            CString::new("http://127.0.0.1:8080/public/blobs/internal/aarch64/ipxe.efi").unwrap()
        );
    }

    #[test]
    fn test_use_booturl_from_api() {
        let dhcp_record = rpc::DhcpRecord {
            booturl: Some("https://foobar".to_string()),
            ..Default::default()
        };

        let mut machine = Box::new(Machine {
            inner: dhcp_record,
            discovery_info: Discovery {
                relay_address: "127.0.0.1".parse::<Ipv4Addr>().unwrap(),
                mac_address: "00:00:00:00:00:00".parse().unwrap(),
                _client_system: None,
                vendor_class: None,
                link_select_address: "127.0.0.1".parse::<Ipv4Addr>().ok(),
                circuit_id: None,
                remote_id: None,
                desired_address: None,
            },
            vendor_class: VendorClass::from_str("HTTPClient:Arch:00011:UNDI:003000")
                .unwrap()
                .into(),
        });

        let out = machine_get_filename(&mut *machine);

        assert_ne!(out, std::ptr::null());

        // SAFETY: `out` is the unreclaimed pointer returned by this test's
        // `machine_get_filename` call and is converted back exactly once.
        let cstr = unsafe { CString::from_raw(out as *mut _) };

        assert_eq!(cstr, CString::new("https://foobar").unwrap());
    }

    #[test]
    fn test_machine_get_ntpservers() {
        // SAFETY: The local CString remains live and NUL-terminated for this
        // synchronous configuration call.
        unsafe {
            let s = CString::new("10.0.0.1").unwrap();
            carbide_set_config_ntp(s.as_ptr());
        }

        // Test with ntp servers in the dhcp record
        let mut machine = Box::new(Machine {
            inner: rpc::DhcpRecord {
                ntp_servers: vec!["198.51.100.1".to_string(), "198.51.100.2".to_string()],
                ..Default::default()
            },
            discovery_info: Discovery {
                relay_address: "127.0.0.1".parse().unwrap(),
                mac_address: "00:00:00:00:00:01".parse().unwrap(),
                _client_system: None,
                vendor_class: None,
                link_select_address: None,
                circuit_id: None,
                remote_id: None,
                desired_address: None,
            },
            vendor_class: None,
        });

        let raw = machine_get_ntpservers(&mut *machine);
        // SAFETY: `raw` is the unreclaimed pointer returned by the getter above
        // and is converted back into its CString exactly once.
        let cstr = unsafe { CString::from_raw(raw) };
        assert_eq!(cstr.to_str().unwrap(), "198.51.100.1,198.51.100.2");

        // Test with no ntp servers in the dhcp record
        // SAFETY: The local CString remains live and NUL-terminated for this
        // synchronous configuration call.
        unsafe {
            let s = CString::new("10.0.0.2,10.0.0.3").unwrap();
            carbide_set_config_ntp(s.as_ptr());
        }

        let mut machine = Box::new(Machine {
            inner: rpc::DhcpRecord {
                ntp_servers: vec![],
                ..Default::default()
            },
            discovery_info: Discovery {
                relay_address: "127.0.0.1".parse().unwrap(),
                mac_address: "00:00:00:00:00:02".parse().unwrap(),
                _client_system: None,
                vendor_class: None,
                link_select_address: None,
                circuit_id: None,
                remote_id: None,
                desired_address: None,
            },
            vendor_class: None,
        });

        let raw = machine_get_ntpservers(&mut *machine);
        // SAFETY: `raw` is the unreclaimed pointer returned by the getter above
        // and is converted back into its CString exactly once.
        let cstr = unsafe { CString::from_raw(raw) };
        assert_eq!(cstr.to_str().unwrap(), "10.0.0.2,10.0.0.3");
    }

    #[test]
    fn encodes_ipv6_address_lists_for_dhcp_options() {
        // DHCPv6 option 23 is a flat sequence of 16-byte IPv6 addresses.
        let addresses = [
            "2001:db8::1".parse::<Ipv6Addr>().unwrap(),
            "2001:db8::2".parse::<Ipv6Addr>().unwrap(),
        ];

        assert_eq!(flatten_ipv6_addresses(&addresses).len(), 32);
    }

    #[test]
    fn encodes_ntp_server_suboptions() {
        // RFC 5908 option 56 wraps each server address in a suboption TLV.
        let addresses = [
            "2001:db8::123".parse::<Ipv6Addr>().unwrap(),
            "ff05::101".parse::<Ipv6Addr>().unwrap(),
        ];
        let payload = ntp_server_option_payload(&addresses);

        assert_eq!(&payload[..4], &[0, 1, 0, 16]);
        assert_eq!(&payload[4..20], &addresses[0].octets());
        assert_eq!(&payload[20..24], &[0, 2, 0, 16]);
        assert_eq!(&payload[24..], &addresses[1].octets());
    }

    #[test]
    fn encodes_fqdn_options_as_dns_names() {
        // Domain-search uses the parent domain while client-fqdn uses the
        // full hostname domain name.
        assert_eq!(parent_domain("host.forge.local"), Some("forge.local"));
        assert_eq!(
            encode_domain_name("forge.local").unwrap(),
            b"\x05forge\x05local\0".to_vec()
        );
    }
}
