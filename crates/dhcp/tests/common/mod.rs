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
#[allow(dead_code)]
mod dhcp_factory;
#[allow(dead_code)]
mod dhcpv6_factory;
#[allow(dead_code)]
mod kea;
#[allow(dead_code)]
mod kea_v6;

use std::io::{Read, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpStream, UdpSocket};
use std::time::{Duration, Instant};

use dhcp::mock_api_server::DHCP_RESPONSE_FQDN;
#[allow(unused_imports)]
pub(super) use dhcp_factory::DHCPFactory;
use dhcproto::v6::{DhcpOption, Message, NtpSuboption, OptionCode};
#[allow(unused_imports)]
pub(super) use dhcpv6_factory::DHCPv6Factory;
#[allow(unused_imports)]
pub(super) use kea::Kea;
use kea_v6::{HOOK_DNS_SERVERS_IPV6, HOOK_NTP_SERVERS_IPV6};
#[allow(unused_imports)]
pub(super) use kea_v6::{Kea6, Kea6Config, Kea6ExpiredLeasesProcessing};

#[allow(dead_code)]
const METRICS_READ_TIMEOUT: Duration = Duration::from_millis(500);

// Send one relayed DHCPv6 packet, returning None only for a timeout or
// WouldBlock while preserving transport and decoding failures as errors.
pub(super) fn send_and_recv_v6(
    socket: &UdpSocket,
    packet: Vec<u8>,
) -> Result<Option<Message>, eyre::Report> {
    socket.send(&packet)?;
    let mut buf = [0u8; 1500];
    let n = match socket.recv(&mut buf) {
        Ok(n) => n,
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(Some(DHCPv6Factory::decode_reply(&buf[..n])?))
}

/// Return the Kea hook built by the same Cargo invocation as this integration test.
fn hook_library_path() -> String {
    let test_executable =
        std::env::current_exe().expect("cannot locate integration test executable");
    let deps_dir = test_executable
        .parent()
        .expect("integration test executable has no parent directory");
    let library_name = format!(
        "{}dhcp{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    );
    let hook_library = deps_dir.join(library_name);

    assert!(
        hook_library.is_file(),
        "Kea hook library was not built next to the integration test executable: {}",
        hook_library.display()
    );

    hook_library.to_string_lossy().into_owned()
}

/// Return the DHCPv6 dropped-request counter value for a reason label.
#[allow(dead_code)]
pub(super) fn v6_drop_metric_value(endpoint: SocketAddr, reason: &str) -> f64 {
    labeled_counter_value(
        endpoint,
        "carbide_dhcp_v6_requests_dropped_total",
        "reason",
        reason,
    )
}

/// Wait until the DHCPv6 dropped-request counter reaches the expected value.
#[allow(dead_code)]
pub(super) fn wait_for_v6_drop_metric_at_least(
    endpoint: SocketAddr,
    reason: &str,
    minimum: f64,
    timeout: Duration,
) -> bool {
    wait_for_labeled_counter_at_least(
        endpoint,
        "carbide_dhcp_v6_requests_dropped_total",
        "reason",
        reason,
        minimum,
        timeout,
    )
}

/// Scrape the Prometheus text endpoint from a Kea child process.
#[allow(dead_code)]
fn scrape_metrics(endpoint: SocketAddr) -> Option<String> {
    let mut stream = TcpStream::connect_timeout(&endpoint, METRICS_READ_TIMEOUT).ok()?;
    stream.set_read_timeout(Some(METRICS_READ_TIMEOUT)).ok()?;
    stream
        .write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")
        .ok()?;

    let mut response = String::new();
    stream.read_to_string(&mut response).ok()?;
    Some(response)
}

// Return one labeled counter value from the Prometheus endpoint, or zero when
// the scrape fails or contains no parseable matching sample.
pub(super) fn labeled_counter_value(
    endpoint: SocketAddr,
    metric_name: &str,
    label_name: &str,
    label_value: &str,
) -> f64 {
    let Some(metrics) = scrape_metrics(endpoint) else {
        return 0.0;
    };
    let prefix = format!("{metric_name}{{{label_name}=\"{label_value}\"}} ");
    metrics
        .lines()
        .filter_map(|line| line.strip_prefix(&prefix))
        .filter_map(|value| value.split_whitespace().next())
        .filter_map(|value| value.parse::<f64>().ok())
        .sum()
}

// Wait until one labeled Prometheus counter reaches the expected value.
pub(super) fn wait_for_labeled_counter_at_least(
    endpoint: SocketAddr,
    metric_name: &str,
    label_name: &str,
    label_value: &str,
    minimum: f64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if labeled_counter_value(endpoint, metric_name, label_name, label_value) >= minimum {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Assert that the DHCPv6 response carries hook-configured DNS servers.
#[allow(dead_code)]
pub(super) fn assert_hook_v6_dns_servers(response: &Message) {
    // Keep the expected value tied to the Kea test config, not mock API data.
    let expected = HOOK_DNS_SERVERS_IPV6
        .into_iter()
        .map(|server| server.parse::<Ipv6Addr>().unwrap())
        .collect::<Vec<_>>();

    match response.opts().get(OptionCode::DomainNameServers) {
        Some(DhcpOption::DomainNameServers(servers)) => assert_eq!(servers, &expected),
        other => panic!("DHCPv6 response did not include DNS servers: {other:?}"),
    }
}

/// Assert that the DHCPv6 response carries the API-owned parent domain.
#[allow(dead_code)]
pub(super) fn assert_api_v6_domain_search(response: &Message) {
    // DHCPv6 domain-search uses the trusted parent domain from the API FQDN.
    let expected = DHCP_RESPONSE_FQDN
        .split_once('.')
        .map(|(_, domain)| format!("{domain}."))
        .expect("mock DHCP FQDN must include a parent domain");

    let actual = match response.opts().get(OptionCode::DomainSearchList) {
        Some(DhcpOption::DomainSearchList(names)) => {
            names.iter().map(|name| name.to_ascii()).collect::<Vec<_>>()
        }
        other => panic!("DHCPv6 response did not include domain search: {other:?}"),
    };
    assert_eq!(actual, vec![expected]);
}

/// Assert that the DHCPv6 response carries hook-configured NTP servers.
#[allow(dead_code)]
pub(super) fn assert_hook_v6_ntp_servers(response: &Message) {
    // Keep the expected value tied to the Kea test config, not API mock data.
    let expected = HOOK_NTP_SERVERS_IPV6
        .into_iter()
        .map(|server| server.parse::<Ipv6Addr>().unwrap())
        .collect::<Vec<_>>();

    assert_eq!(ntp_server_addresses(response), expected);
}

/// Return IPv6 NTP server addresses from a DHCPv6 option 56 response.
#[allow(dead_code)]
fn ntp_server_addresses(response: &Message) -> Vec<Ipv6Addr> {
    match response.opts().get(OptionCode::NtpServer) {
        Some(DhcpOption::NtpServer(suboptions)) => suboptions
            .iter()
            .filter_map(|option| match option {
                NtpSuboption::ServerAddress(address) | NtpSuboption::MulticastAddress(address) => {
                    Some(*address)
                }
                NtpSuboption::FQDN(_) => None,
            })
            .collect(),
        other => panic!("DHCPv6 response did not include NTP server addresses: {other:?}"),
    }
}
