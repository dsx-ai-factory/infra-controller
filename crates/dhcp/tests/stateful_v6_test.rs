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
use std::collections::BTreeMap;
use std::fmt::Display;
use std::fs;
use std::net::{Ipv6Addr, SocketAddr, UdpSocket};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use carbide_test_support::Outcome::Yields;
use carbide_test_support::scenarios;
use dhcp::mock_api_server::{self, ENDPOINT_DISCOVER_DHCP, ENDPOINT_EXPIRE_DHCP_LEASE};
use dhcproto::v6::{self, DhcpOption, OptionCode, Status};
use eyre::WrapErr;
use rpc::forge as rpc;

mod common;

use common::{
    DHCPv6Factory, Kea6, Kea6Config, Kea6ExpiredLeasesProcessing, labeled_counter_value,
    send_and_recv_v6, v6_drop_metric_value, wait_for_labeled_counter_at_least,
    wait_for_v6_drop_metric_at_least,
};

const READ_TIMEOUT: Duration = Duration::from_millis(500);
const MEMFILE_TIMEOUT: Duration = Duration::from_secs(2);
const EXPIRY_TIMEOUT: Duration = Duration::from_secs(15);
const METRIC_TIMEOUT: Duration = Duration::from_secs(5);
const KEA_LEASE_TYPE_NA: u32 = 0;
const KEA_LEASE_STATE_DEFAULT: u32 = 0;
const KEA_LEASE_STATE_DECLINED: u32 = 1;

fn assert_assigned_leases(stats: &serde_json::Value, subnet_count: i64, pool_count: i64) {
    assert_eq!(stats["subnet[1].assigned-nas"][0][0], subnet_count);
    // Kea 2.4 adds pool gauges; their numeric IDs are assigned by Kea.
    for (name, samples) in stats.as_object().unwrap() {
        if name.starts_with("subnet[1].pool[") && name.ends_with(".assigned-nas") {
            assert_eq!(samples[0][0], pool_count, "{name}");
        }
    }
}

// Represent the fields from one current Kea DHCPv6 memfile lease row that the
// integration scenarios inspect.
#[derive(Debug)]
struct Lease6Entry {
    address: Ipv6Addr,
    duid: String,
    valid_lifetime: u32,
    lease_type: u32,
    hwaddr: String,
    state: u32,
}

impl Lease6Entry {
    // Report whether this row is an active non-temporary address lease for the
    // expected address and client DUID.
    fn is_active_na_for(&self, address: Ipv6Addr, duid: &str) -> bool {
        self.address == address
            && normalize_hex(&self.duid) == normalize_hex(duid)
            && self.valid_lifetime > 0
            && self.lease_type == KEA_LEASE_TYPE_NA
            && self.state == KEA_LEASE_STATE_DEFAULT
    }
}

// Normalize the separator and case variants Kea uses for hexadecimal client
// identifiers before comparing them.
fn normalize_hex(value: &str) -> String {
    value
        .chars()
        .filter(|ch| ch.is_ascii_hexdigit())
        .map(|ch| ch.to_ascii_lowercase())
        .collect()
}

// Parse one typed memfile column while retaining the row and file context on
// malformed data.
fn parse_memfile_field<T>(
    value: &str,
    name: &str,
    line_number: usize,
    path: &Path,
    line: &str,
) -> Result<T, eyre::Report>
where
    T: FromStr,
    T::Err: Display,
{
    value.parse().map_err(|error| {
        eyre::eyre!(
            "invalid {name} on line {line_number} of kea memfile {}: {line} ({error})",
            path.display()
        )
    })
}

// Own one mock Core API, Kea process, DHCP socket, and isolated lease memfile
// for a real-hook integration scenario.
struct Harness {
    _rt: tokio::runtime::Runtime,
    api_server: mock_api_server::MockAPIServer,
    kea: Kea6,
    socket: UdpSocket,
    _lease_dir: tempfile::TempDir,
    lease_file_path: PathBuf,
}

impl Harness {
    // Start a harness with the standard stateful DHCPv6 configuration.
    fn new() -> Self {
        Self::new_with_config(Kea6Config::default())
    }

    // Start a harness with scenario-specific Kea timers and identity settings.
    fn new_with_config(config: Kea6Config) -> Self {
        Self::new_with_leases(config, "")
    }

    // Load existing lease state before Kea starts so reuse paths are exercised.
    fn new_with_leases(config: Kea6Config, leases: &str) -> Self {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        let api_server = rt.block_on(mock_api_server::MockAPIServer::start());
        let lease_dir = tempfile::tempdir().unwrap();
        let lease_file_path = lease_dir.path().join("kea-leases6.csv");
        let (mut kea, socket) =
            Kea6::start_with_config(api_server.local_http_addr(), Some(&lease_file_path), config)
                .unwrap();
        socket.set_read_timeout(Some(READ_TIMEOUT)).unwrap();
        if !leases.is_empty() {
            // Let the native daemon provide its schema so Kea 2.4 does not
            // rotate fixtures into an LFC conversion file at startup.
            let native = fs::read_to_string(&lease_file_path).unwrap();
            let native_header = native.lines().next().unwrap();
            let mut fixture = leases.lines();
            let fixture_header = fixture.next().unwrap();
            let suffix = if native_header == fixture_header {
                ""
            } else {
                assert_eq!(native_header, format!("{fixture_header},pool_id"));
                ",0"
            };
            let mut seeded = format!("{native_header}\n");
            for row in fixture {
                seeded.push_str(&format!("{row}{suffix}\n"));
            }
            fs::write(&lease_file_path, seeded).unwrap();
            kea.restart().unwrap();
        }

        Self {
            _rt: rt,
            api_server,
            kea,
            socket,
            _lease_dir: lease_dir,
            lease_file_path,
        }
    }

    // Read Kea's append-only memfile as its latest state for each address.
    fn read_leases(&self) -> Result<Vec<Lease6Entry>, eyre::Report> {
        let contents = fs::read_to_string(&self.lease_file_path).wrap_err_with(|| {
            format!(
                "failed to read kea memfile {}",
                self.lease_file_path.display()
            )
        })?;

        let mut current_by_address = BTreeMap::new();
        for (line_index, line) in contents.lines().enumerate() {
            if line_index == 0 && line.starts_with("address,") {
                continue;
            }
            let columns = line.split(',').collect::<Vec<_>>();
            eyre::ensure!(
                columns.len() >= 14,
                "kea memfile {} line {} has {} columns instead of at least 14: {line}",
                self.lease_file_path.display(),
                line_index + 1,
                columns.len()
            );
            let lease = Lease6Entry {
                address: parse_memfile_field(
                    columns[0],
                    "address",
                    line_index + 1,
                    &self.lease_file_path,
                    line,
                )?,
                duid: columns[1].to_string(),
                valid_lifetime: parse_memfile_field(
                    columns[2],
                    "valid lifetime",
                    line_index + 1,
                    &self.lease_file_path,
                    line,
                )?,
                lease_type: parse_memfile_field(
                    columns[6],
                    "lease type",
                    line_index + 1,
                    &self.lease_file_path,
                    line,
                )?,
                hwaddr: columns[12].to_string(),
                state: parse_memfile_field(
                    columns[13],
                    "lease state",
                    line_index + 1,
                    &self.lease_file_path,
                    line,
                )?,
            };
            current_by_address.insert(lease.address, lease);
        }
        Ok(current_by_address.into_values().collect())
    }

    // Find an active non-temporary lease for one address and DUID.
    fn active_lease(
        &self,
        address: Ipv6Addr,
        duid: &str,
    ) -> Result<Option<Lease6Entry>, eyre::Report> {
        Ok(self
            .read_leases()?
            .into_iter()
            .find(|lease| lease.is_active_na_for(address, duid)))
    }

    // Wait for Kea to persist an active lease, surfacing the last memfile error
    // or state when the bounded poll expires.
    fn wait_for_active_lease(
        &self,
        address: Ipv6Addr,
        duid: &str,
    ) -> Result<Lease6Entry, eyre::Report> {
        let deadline = Instant::now() + MEMFILE_TIMEOUT;
        loop {
            match self.active_lease(address, duid) {
                Ok(Some(lease)) => return Ok(lease),
                Ok(None) if Instant::now() < deadline => {}
                Ok(None) => {
                    eyre::bail!(
                        "kea did not persist active lease {address} for DUID {duid}: {:?}",
                        self.read_leases()?
                    );
                }
                Err(_) if Instant::now() < deadline => {}
                Err(error) => return Err(error),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // Wait for an established address and DUID pair to stop being an active
    // lease without mistaking memfile I/O failures for successful cleanup.
    fn wait_for_no_active_lease(&self, address: Ipv6Addr, duid: &str) -> Result<(), eyre::Report> {
        let deadline = Instant::now() + MEMFILE_TIMEOUT;
        loop {
            match self.active_lease(address, duid) {
                Ok(None) => return Ok(()),
                Ok(Some(_)) if Instant::now() < deadline => {}
                Ok(Some(_)) => {
                    eyre::bail!(
                        "kea retained lease {address} for DUID {duid}: {:?}",
                        self.read_leases()?
                    );
                }
                Err(_) if Instant::now() < deadline => {}
                Err(error) => return Err(error),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // Complete SOLICIT and REQUEST through Kea and return its server identifier
    // together with the API-selected address it persisted.
    fn establish_lease(&self, idx: u8) -> Result<(Vec<u8>, Ipv6Addr), eyre::Report> {
        let expected_addr = DHCPv6Factory::mock_addr(idx);
        let discover_calls = self.api_server.calls_for(ENDPOINT_DISCOVER_DHCP);
        let advertise = send_and_recv_v6(&self.socket, DHCPv6Factory::solicit(idx))?
            .expect("Kea did not respond to stateful SOLICIT");
        assert_eq!(advertise.msg_type(), v6::MessageType::Advertise);
        assert_eq!(DHCPv6Factory::ia_addr(&advertise), Some(expected_addr));
        assert_discovery_metadata(
            &self.api_server.discoveries()[discover_calls],
            idx,
            &DHCPv6Factory::duid_ll(idx),
        );
        let server_id = DHCPv6Factory::server_id(&advertise);

        // Prove that the hook, rather than Kea's pool choice, owns persistence.
        let requested_addr = format!("2001:db8::f0{idx:02x}").parse().unwrap();
        assert_ne!(requested_addr, expected_addr);
        let reply = send_and_recv_v6(
            &self.socket,
            DHCPv6Factory::request(idx, server_id.clone(), requested_addr),
        )?
        .expect("Kea did not respond to stateful REQUEST");
        assert_eq!(reply.msg_type(), v6::MessageType::Reply);
        assert_eq!(DHCPv6Factory::ia_addr(&reply), Some(expected_addr));
        self.wait_for_active_lease(expected_addr, &DHCPv6Factory::duid_ll_hex(idx))?;
        assert_eq!(
            self.api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
            discover_calls + 1,
            "SOLICIT and REQUEST should share one lease-cache entry"
        );

        Ok((server_id, expected_addr))
    }

    // Wait for exactly one Core expiry call scoped to the expected IPv6 address
    // and relay-selected MAC address.
    fn wait_for_scoped_expiry(&self, address: Ipv6Addr, mac_address: &str) {
        let deadline = Instant::now() + EXPIRY_TIMEOUT;
        loop {
            let expired = self.api_server.expired_leases();
            assert!(
                expired.iter().all(|request| {
                    request.ip_address == address.to_string()
                        && request
                            .mac_address
                            .as_deref()
                            .is_some_and(|actual| actual.eq_ignore_ascii_case(mac_address))
                }),
                "expiry must remain scoped to {address}/{mac_address}: {expired:?}"
            );
            if expired.len() == 1 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Kea did not expire {address}/{mac_address}: {expired:?}"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

// Assert that real Kea relay metadata reaches the Core request boundary intact.
fn assert_discovery_metadata(discovery: &rpc::DhcpDiscovery, idx: u8, duid: &[u8]) {
    assert_eq!(discovery.mac_address, DHCPv6Factory::mac_string(idx));
    assert_eq!(discovery.relay_address, DHCPv6Factory::RELAY_LINK_ADDR);
    assert_eq!(
        discovery.link_address.as_deref(),
        Some(DHCPv6Factory::RELAY_LINK_ADDR)
    );
    assert_eq!(
        discovery.circuit_id.as_deref(),
        Some(DHCPv6Factory::RELAY_INTERFACE_ID_HEX)
    );
    assert_eq!(
        discovery.remote_id.as_deref(),
        Some(DHCPv6Factory::RELAY_REMOTE_ID_HEX)
    );
    assert_eq!(discovery.duid.as_deref(), Some(duid));
    assert_eq!(
        discovery.address_family,
        Some(rpc::AddressFamily::V6 as i32)
    );
    assert_eq!(
        discovery.message_kind,
        Some(rpc::MessageKind::V6Solicit as i32)
    );
}

// Build short lease timers so expiry and cache invalidation remain bounded in
// the real-Kea integration test.
fn short_expiry_config() -> Kea6Config {
    Kea6Config {
        preferred_lifetime: 3,
        valid_lifetime: 4,
        renew_timer: 1,
        rebind_timer: 2,
        rapid_commit_v6: false,
        kea_rapid_commit_v6: true,
        // Let Kea derive a conflicting MAC from the DUID so the hook must
        // replace it with the trusted relay-selected identity.
        mac_sources: Some(&["duid"]),
        expired_leases_processing: Some(Kea6ExpiredLeasesProcessing {
            reclaim_timer_wait_time: 1,
            flush_reclaimed_timer_wait_time: 1,
            hold_reclaimed_time: 0,
            max_reclaim_leases: 10,
            max_reclaim_time: 100,
            unwarned_reclaim_cycles: 1,
        }),
        ..Kea6Config::default()
    }
}

// Extract and compare the top-level DHCPv6 status option in a CONFIRM reply.
fn assert_status(response: &v6::Message, expected: Status) {
    match response.opts().get(OptionCode::StatusCode) {
        Some(DhcpOption::StatusCode(status)) => assert_eq!(status.status, expected),
        other => panic!("expected DHCPv6 status {expected:?}, got {other:?}"),
    }
}

// Return the sent-response counter for one DHCPv6 message-type label.
fn v6_reply_metric_value(endpoint: SocketAddr, message_type: &str) -> f64 {
    labeled_counter_value(
        endpoint,
        "carbide_dhcp_v6_replies_sent_total",
        "message_type",
        message_type,
    )
}

// Wait until the sent-response counter reaches the expected value.
fn wait_for_v6_reply_metric_at_least(
    endpoint: SocketAddr,
    message_type: &str,
    minimum: f64,
    timeout: Duration,
) -> bool {
    wait_for_labeled_counter_at_least(
        endpoint,
        "carbide_dhcp_v6_replies_sent_total",
        "message_type",
        message_type,
        minimum,
        timeout,
    )
}

// Exercise normal stateful renewal and both client-initiated lease-ending
// operations while Kea retains the API-selected address.
#[test]
fn stateful_lifecycle_keeps_kea_on_the_api_address() -> Result<(), eyre::Report> {
    let idx = 0x40;
    let h = Harness::new();
    let initial_replies = v6_reply_metric_value(h.kea.metrics_endpoint(), "reply");
    let (server_id, address) = h.establish_lease(idx)?;
    let duid = DHCPv6Factory::duid_ll_hex(idx);
    let discover_calls = h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP);

    for (exchange, packet) in [
        (
            "RENEW",
            DHCPv6Factory::renew(idx, server_id.clone(), address),
        ),
        ("REBIND", DHCPv6Factory::rebind(idx, address)),
    ] {
        let response = send_and_recv_v6(&h.socket, packet)?
            .unwrap_or_else(|| panic!("Kea did not respond to {exchange}"));
        assert_eq!(response.msg_type(), v6::MessageType::Reply, "{exchange}");
        assert_eq!(
            DHCPv6Factory::ia_addr(&response),
            Some(address),
            "{exchange}"
        );
        assert!(h.active_lease(address, &duid)?.is_some(), "{exchange}");
        assert_eq!(
            h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
            discover_calls,
            "{exchange} should reuse the cached API lease"
        );
    }

    let release = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::release_with_hop_count(idx, server_id, address, 0),
    )?
    .expect("Kea did not respond to RELEASE");
    assert_eq!(release.msg_type(), v6::MessageType::Reply);
    h.wait_for_no_active_lease(address, &duid)?;
    assert_eq!(
        h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
        discover_calls,
        "RELEASE is local lease state, not API deallocation"
    );
    assert_eq!(h.api_server.calls_for(ENDPOINT_EXPIRE_DHCP_LEASE), 0);
    assert!(h.api_server.expired_leases().is_empty());

    let decline_idx = idx + 1;
    let (decline_server_id, decline_address) = h.establish_lease(decline_idx)?;
    let decline_duid = DHCPv6Factory::duid_ll_hex(decline_idx);
    let discover_calls = h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP);
    let decline = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::decline_with_hop_count(decline_idx, decline_server_id, decline_address, 0),
    )?
    .expect("Kea did not respond to DECLINE");
    assert_eq!(decline.msg_type(), v6::MessageType::Reply);
    h.wait_for_no_active_lease(decline_address, &decline_duid)?;
    let declined_lease = h
        .read_leases()?
        .into_iter()
        .find(|lease| lease.address == decline_address)
        .expect("DECLINE should retain a quarantined Kea lease row");
    assert_eq!(declined_lease.state, KEA_LEASE_STATE_DECLINED);
    assert_eq!(
        h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
        discover_calls,
        "DECLINE is local lease state, not API deallocation"
    );
    assert_eq!(h.api_server.calls_for(ENDPOINT_EXPIRE_DHCP_LEASE), 0);
    assert!(wait_for_v6_reply_metric_at_least(
        h.kea.metrics_endpoint(),
        "reply",
        initial_replies + 6.0,
        METRIC_TIMEOUT,
    ));

    Ok(())
}

#[test]
fn mixed_address_and_prefix_requests_keep_address_lease() -> Result<(), eyre::Report> {
    // Verify prefix refusal with and without a configured prefix pool.
    for prefix_pool in [false, true] {
        let h = Harness::new_with_config(Kea6Config {
            prefix_pool,
            ..Kea6Config::default()
        });
        let idx = 42;
        let address = DHCPv6Factory::mock_addr(idx);
        let advertise = send_and_recv_v6(
            &h.socket,
            DHCPv6Factory::with_ia_pd(DHCPv6Factory::solicit(idx), idx),
        )?
        .expect("Kea must answer the address request despite unavailable prefix delegation");
        assert_eq!(advertise.msg_type(), v6::MessageType::Advertise);
        assert_address_with_no_prefix(&advertise, address, idx);
        let server_id = DHCPv6Factory::server_id(&advertise);

        for (exchange, packet) in [
            (
                "REQUEST",
                DHCPv6Factory::request(idx, server_id.clone(), address),
            ),
            ("RENEW", DHCPv6Factory::renew(idx, server_id, address)),
            ("REBIND", DHCPv6Factory::rebind(idx, address)),
        ] {
            let response = send_and_recv_v6(&h.socket, DHCPv6Factory::with_ia_pd(packet, idx))?
                .unwrap_or_else(|| panic!("Kea did not respond to mixed {exchange}"));
            assert_eq!(response.msg_type(), v6::MessageType::Reply, "{exchange}");
            assert_address_with_no_prefix(&response, address, idx);
            h.wait_for_active_lease(address, &DHCPv6Factory::duid_ll_hex(idx))?;
        }
        let leases = h.read_leases()?;
        assert_eq!(leases.len(), 1, "No delegated prefix may be persisted");
        assert_eq!(leases[0].lease_type, KEA_LEASE_TYPE_NA);
        assert_eq!(h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP), 1);
    }
    Ok(())
}

/// Existing prefixes must neither be reused on allocation nor extended by renewal.
#[test]
fn mixed_requests_leave_existing_prefix_lease_unchanged() -> Result<(), eyre::Report> {
    const PREFIX: &str = "2001:db8:1::";
    const HEADER: &str = "address,duid,valid_lifetime,expire,subnet_id,pref_lifetime,lease_type,iaid,prefix_len,fqdn_fwd,fqdn_rev,hostname,hwaddr,state,user_context,hwtype,hwaddr_source";
    let idx = 43;
    let duid = DHCPv6Factory::duid_ll_hex(idx);
    let expires = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() + 600;
    let prefix_row = format!("{PREFIX},{duid},600,{expires},1,300,2,{idx},64,0,0,,,0,,0,0");
    let h = Harness::new_with_leases(
        Kea6Config {
            prefix_pool: true,
            ..Kea6Config::default()
        },
        &format!("{HEADER}\n{prefix_row}\n"),
    );
    let prefix_row = fs::read_to_string(&h.lease_file_path)?
        .lines()
        .find(|line| line.starts_with(PREFIX))
        .expect("seeded prefix must be present")
        .to_string();
    let address = DHCPv6Factory::mock_addr(idx);
    let advertise = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::with_ia_pd(DHCPv6Factory::solicit(idx), idx),
    )?
    .expect("Kea must advertise an address without reusing the existing prefix");
    assert_address_with_no_prefix(&advertise, address, idx);
    let server_id = DHCPv6Factory::server_id(&advertise);

    for (exchange, packet) in [
        (
            "REQUEST",
            DHCPv6Factory::request(idx, server_id.clone(), address),
        ),
        ("RENEW", DHCPv6Factory::renew(idx, server_id, address)),
        ("REBIND", DHCPv6Factory::rebind(idx, address)),
    ] {
        let response = send_and_recv_v6(&h.socket, DHCPv6Factory::with_ia_pd(packet, idx))?
            .unwrap_or_else(|| panic!("Kea did not respond to mixed {exchange}"));
        assert_eq!(response.msg_type(), v6::MessageType::Reply, "{exchange}");
        assert_address_with_no_prefix(&response, address, idx);
        h.wait_for_active_lease(address, &duid)?;
        // Compare the complete persisted row, including expiry, after each exchange.
        let contents = fs::read_to_string(&h.lease_file_path)?;
        let prefix_rows = contents
            .lines()
            .filter(|line| line.starts_with(PREFIX))
            .collect::<Vec<_>>();
        assert_eq!(prefix_rows, vec![prefix_row.as_str()], "{exchange}");
    }
    assert_eq!(h.read_leases()?.len(), 2);
    Ok(())
}

/// Prefix refusal must also preserve the address committed during SOLICIT.
#[test]
fn mixed_rapid_commit_persists_only_address_lease() -> Result<(), eyre::Report> {
    let h = Harness::new_with_config(Kea6Config {
        prefix_pool: true,
        rapid_commit_v6: true,
        ..Kea6Config::default()
    });
    let idx = 44;
    let address = DHCPv6Factory::mock_addr(idx);
    let response = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::with_ia_pd(
            DHCPv6Factory::solicit_with_rapid_commit_options(idx, 1),
            idx,
        ),
    )?
    .expect("Kea must commit the address despite unavailable prefix delegation");
    assert_eq!(response.msg_type(), v6::MessageType::Reply);
    assert_address_with_no_prefix(&response, address, idx);
    assert_eq!(
        response
            .opts()
            .get_all(OptionCode::RapidCommit)
            .unwrap()
            .len(),
        1
    );
    h.wait_for_active_lease(address, &DHCPv6Factory::duid_ll_hex(idx))?;
    let leases = h.read_leases()?;
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].lease_type, KEA_LEASE_TYPE_NA);
    let discoveries = h.api_server.discoveries();
    assert_eq!(discoveries.len(), 1);
    assert_eq!(
        discoveries[0].message_kind,
        Some(rpc::MessageKind::V6Request as i32)
    );
    Ok(())
}

/// Verify prefix refusal is scoped to IA_PD while the address allocation succeeds.
fn assert_address_with_no_prefix(response: &v6::Message, address: Ipv6Addr, idx: u8) {
    if let Some(DhcpOption::StatusCode(status)) = response.opts().get(OptionCode::StatusCode) {
        assert_eq!(status.status, Status::Success);
    }
    let Some(DhcpOption::IANA(ia_na)) = response.opts().get(OptionCode::IANA) else {
        panic!("Missing IA_NA response: {response:?}");
    };
    assert_eq!(ia_na.id, u32::from(idx));
    if let Some(DhcpOption::StatusCode(status)) = ia_na.opts.get(OptionCode::StatusCode) {
        assert_eq!(status.status, Status::Success);
    }
    let Some(DhcpOption::IAAddr(ia_addr)) = ia_na.opts.get(OptionCode::IAAddr) else {
        panic!("Missing IAADDR response: {response:?}");
    };
    assert_eq!(ia_addr.addr, address);
    assert!(ia_addr.preferred_life > 0);
    assert!(ia_addr.valid_life >= ia_addr.preferred_life);
    let Some(DhcpOption::IAPD(prefix)) = response.opts().get(OptionCode::IAPD) else {
        panic!("Missing IA_PD response: {response:?}");
    };
    assert_eq!(prefix.id, u32::from(idx));
    assert!(prefix.opts.get(OptionCode::IAPrefix).is_none());
    match prefix.opts.get(OptionCode::StatusCode) {
        Some(DhcpOption::StatusCode(status)) => assert_eq!(status.status, Status::NoPrefixAvail),
        other => panic!("Expected IA_PD-scoped NoPrefixAvail, got {other:?}"),
    }
}

/// Verify valid non-MAC DUIDs fail at the trusted relay-identity boundary.
///
/// DUID-EN and DUID-UUID are valid identifiers, but neither may select a
/// cross-family machine row without relay-supplied option 79.
#[test]
fn non_mac_duids_without_option79_are_dropped() {
    let h = Harness::new();
    let initial_drops = v6_drop_metric_value(h.kea.metrics_endpoint(), "no_mac_no_option79");
    scenarios!(run = |(client_index, duid)| {
        // Keep the relay envelope but omit its trusted client-link-layer option.
        send_and_recv_v6(
            &h.socket,
            DHCPv6Factory::solicit_with_duid(client_index, duid, false),
        )
        .map(|response| response.is_none())
        .map_err(|error| error.to_string())
    };
        "valid non-MAC DUID without trusted relay identity" {
            // Enterprise identifiers contain no standardized link-layer address.
            (0x45, DHCPv6Factory::duid_en(12)) => Yields(true),
            // UUID identifiers exercise the other supported non-MAC DUID format.
            (0x46, DHCPv6Factory::duid_uuid(0x46)) => Yields(true),
        }
    );

    // Both valid DUID forms must share the stable identity failure and avoid API calls.
    assert_eq!(h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP), 0);
    assert!(wait_for_v6_drop_metric_at_least(
        h.kea.metrics_endpoint(),
        "no_mac_no_option79",
        initial_drops + 2.0,
        METRIC_TIMEOUT,
    ));
}

// Prove trusted relay identity scopes persistence and expiry, and that expiry
// invalidates the cached API allocation before the next solicitation.
#[test]
fn relay_identity_scopes_expiry_and_refreshes_the_cache() -> Result<(), eyre::Report> {
    let idx = 0x41;
    let duid_idx = 0x61;
    let h = Harness::new_with_config(short_expiry_config());
    let duid = DHCPv6Factory::duid_ll(duid_idx);
    let duid_hex = DHCPv6Factory::duid_hex(&duid);
    let expected_addr = DHCPv6Factory::mock_addr(idx);
    let expected_mac = DHCPv6Factory::mac_string(idx);
    let initial_drops = v6_drop_metric_value(h.kea.metrics_endpoint(), "no_mac_no_option79");

    assert!(
        send_and_recv_v6(
            &h.socket,
            DHCPv6Factory::solicit_with_inner_option79(idx, DHCPv6Factory::duid_en(12)),
        )?
        .is_none(),
        "a client-supplied option 79 must not substitute for trusted relay identity"
    );
    assert_eq!(h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP), 0);
    assert!(wait_for_v6_drop_metric_at_least(
        h.kea.metrics_endpoint(),
        "no_mac_no_option79",
        initial_drops + 1.0,
        METRIC_TIMEOUT,
    ));

    let advertise = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::solicit_with_duid(idx, duid.clone(), true),
    )?
    .expect("Kea did not respond when relay option 79 supplied the identity");
    assert_eq!(DHCPv6Factory::ia_addr(&advertise), Some(expected_addr));
    assert_eq!(
        h.api_server.discoveries().last().unwrap().mac_address,
        expected_mac
    );
    assert!(
        h.kea
            .wait_for_log("option 79 MAC disagrees with DUID MAC", MEMFILE_TIMEOUT)
    );
    let server_id = DHCPv6Factory::server_id(&advertise);
    let reply = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::request_with_duid(idx, server_id, expected_addr, duid.clone(), true),
    )?
    .expect("Kea did not persist the relay-identified lease");
    assert_eq!(DHCPv6Factory::ia_addr(&reply), Some(expected_addr));
    let lease = h.wait_for_active_lease(expected_addr, &duid_hex)?;
    assert!(
        lease.hwaddr.eq_ignore_ascii_case(&expected_mac),
        "{lease:?}"
    );

    h.wait_for_scoped_expiry(expected_addr, &expected_mac);
    let refreshed_addr = "2001:db8::ee41".parse::<Ipv6Addr>()?;
    h.api_server
        .set_address_override(&expected_mac, &refreshed_addr.to_string());
    let discover_calls = h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP);
    let retry = send_and_recv_v6(&h.socket, DHCPv6Factory::solicit_with_duid(idx, duid, true))?
        .expect("Kea did not respond after expiry invalidated the lease cache");
    assert_eq!(DHCPv6Factory::ia_addr(&retry), Some(refreshed_addr));
    assert_eq!(
        h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
        discover_calls + 1,
        "post-expiry retry must fetch the current API address"
    );

    Ok(())
}

// Firmware and the installed OS use different DUIDs on the same physical NIC.
#[test]
fn boot_stage_duid_change_reuses_the_mac_owned_address() -> Result<(), eyre::Report> {
    let idx = 0x47;
    let h = Harness::new();
    let address = DHCPv6Factory::mock_addr(idx);
    let firmware_duid = DHCPv6Factory::duid_uuid(idx);
    let os_duid = DHCPv6Factory::duid_en(12);
    for duid in [&firmware_duid, &os_duid] {
        let advertise = send_and_recv_v6(
            &h.socket,
            DHCPv6Factory::solicit_with_duid(idx, duid.clone(), true),
        )?
        .expect("boot-stage SOLICIT should receive an ADVERTISE");
        assert_eq!(DHCPv6Factory::ia_addr(&advertise), Some(address));
        if duid == &os_duid {
            assert!(
                h.active_lease(address, &DHCPv6Factory::duid_hex(&firmware_duid))?
                    .is_some(),
                "SOLICIT must preserve the firmware lease until REQUEST"
            );
        }
        let reply = send_and_recv_v6(
            &h.socket,
            DHCPv6Factory::request_with_duid(
                idx,
                DHCPv6Factory::server_id(&advertise),
                address,
                duid.clone(),
                true,
            ),
        )?
        .expect("boot-stage REQUEST must complete despite a previous DUID lease");
        assert_eq!(DHCPv6Factory::ia_addr(&reply), Some(address));
        h.wait_for_active_lease(address, &DHCPv6Factory::duid_hex(duid))?;
    }
    assert!(
        h.active_lease(address, &DHCPv6Factory::duid_hex(&firmware_duid))?
            .is_none()
    );
    assert!(
        h.api_server.expired_leases().is_empty(),
        "a boot-stage handoff must not expire the Core allocation"
    );
    let stats = h.kea.statistics()?;
    assert_assigned_leases(&stats, 1, 1);

    Ok(())
}

#[test]
fn address_collision_does_not_replace_another_mac_lease() -> Result<(), eyre::Report> {
    let h = Harness::new();
    let owner = 0x48;
    let other = 0x49;
    let (_, address) = h.establish_lease(owner)?;
    h.api_server
        .set_address_override(&DHCPv6Factory::mac_string(other), &address.to_string());
    let response = send_and_recv_v6(&h.socket, DHCPv6Factory::solicit(other))?;
    assert!(response.as_ref().and_then(DHCPv6Factory::ia_addr).is_none());
    assert!(
        h.active_lease(address, &DHCPv6Factory::duid_ll_hex(owner))?
            .is_some()
    );
    // A rejected collision must not strand Kea in pool-wide allocation retries.
    h.establish_lease(0x50)?;
    Ok(())
}

// A cached lease can be updated by an existing-DUID REQUEST without SELECT.
// Hold that transaction at Core discovery and require the new boot stage to retry.
#[test]
fn handoff_waits_for_existing_duid_transaction() -> Result<(), eyre::Report> {
    let idx = 0x51;
    let mut h = Harness::new();
    let (server_id, address) = h.establish_lease(idx)?;
    h.kea.restart()?;
    let (entered, resume) = h.api_server.pause_next_discovery();
    h.socket
        .send(&DHCPv6Factory::request(idx, server_id.clone(), address))?;
    h._rt
        .block_on(async { tokio::time::timeout(METRIC_TIMEOUT, entered).await })??;
    let new_duid = DHCPv6Factory::duid_en(19);
    let request =
        DHCPv6Factory::request_with_duid(idx, server_id.clone(), address, new_duid.clone(), true);
    h.socket.send(&request)?;
    assert!(
        wait_for_v6_drop_metric_at_least(
            h.kea.metrics_endpoint(),
            "lease_transaction_busy",
            1.0,
            METRIC_TIMEOUT
        ),
        "a competing boot-stage packet must retry before Kea reads or mutates leases"
    );
    assert!(
        h.active_lease(address, &DHCPv6Factory::duid_ll_hex(idx))?
            .is_some()
    );
    resume.send(()).unwrap();
    let mut packet = [0u8; 1500];
    h.socket.recv(&mut packet)?;
    let response = send_and_recv_v6(&h.socket, request)?.expect("handoff retry should complete");
    assert_eq!(DHCPv6Factory::ia_addr(&response), Some(address));
    h.wait_for_active_lease(address, &DHCPv6Factory::duid_hex(&new_duid))?;
    let old_renew = send_and_recv_v6(&h.socket, DHCPv6Factory::renew(idx, server_id, address))?;
    assert!(
        old_renew
            .as_ref()
            .and_then(DHCPv6Factory::ia_addr)
            .is_none()
    );
    assert!(
        h.active_lease(address, &DHCPv6Factory::duid_hex(&new_duid))?
            .is_some()
    );
    assert!(h.api_server.expired_leases().is_empty());
    Ok(())
}

#[test]
fn native_packet_exception_releases_transaction_guard() -> Result<(), eyre::Report> {
    let idx = 0x53;
    let h = Harness::new();
    assert!(send_and_recv_v6(&h.socket, DHCPv6Factory::request_without_server_id(idx))?.is_none());
    let discovery = &h.api_server.discoveries()[0];
    assert_eq!(discovery.mac_address, DHCPv6Factory::mac_string(idx));
    assert_eq!(
        discovery.message_kind,
        Some(rpc::MessageKind::V6Request as i32)
    );
    assert!(
        h.kea
            .wait_for_log("DHCP6_REQUIRED_OPTIONS_CHECK_FAIL", MEMFILE_TIMEOUT)
    );
    assert!(h.read_leases()?.is_empty());
    let offer = send_and_recv_v6(&h.socket, DHCPv6Factory::solicit(idx))?
        .expect("the same client must proceed after native validation fails");
    let address = DHCPv6Factory::mock_addr(idx);
    let reply = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::request(idx, DHCPv6Factory::server_id(&offer), address),
    )?
    .expect("the released guard must allow persistence");
    assert_eq!(DHCPv6Factory::ia_addr(&reply), Some(address));
    h.wait_for_active_lease(address, &DHCPv6Factory::duid_ll_hex(idx))?;
    Ok(())
}

#[test]
fn busy_reclamation_preserves_lease_and_counters_until_retry() -> Result<(), eyre::Report> {
    let idx = 0x52;
    let mut h = Harness::new_with_config(short_expiry_config());
    let (_, address) = h.establish_lease(idx)?;
    h.kea.restart()?;
    let (entered, resume) = h.api_server.pause_next_discovery();
    h.socket.send(&DHCPv6Factory::solicit(idx))?;
    h._rt
        .block_on(async { tokio::time::timeout(METRIC_TIMEOUT, entered).await })??;
    // The compensation sample proves reclamation actually ran while busy.
    let deadline = Instant::now() + EXPIRY_TIMEOUT;
    let stats = loop {
        let stats = h.kea.statistics()?;
        let assigned = stats["subnet[1].assigned-nas"].as_array().unwrap();
        if assigned[0][0] == 1 && assigned.iter().any(|sample| sample[0] == 2) {
            break stats;
        }
        assert!(
            Instant::now() < deadline,
            "Kea did not attempt busy reclamation"
        );
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(h.api_server.expired_leases().is_empty());
    assert!(
        h.active_lease(address, &DHCPv6Factory::duid_ll_hex(idx))?
            .is_some()
    );
    assert_assigned_leases(&stats, 1, 1);
    assert_eq!(stats["reclaimed-leases"][0][0], 0);
    resume.send(()).unwrap();
    let mut packet = [0u8; 1500];
    h.socket.recv(&mut packet)?;
    h.wait_for_scoped_expiry(address, &DHCPv6Factory::mac_string(idx));
    Ok(())
}

// Prove RENEW and REBIND independently refuse to migrate a persisted lease
// when a cache miss returns a different API-owned address.
#[test]
fn renew_and_rebind_fail_closed_on_address_migration() -> Result<(), eyre::Report> {
    let idx = 0x42;
    let mut h = Harness::new();
    let (server_id, original_addr) = h.establish_lease(idx)?;
    let duid = DHCPv6Factory::duid_ll_hex(idx);
    let migrated_addr = "2001:db8::ee42".parse::<Ipv6Addr>()?;
    h.api_server
        .set_address_override(&DHCPv6Factory::mac_string(idx), &migrated_addr.to_string());

    let discover_calls = h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP);

    for (exchange, packet, diagnostic) in [
        (
            "RENEW",
            DHCPv6Factory::renew(idx, server_id, original_addr),
            "lease6_renew: refusing to migrate existing lease",
        ),
        (
            "REBIND",
            DHCPv6Factory::rebind(idx, original_addr),
            "lease6_rebind: refusing to migrate existing lease",
        ),
    ] {
        h.kea.restart()?;
        h.wait_for_active_lease(original_addr, &duid)?;
        let calls_before_exchange = h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP);
        assert!(
            send_and_recv_v6(&h.socket, packet)?.is_none(),
            "{exchange} must not send a reply for a changed API address"
        );
        assert_eq!(
            h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
            calls_before_exchange + 1,
            "{exchange} should refresh the empty hook cache"
        );
        assert!(
            h.kea.wait_for_log(diagnostic, MEMFILE_TIMEOUT),
            "{exchange} should explain the fail-closed decision"
        );
        assert!(
            h.active_lease(original_addr, &duid)?.is_some(),
            "{exchange} should preserve the existing lease"
        );
        assert!(
            h.active_lease(migrated_addr, &duid)?.is_none(),
            "{exchange} must not persist the changed API address"
        );
    }
    assert_eq!(
        h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
        discover_calls + 2,
        "RENEW and REBIND should each refresh an empty hook cache"
    );
    Ok(())
}

// Prove CONFIRM consults the identity-wide cache without another Core request
// and reports whether each proposed address belongs to the cached prefix.
#[test]
fn confirm_is_cache_only_and_requires_the_cached_prefix() -> Result<(), eyre::Report> {
    let idx = 0x43;
    let h = Harness::new();
    // Seed a vendor-specific cache entry, then deliberately omit vendor class
    // from CONFIRM to exercise the identity-only fallback.
    let advertise = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::solicit_with_vendor_class(idx, b"HTTPClient"),
    )?
    .expect("Kea did not respond to vendor-class SOLICIT");
    let address = DHCPv6Factory::mock_addr(idx);
    assert_eq!(DHCPv6Factory::ia_addr(&advertise), Some(address));
    let discover_calls = h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP);

    let cases = [
        (
            "cached on-prefix address",
            idx,
            0x70,
            address,
            Status::Success,
        ),
        (
            "cached identity with off-prefix address",
            idx,
            0x71,
            "2001:db9::1".parse()?,
            Status::NotOnLink,
        ),
        (
            "uncached identity",
            idx + 1,
            0x72,
            DHCPv6Factory::mock_addr(idx + 1),
            Status::NotOnLink,
        ),
    ];
    for (case, client_idx, transaction_id, confirmed_addr, expected_status) in cases {
        let response = send_and_recv_v6(
            &h.socket,
            DHCPv6Factory::confirm(client_idx, transaction_id, confirmed_addr),
        )?
        .unwrap_or_else(|| panic!("Kea did not respond to CONFIRM case {case}"));
        assert_eq!(response.msg_type(), v6::MessageType::Reply, "{case}");
        assert_status(&response, expected_status);
    }
    assert_eq!(
        h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
        discover_calls,
        "CONFIRM must resolve exclusively from the lease cache"
    );

    Ok(())
}

#[test]
fn unrelated_cached_renew_stays_available_during_discovery() -> Result<(), eyre::Report> {
    let cached = 0x61;
    let uncached = 0x62;
    let h = Harness::new();
    let (server_id, address) = h.establish_lease(cached)?;
    let calls_before = h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP);
    let (entered, resume) = h.api_server.pause_next_discovery();
    h.socket.send(&DHCPv6Factory::solicit_with_address_hint(
        uncached,
        address,
        DHCPv6Factory::duid_ll(uncached),
    ))?;
    h._rt
        .block_on(async { tokio::time::timeout(METRIC_TIMEOUT, entered).await })??;

    let renewal = DHCPv6Factory::renew(cached, server_id, address);
    let response = send_and_recv_v6(&h.socket, renewal.clone());
    let busy_drops = v6_drop_metric_value(h.kea.metrics_endpoint(), "lease_transaction_busy");
    // Release the mock even when the availability check failed.
    drop(resume);
    let mut packet = [0u8; 1500];
    h.socket.recv(&mut packet)?;
    let retry = send_and_recv_v6(&h.socket, renewal)?;
    assert_eq!(
        retry.as_ref().and_then(DHCPv6Factory::ia_addr),
        Some(address)
    );
    assert_eq!(
        h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP),
        calls_before + 1
    );
    assert!(
        h.active_lease(address, &DHCPv6Factory::duid_ll_hex(cached))?
            .is_some()
    );
    assert_eq!(
        response?.as_ref().and_then(DHCPv6Factory::ia_addr),
        Some(address),
        "an unrelated slow discovery must not block a cached renewal; busy drops: {busy_drops}"
    );
    Ok(())
}

#[test]
fn stale_lease_end_messages_preserve_handoff_owner() -> Result<(), eyre::Report> {
    let idx = 0x63;
    let h = Harness::new();
    let (server_id, address) = h.establish_lease(idx)?;
    let new_duid = DHCPv6Factory::duid_en(20);
    let response = send_and_recv_v6(
        &h.socket,
        DHCPv6Factory::request_with_duid(idx, server_id.clone(), address, new_duid.clone(), true),
    )?;
    assert_eq!(
        response.as_ref().and_then(DHCPv6Factory::ia_addr),
        Some(address)
    );
    h.wait_for_active_lease(address, &DHCPv6Factory::duid_hex(&new_duid))?;

    for (case, packet) in [
        (
            "old release",
            DHCPv6Factory::release_with_hop_count(idx, server_id.clone(), address, 0),
        ),
        (
            "old decline",
            DHCPv6Factory::decline_with_hop_count(idx, server_id, address, 0),
        ),
    ] {
        let response = send_and_recv_v6(&h.socket, packet)?;
        assert!(response.is_some(), "{case} must receive a protocol reply");
        assert!(
            h.active_lease(address, &DHCPv6Factory::duid_hex(&new_duid))?
                .is_some(),
            "{case}"
        );
        assert!(h.api_server.expired_leases().is_empty(), "{case}");
        assert_eq!(
            h.kea.statistics()?["subnet[1].assigned-nas"][0][0],
            1,
            "{case}"
        );
    }
    Ok(())
}

#[test]
fn handoff_outside_dummy_pool_preserves_assigned_count() -> Result<(), eyre::Report> {
    let idx = 0x64;
    let h = Harness::new();
    let address = "2001:db8:1::64".parse::<Ipv6Addr>()?;
    h.api_server
        .set_address_override(&DHCPv6Factory::mac_string(idx), &address.to_string());
    for duid in [DHCPv6Factory::duid_uuid(idx), DHCPv6Factory::duid_en(21)] {
        let advertise = send_and_recv_v6(
            &h.socket,
            DHCPv6Factory::solicit_with_duid(idx, duid.clone(), true),
        )?
        .expect("boot-stage solicit must produce an advertisement");
        assert_eq!(DHCPv6Factory::ia_addr(&advertise), Some(address));
        let response = send_and_recv_v6(
            &h.socket,
            DHCPv6Factory::request_with_duid(
                idx,
                DHCPv6Factory::server_id(&advertise),
                address,
                duid.clone(),
                true,
            ),
        )?;
        assert_eq!(
            response.as_ref().and_then(DHCPv6Factory::ia_addr),
            Some(address)
        );
        h.wait_for_active_lease(address, &DHCPv6Factory::duid_hex(&duid))?;
        assert_assigned_leases(&h.kea.statistics()?, 1, 0);
        assert!(h.api_server.expired_leases().is_empty());
    }
    Ok(())
}

// Exercise both native reuse entry points, including the UPDATE performed by
// REQUEST after the hook redirects an expired allocator candidate.
#[test]
fn expired_candidate_redirects_to_authoritative_address() -> Result<(), eyre::Report> {
    const HEADER: &str = "address,duid,valid_lifetime,expire,subnet_id,pref_lifetime,lease_type,iaid,prefix_len,fqdn_fwd,fqdn_rev,hostname,hwaddr,state,user_context,hwtype,hwaddr_source";
    for (case, hint, target_exists, collision) in [
        ("iterative candidate, absent target", false, false, false),
        ("explicit hint, same owner target", true, true, false),
        ("explicit hint, foreign owner refusal", true, true, true),
        ("iterative candidate refusal", false, true, true),
    ] {
        let idx = 0x65;
        let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
        let old_duid = DHCPv6Factory::duid_ll_hex(1);
        let old_mac = DHCPv6Factory::mac_string(1);
        let candidate = "2001:db8::1".parse::<Ipv6Addr>()?;
        let address = DHCPv6Factory::mock_addr(idx);
        let mut rows = format!(
            "{HEADER}\n{candidate},{old_duid},600,{},1,300,0,1,128,0,0,,{old_mac},0,,1,1\n",
            now - 60
        );
        let owner = if collision { idx + 1 } else { idx };
        let owner_duid = DHCPv6Factory::duid_ll_hex(owner);
        if target_exists {
            rows.push_str(&format!(
                "{address},{owner_duid},7200,{},1,3600,0,1,128,0,0,,{},0,,1,1\n",
                now + 7200,
                DHCPv6Factory::mac_string(owner)
            ));
        }
        let mut h = Harness::new_with_leases(
            Kea6Config {
                address_pool: if hint {
                    "::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff"
                } else {
                    "2001:db8::1-2001:db8::ffff"
                },
                expired_leases_processing: Some(Kea6ExpiredLeasesProcessing {
                    reclaim_timer_wait_time: 3600,
                    flush_reclaimed_timer_wait_time: 3600,
                    hold_reclaimed_time: 0,
                    max_reclaim_leases: 10,
                    max_reclaim_time: 100,
                    unwarned_reclaim_cycles: 1,
                }),
                ..Kea6Config::default()
            },
            &rows,
        );
        let new_duid = DHCPv6Factory::duid_en(22);
        let solicit = if hint {
            DHCPv6Factory::solicit_with_address_hint(idx, candidate, new_duid.clone())
        } else {
            DHCPv6Factory::solicit_with_duid(idx, new_duid.clone(), true)
        };
        let offer = send_and_recv_v6(&h.socket, solicit)?.expect(case);
        assert_eq!(
            DHCPv6Factory::ia_addr(&offer),
            (!collision).then_some(address),
            "{case}"
        );
        assert_eq!(
            h.read_leases()?.len(),
            1 + usize::from(target_exists),
            "SOLICIT must not persist: {case}"
        );
        if !hint && !collision {
            // Reset the iterative allocator cursor advanced by SOLICIT so
            // REQUEST must encounter the persisted expired candidate again.
            h.kea.restart()?;
        }
        let request = if hint || collision {
            DHCPv6Factory::request_with_duid(
                idx,
                DHCPv6Factory::server_id(&offer),
                candidate,
                new_duid.clone(),
                true,
            )
        } else {
            DHCPv6Factory::request_without_address_hint(
                idx,
                DHCPv6Factory::server_id(&offer),
                new_duid.clone(),
            )
        };
        let response = send_and_recv_v6(&h.socket, request)?.expect(case);
        assert_eq!(
            DHCPv6Factory::ia_addr(&response),
            (!collision).then_some(address),
            "{case}"
        );
        if collision {
            let retry = send_and_recv_v6(
                &h.socket,
                DHCPv6Factory::request_with_duid(
                    idx,
                    DHCPv6Factory::server_id(&offer),
                    candidate,
                    new_duid.clone(),
                    true,
                ),
            )?
            .expect(case);
            assert!(DHCPv6Factory::ia_addr(&retry).is_none(), "{case}");
            assert!(h.active_lease(address, &owner_duid)?.is_some(), "{case}");
            assert_assigned_leases(&h.kea.statistics()?, 1, 1);
            assert_eq!(
                h.api_server.expired_leases().len(),
                1,
                "repeated refusal must not reclaim twice: {case}"
            );
        } else {
            h.wait_for_active_lease(address, &DHCPv6Factory::duid_hex(&new_duid))?;
            assert!(
                !h.read_leases()?
                    .iter()
                    .any(|lease| lease.address == candidate && lease.valid_lifetime > 0),
                "expired candidate must be retired: {case}"
            );
            assert_assigned_leases(&h.kea.statistics()?, 1, 1);
        }
        // An allocation refusal must leave the daemon serving other clients.
        h.establish_lease(idx + 2)?;
    }
    Ok(())
}
