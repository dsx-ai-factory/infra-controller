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
//! Kea DHCP hook library for Carbide.
//!
//! Example Kea configurations live in `examples/kea-dhcp4-carbide.conf` and
//! `examples/kea-dhcp6-carbide.conf`.

use std::ffi::CStr;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::atomic::AtomicI64;
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

use forge_tls::default as tls_default;
use libc::c_char;
use mac_address::MacAddress;
use metrics_endpoint::HealthController;
use once_cell::sync::Lazy;
use rpc::forge_tls_client::ForgeClientConfig;
use tokio::runtime::{Builder, Runtime};

mod cache;
#[allow(
    unreachable_pub,
    reason = "cbindgen exposes this module's public items through the Kea C++ ABI"
)]
mod discovery;
#[allow(
    unreachable_pub,
    reason = "cbindgen exposes this module's public items through the Kea C++ ABI"
)]
mod discovery_v6;
mod kea;
mod kea_logger;
#[allow(
    unreachable_pub,
    reason = "cbindgen exposes this module's public items through the Kea C++ ABI"
)]
mod lease_expiration;
#[allow(
    unreachable_pub,
    reason = "cbindgen exposes this module's public items through the Kea C++ ABI"
)]
mod machine;
#[allow(
    unreachable_pub,
    reason = "cbindgen exposes this module's public items through the Kea C++ ABI"
)]
mod machine_v6;
mod metrics;
#[cfg(any(test, feature = "test-support"))]
pub mod mock_api_server;
mod tls;

static CONFIG: Lazy<RwLock<CarbideDhcpContext>> =
    Lazy::new(|| RwLock::new(CarbideDhcpContext::default()));

static LOGGER: kea_logger::KeaLogger = kea_logger::KeaLogger;

#[derive(Debug)]
struct CarbideDhcpContext {
    api_endpoint: String,
    nameservers: Vec<Ipv4Addr>,
    dns_servers_ipv6: Vec<Ipv6Addr>,
    mqtt_server: Option<String>,
    ntpservers: Vec<Ipv4Addr>,
    ntp_servers_ipv6: Vec<Ipv6Addr>,
    provisioning_server_ipv4: Option<Ipv4Addr>,
    provisioning_server_ipv6: Option<Ipv6Addr>,
    rapid_commit_v6: bool,
    forge_root_ca_path: String,
    forge_client_cert_path: String,
    forge_client_key_path: String,
    metrics_endpoint: Option<SocketAddr>,
    metrics: Option<CarbideDhcpMetrics>,
    health_controller: Option<HealthController>,
    startup_time: chrono::DateTime<chrono::Utc>,
}

// The request/drop/reply counters are `carbide-instrument` events declared in
// `metrics.rs` and resolve from the global meter; this struct holds only the
// state the certificate-expiry gauge reports.
#[derive(Debug, Clone)]
struct CarbideDhcpMetrics {
    forge_client_config: ForgeClientConfig,
    certificate_expiration_value: Arc<AtomicI64>,
}

const METRICS_INIT_TIMEOUT: Duration = Duration::from_secs(5);
const METRICS_INIT_POLL_INTERVAL: Duration = Duration::from_millis(10);

fn wait_for_metrics_initialization() -> bool {
    let deadline = Instant::now() + METRICS_INIT_TIMEOUT;
    loop {
        if CONFIG.read().unwrap().metrics.is_some() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(METRICS_INIT_POLL_INTERVAL);
    }
}

/// Read a TLS file path from the environment, falling back to the built-in
/// default when the variable is unset, unreadable, or explicitly empty --
/// `std::env::var` returns `Ok("")` for an empty variable, and an empty path
/// must not reach the client config.
fn forge_tls_path(var: &str, default: &str, label: &str) -> String {
    match std::env::var(var) {
        Ok(path) if !path.is_empty() => return path,
        Ok(_) => {
            log::warn!(
                "{var} is set but empty; falling back to the built-in default {label} at {default}"
            )
        }
        Err(e) => {
            log::warn!(
                "{var} unset or unreadable ({e}); falling back to the built-in default {label} at {default}"
            )
        }
    }
    default.to_string()
}

impl Default for CarbideDhcpContext {
    fn default() -> Self {
        Self {
            api_endpoint: "https://[::1]:1079".to_string(),
            nameservers: vec![Ipv4Addr::new(1, 1, 1, 1)],
            dns_servers_ipv6: Vec::new(),
            forge_root_ca_path: forge_tls_path(
                "FORGE_ROOT_CAFILE_PATH",
                tls_default::ROOT_CA,
                "root CA",
            ),
            forge_client_cert_path: forge_tls_path(
                "FORGE_CLIENT_CERT_PATH",
                tls_default::CLIENT_CERT,
                "client certificate",
            ),
            forge_client_key_path: forge_tls_path(
                "FORGE_CLIENT_KEY_PATH",
                tls_default::CLIENT_KEY,
                "client key",
            ),
            ntpservers: vec![
                Ipv4Addr::new(172, 20, 0, 24),
                Ipv4Addr::new(172, 20, 0, 26),
                Ipv4Addr::new(172, 20, 0, 27),
            ], // local ntp servers
            ntp_servers_ipv6: Vec::new(),
            mqtt_server: None,
            provisioning_server_ipv4: None,
            provisioning_server_ipv6: None,
            rapid_commit_v6: false,
            metrics_endpoint: None,
            metrics: None,
            health_controller: None,
            startup_time: chrono::Utc::now(),
        }
    }
}

fn parse_ipv4_list(addresses: &str) -> Result<Vec<Ipv4Addr>, std::net::AddrParseError> {
    addresses
        .split(',')
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(str::parse)
        .collect()
}

fn format_ipv4_list(addresses: &[Ipv4Addr]) -> String {
    addresses
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse a comma-separated list of IPv6 addresses from hook configuration.
fn parse_ipv6_list(addresses: &str) -> Result<Vec<Ipv6Addr>, std::net::AddrParseError> {
    addresses
        .split(',')
        .map(str::trim)
        .filter(|address| !address.is_empty())
        .map(str::parse)
        .collect()
}

impl CarbideDhcpContext {
    fn get_tokio_runtime() -> &'static Runtime {
        static TOKIO: Lazy<Runtime> = Lazy::new(|| {
            Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("unable to build runtime?")
        });

        &TOKIO
    }
}

/// Take the config parameter from Kea and configure it as our API endpoint
///
/// # Safety
/// Function is unsafe as it dereferences a raw pointer given to it.  Caller is responsible
/// to validate that the pointer passed to it meets the necessary conditions to be dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_set_config_api(api: *const c_char) {
    // SAFETY: Kea supplies a non-null, NUL-terminated configuration string that
    // remains readable until this synchronous setter copies it.
    unsafe {
        let config_api = CStr::from_ptr(api).to_str().unwrap().to_owned();
        CONFIG.write().unwrap().api_endpoint = config_api;
    }
}

/// Take the next-server IP which will be configured as the endpoint for the iPXE client (and DNS
/// for now)
///
/// # Safety
///
/// None, todo!()
#[unsafe(no_mangle)]
pub extern "C" fn carbide_set_config_next_server_ipv4(next_server: u32) {
    CONFIG.write().unwrap().provisioning_server_ipv4 =
        Some(Ipv4Addr::from(next_server.to_be_bytes()));
}

/// Take the name servers for configuring nameservers in the dhcp responses
///
/// # Safety
/// Function is unsafe as it dereferences a raw pointer given to it.  Caller is responsible
/// to validate that the pointer passed to it meets the necessary conditions to be dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_set_config_name_servers(nameservers: *const c_char) {
    // SAFETY: Kea supplies a non-null, NUL-terminated configuration string that
    // remains readable until this synchronous setter copies it.
    unsafe {
        let nameserver_str = CStr::from_ptr(nameservers).to_str().unwrap().to_owned();
        match parse_ipv4_list(&nameserver_str) {
            Ok(nameservers) => CONFIG.write().unwrap().nameservers = nameservers,
            Err(err) => {
                log::error!("failed to parse nameserver configuration {nameserver_str}: {err}");
            }
        }
    }
}

/// Take the MQTT server for configuring mqtt_server in DHCP option 224.
///
/// # Safety
/// Function is unsafe as it dereferences a raw pointer given to it.  Caller is responsible
/// to validate that the pointer passed to it meets the necessary conditions to be dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_set_config_mqtt_server(mqttserver: *const c_char) {
    // SAFETY: Kea supplies a non-null, NUL-terminated configuration string that
    // remains readable until this synchronous setter copies it.
    unsafe {
        let mqttserver_str = CStr::from_ptr(mqttserver).to_str().unwrap().to_owned();
        CONFIG.write().unwrap().mqtt_server = Some(mqttserver_str);
    }
}

/// Take the NTP servers configuring NTP in the dhcp responses as fallback when the Carbide API `DhcpRecord` does not
/// have `ntp_servers` set.
///
/// # Safety
/// Function is unsafe as it dereferences a raw pointer given to it.  Caller is responsible
/// to validate that the pointer passed to it meets the necessary conditions to be dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_set_config_ntp(ntpservers: *const c_char) {
    // SAFETY: Kea supplies a non-null, NUL-terminated configuration string that
    // remains readable until this synchronous setter copies it.
    unsafe {
        let ntp_str = CStr::from_ptr(ntpservers).to_str().unwrap().to_owned();
        match parse_ipv4_list(&ntp_str) {
            Ok(ntpservers) => CONFIG.write().unwrap().ntpservers = ntpservers,
            Err(err) => {
                log::error!("failed to parse NTP server configuration {ntp_str}: {err}");
            }
        }
    }
}

/// Return a UTF-8 hook parameter string from a C pointer.
///
/// # Safety
/// `value` must be null only for invalid input, or point to a valid null-terminated C string.
unsafe fn hook_parameter_string(value: *const c_char, name: &str) -> Option<String> {
    if value.is_null() {
        log::error!("missing value for hook parameter {name}");
        return None;
    }

    // SAFETY: The null case was rejected above; the caller guarantees readable,
    // NUL-terminated storage for the duration of this conversion.
    match unsafe { CStr::from_ptr(value) }.to_str() {
        Ok(value) => Some(value.to_owned()),
        Err(err) => {
            log::error!("failed to parse hook parameter {name} as UTF-8: {err}");
            None
        }
    }
}

/// Take IPv6 DNS servers for DHCPv6 option rendering.
///
/// # Safety
/// Function is unsafe as it dereferences a raw pointer given to it. Caller is responsible
/// to validate that the pointer passed to it meets the necessary conditions to be dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hook_set_config_dns_servers_ipv6(servers: *const c_char) -> bool {
    // SAFETY: This forwards the exported function's C-string contract to the
    // helper, which checks null and copies the value before returning.
    unsafe {
        let Some(servers_str) = hook_parameter_string(servers, "hook-dns-servers-ipv6") else {
            return false;
        };
        match parse_ipv6_list(&servers_str) {
            Ok(servers) => {
                CONFIG.write().unwrap().dns_servers_ipv6 = servers;
                true
            }
            Err(err) => {
                log::error!("failed to parse DHCPv6 DNS server configuration {servers_str}: {err}");
                false
            }
        }
    }
}

/// Take IPv6 NTP servers for DHCPv6 option rendering.
///
/// # Safety
/// Function is unsafe as it dereferences a raw pointer given to it. Caller is responsible
/// to validate that the pointer passed to it meets the necessary conditions to be dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hook_set_config_ntp_servers_ipv6(servers: *const c_char) -> bool {
    // SAFETY: This forwards the exported function's C-string contract to the
    // helper, which checks null and copies the value before returning.
    unsafe {
        let Some(servers_str) = hook_parameter_string(servers, "hook-ntp-servers-ipv6") else {
            return false;
        };
        match parse_ipv6_list(&servers_str) {
            Ok(servers) => {
                CONFIG.write().unwrap().ntp_servers_ipv6 = servers;
                true
            }
            Err(err) => {
                log::error!("failed to parse DHCPv6 NTP server configuration {servers_str}: {err}");
                false
            }
        }
    }
}

/// Take the optional IPv6 provisioning-server address reserved for future DHCPv6 boot options.
///
/// # Safety
/// Function is unsafe as it dereferences a raw pointer given to it. Caller is responsible
/// to validate that the pointer passed to it meets the necessary conditions to be dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn hook_set_config_provisioning_server_ipv6(
    provisioning_server: *const c_char,
) -> bool {
    // SAFETY: This forwards the exported function's C-string contract to the
    // helper, which checks null and copies the value before returning.
    unsafe {
        let Some(provisioning_server_str) =
            hook_parameter_string(provisioning_server, "hook-provisioning-server-ipv6")
        else {
            return false;
        };
        if provisioning_server_str.trim().is_empty() {
            CONFIG.write().unwrap().provisioning_server_ipv6 = None;
            return true;
        }

        match provisioning_server_str.parse::<Ipv6Addr>() {
            Ok(provisioning_server) => {
                CONFIG.write().unwrap().provisioning_server_ipv6 = Some(provisioning_server);
                true
            }
            Err(err) => {
                log::error!(
                    "failed to parse DHCPv6 provisioning-server configuration {provisioning_server_str}: {err}"
                );
                false
            }
        }
    }
}

/// Set whether DHCPv6 rapid-commit rendering is enabled.
///
/// Rapid commit stays disabled by default for this milestone; the setter is
/// present so the Kea parameter is validated and ready for the later gate.
#[unsafe(no_mangle)]
pub extern "C" fn hook_set_config_rapid_commit_v6(enabled: bool) {
    if enabled {
        log::warn!("DHCPv6 rapid-commit is configured but remains disabled for this milestone");
    }
    CONFIG.write().unwrap().rapid_commit_v6 = false;
}

/// Take the config parameter from Kea and configure it as our metrics endpoint.
///
/// Returns false when the endpoint cannot be parsed, allowing Kea load to fail
/// before the process-lifetime metrics server starts.
///
/// # Safety
/// Function is unsafe as it dereferences a raw pointer given to it.  Caller is responsible
/// to validate that the pointer passed to it meets the necessary conditions to be dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_set_config_metrics_endpoint(endpoint: *const c_char) -> bool {
    // SAFETY: This forwards the exported function's C-string contract to the
    // helper, which checks null and copies the value before returning.
    unsafe {
        let Some(config_metrics_endpoint) =
            hook_parameter_string(endpoint, "carbide-metrics-endpoint")
        else {
            return false;
        };
        match config_metrics_endpoint.parse::<SocketAddr>() {
            Ok(metrics_endpoint) => {
                // Store the endpoint before starting the process-lifetime metrics server.
                log::info!("metrics endpoint: {config_metrics_endpoint}");
                CONFIG.write().unwrap().metrics_endpoint = Some(metrics_endpoint);
                static METRICS_SERVER: Lazy<()> = Lazy::new(|| {
                    let _ = thread::spawn(metrics::metrics_server);
                });
                Lazy::force(&METRICS_SERVER);
                if !wait_for_metrics_initialization() {
                    log::warn!(
                        "metrics endpoint configured but metrics did not initialize within {METRICS_INIT_TIMEOUT:?}"
                    );
                }
                true
            }
            Err(err) => {
                log::error!("failed to parse metrics endpoint {config_metrics_endpoint} : {err}");
                false
            }
        }
    }
}

/// Increments the legacy DHCPv4 request counter.
///
/// # Safety
///
/// None
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_increment_total_requests() {
    metrics::increment_total_requests();
}

/// Increments the DHCPv6 request counter, labelled by the request's message
/// type. `message_type` is the raw DHCPv6 message-type code reported by Kea.
///
/// # Safety
///
/// None
#[unsafe(no_mangle)]
pub extern "C" fn carbide_increment_v6_requests(message_type: u8) {
    metrics::increment_v6_requests(metrics::V6RequestMessageType::from(message_type));
}

/// Increments the legacy DHCPv4 dropped-or-refused request counter. The reason
/// string is mapped onto the bounded [`metrics::DropReason`] taxonomy; a
/// string outside the taxonomy (or a null / non-UTF-8 input) is bucketed as
/// `Unknown` so the metric's label domain stays closed.
///
/// # Safety
/// Function is unsafe as it dereferences a raw pointer given to it.  Caller is responsible
/// to validate that the pointer passed to it meets the necessary conditions to be dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_increment_dropped_requests(reason: *const c_char) {
    let reason = if reason.is_null() {
        metrics::DropReason::Unknown
    } else {
        // SAFETY: The C ABI caller keeps this non-null, NUL-terminated reason
        // string readable for the synchronous label conversion.
        unsafe { CStr::from_ptr(reason) }
            .to_str()
            .map_or(metrics::DropReason::Unknown, metrics::DropReason::from)
    };
    metrics::increment_dropped_requests(reason);
}

/// Increments counter for number of DHCP replies sent, labelled by the
/// reply's message type. `message_type` is the raw RFC 2131 message-type code
/// from the response packet (`Pkt4::getType()`); the mapping onto the bounded
/// label lives in [`metrics::ReplyMessageType`].
///
/// # Safety
///
/// None
#[unsafe(no_mangle)]
pub extern "C" fn carbide_increment_reply_sent(message_type: u8) {
    metrics::increment_reply_sent(metrics::ReplyMessageType::from(message_type));
}

/// Increments counter for number of DHCPv6 replies sent, labelled by the
/// response's message type. `message_type` is the raw DHCPv6 message-type code
/// from the response packet (`Pkt6::getType()`); the mapping onto the bounded
/// label lives in [`metrics::V6ReplyMessageType`].
///
/// # Safety
///
/// None
#[unsafe(no_mangle)]
pub extern "C" fn carbide_increment_v6_reply_sent(message_type: u8) {
    metrics::increment_v6_reply_sent(metrics::V6ReplyMessageType::from(message_type));
}

/// Increments the DHCPv6 dropped-or-refused request counter.
///
/// # Safety
///
/// None
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_increment_dropped_v6_requests(reason: *const c_char) {
    let reason = if reason.is_null() {
        metrics::V6DropReason::Unknown
    } else {
        // SAFETY: The C ABI caller keeps this non-null, NUL-terminated reason
        // string readable for the synchronous label conversion.
        unsafe { CStr::from_ptr(reason) }
            .to_str()
            .map_or(metrics::V6DropReason::Unknown, metrics::V6DropReason::from)
    };
    metrics::increment_dropped_v6_requests(reason);
}

/// Invalidates DHCPv6 lease-cache entries for an expired lease.
///
/// # Safety
///
/// `ip_address` and `mac_address` must be valid null-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_invalidate_v6_lease_cache(
    ip_address: *const c_char,
    mac_address: *const c_char,
) -> usize {
    if ip_address.is_null() || mac_address.is_null() {
        return 0;
    }

    // SAFETY: Both pointers passed the null guard and the caller keeps their
    // NUL-terminated strings readable for this synchronous parse.
    let Ok(ip_address) = unsafe { CStr::from_ptr(ip_address) }.to_str() else {
        return 0;
    };
    // SAFETY: Both pointers passed the null guard and the caller keeps their
    // NUL-terminated strings readable for this synchronous parse.
    let Ok(mac_address) = unsafe { CStr::from_ptr(mac_address) }.to_str() else {
        return 0;
    };
    match (
        ip_address.parse::<Ipv6Addr>(),
        mac_address.parse::<MacAddress>(),
    ) {
        (Ok(ip_address), Ok(mac_address)) => cache::invalidate_v6_lease(ip_address, mac_address),
        (ip_result, mac_result) => {
            log::warn!(
                "Unable to invalidate DHCPv6 lease cache for expired lease: ip={ip_address} ip_error={:?} mac={mac_address} mac_error={:?}",
                ip_result.err(),
                mac_result.err()
            );
            0
        }
    }
}

/// Clears the recent-expiry DHCPv6 lease cache tombstone for a lease.
///
/// # Safety
///
/// `ip_address` and `mac_address` must be valid null-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn carbide_clear_v6_lease_cache_invalidation(
    ip_address: *const c_char,
    mac_address: *const c_char,
) -> bool {
    if ip_address.is_null() || mac_address.is_null() {
        return false;
    }

    // SAFETY: Both pointers passed the null guard and the caller keeps their
    // NUL-terminated strings readable for this synchronous parse.
    let Ok(ip_address) = unsafe { CStr::from_ptr(ip_address) }.to_str() else {
        return false;
    };
    // SAFETY: Both pointers passed the null guard and the caller keeps their
    // NUL-terminated strings readable for this synchronous parse.
    let Ok(mac_address) = unsafe { CStr::from_ptr(mac_address) }.to_str() else {
        return false;
    };
    match (
        ip_address.parse::<Ipv6Addr>(),
        mac_address.parse::<MacAddress>(),
    ) {
        (Ok(ip_address), Ok(mac_address)) => {
            cache::clear_v6_lease_invalidation(ip_address, mac_address)
        }
        (ip_result, mac_result) => {
            log::warn!(
                "Unable to clear DHCPv6 lease cache invalidation: ip={ip_address} ip_error={:?} mac={mac_address} mac_error={:?}",
                ip_result.err(),
                mac_result.err()
            );
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::CString;
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::{
        format_ipv4_list, hook_set_config_dns_servers_ipv6, hook_set_config_ntp_servers_ipv6,
        hook_set_config_provisioning_server_ipv6, parse_ipv4_list, parse_ipv6_list,
    };

    #[test]
    fn parses_comma_separated_ipv4_list() {
        let addresses = parse_ipv4_list("1.1.1.1, 8.8.8.8,172.20.0.24").unwrap();

        assert_eq!(
            addresses,
            vec![
                Ipv4Addr::new(1, 1, 1, 1),
                Ipv4Addr::new(8, 8, 8, 8),
                Ipv4Addr::new(172, 20, 0, 24),
            ]
        );
    }

    #[test]
    fn rejects_non_ipv4_list_entries() {
        assert!(parse_ipv4_list("1.1.1.1,fd00::1").is_err());
        assert!(parse_ipv4_list("1.1.1.1,not-an-ip").is_err());
    }

    #[test]
    fn parses_empty_ipv4_list_as_empty() {
        assert_eq!(parse_ipv4_list("").unwrap(), Vec::<Ipv4Addr>::new());
        assert_eq!(parse_ipv4_list("  ").unwrap(), Vec::<Ipv4Addr>::new());
    }

    #[test]
    fn parses_trailing_comma_ipv4_list() {
        let addresses = parse_ipv4_list("1.1.1.1,").unwrap();

        assert_eq!(addresses, vec![Ipv4Addr::new(1, 1, 1, 1)]);
    }

    #[test]
    fn formats_ipv4_list_for_kea_option_payload() {
        let addresses = [Ipv4Addr::new(1, 1, 1, 1), Ipv4Addr::new(8, 8, 8, 8)];

        assert_eq!(format_ipv4_list(&addresses), "1.1.1.1,8.8.8.8");
    }

    #[test]
    fn parses_comma_separated_ipv6_list() {
        let addresses = parse_ipv6_list("2001:db8::1, 2001:db8::2").unwrap();

        assert_eq!(
            addresses,
            vec![
                "2001:db8::1".parse::<Ipv6Addr>().unwrap(),
                "2001:db8::2".parse::<Ipv6Addr>().unwrap(),
            ]
        );
    }

    #[test]
    fn rejects_non_ipv6_list_entries() {
        assert!(parse_ipv6_list("2001:db8::1,1.1.1.1").is_err());
        assert!(parse_ipv6_list("2001:db8::1,not-an-ip").is_err());
    }

    #[test]
    fn v6_hook_setters_report_invalid_addresses() {
        let invalid_list = CString::new("2001:db8::1,not-an-ip").unwrap();
        let invalid_address = CString::new("not-an-ip").unwrap();

        // Invalid present hook values must fail Kea load instead of leaving
        // missing or stale DHCPv6 option state behind.
        // SAFETY: `invalid_list` remains live and NUL-terminated for this call.
        let dns_result = unsafe { hook_set_config_dns_servers_ipv6(invalid_list.as_ptr()) };
        // SAFETY: `invalid_list` remains live and NUL-terminated for this call.
        let ntp_result = unsafe { hook_set_config_ntp_servers_ipv6(invalid_list.as_ptr()) };
        // SAFETY: `invalid_address` remains live and NUL-terminated for this
        // call.
        let provisioning_result =
            unsafe { hook_set_config_provisioning_server_ipv6(invalid_address.as_ptr()) };

        assert!(!dns_result);
        assert!(!ntp_result);
        assert!(!provisioning_result);
    }
}
