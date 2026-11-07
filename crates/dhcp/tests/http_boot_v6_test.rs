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
use std::fs;
use std::net::{Ipv6Addr, UdpSocket};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases};
use dhcp::mock_api_server::{DHCP_RESPONSE_FQDN, ENDPOINT_DISCOVER_DHCP, MockAPIServer};
use dhcproto::v6::{
    Architecture, DhcpOption, Message, MessageType, OptionCode, UnknownOption, VendorClass,
};
use eyre::WrapErr;
use rpc::forge as rpc;

mod common;

use common::{
    DHCPv6Factory, Kea6, Kea6Config, assert_api_v6_domain_search, assert_hook_v6_dns_servers,
    assert_hook_v6_ntp_servers, send_and_recv_v6,
};

const ENTERPRISE_NUMBER: u32 = 32_473;
const X64_URL: &str = "http://[2001:db8::10]:8080/public/blobs/internal/x86_64/ipxe.efi";
const ARM_URL: &str = "http://[2001:db8::10]:8080/public/blobs/internal/aarch64/ipxe.efi";
const API_URL: &str = "https://boot.example.test/custom.efi";
const BOOT_REQUESTS: &[OptionCode] = &[OptionCode::OptBootfileUrl, OptionCode::VendorClass];

/// Keep each daemon, API fixture, and lease file alive for one packet sequence.
struct Harness {
    _kea: Kea6,
    api_server: MockAPIServer,
    socket: UdpSocket,
    lease_path: PathBuf,
    _lease_dir: tempfile::TempDir,
    _runtime: tokio::runtime::Runtime,
}

impl Harness {
    /// Enable generated boot URLs with a private lease file and dynamic ports.
    fn start() -> Result<Self, eyre::Report> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let api_server = runtime.block_on(MockAPIServer::start());
        let lease_dir = tempfile::tempdir()?;
        let lease_path = lease_dir.path().join("kea-leases6.csv");
        let (kea, socket) = Kea6::start_with_config(
            api_server.local_http_addr(),
            Some(&lease_path),
            Kea6Config {
                provisioning_server_ipv6: "2001:db8::10",
                ..Kea6Config::default()
            },
        )?;
        socket.set_read_timeout(Some(Duration::from_secs(2)))?;
        Ok(Self {
            _kea: kea,
            api_server,
            socket,
            lease_path,
            _lease_dir: lease_dir,
            _runtime: runtime,
        })
    }

    /// A missing reply is a test failure, not evidence that options were omitted.
    fn exchange(&self, packet: Vec<u8>) -> Result<Message, eyre::Report> {
        send_and_recv_v6(&self.socket, packet)?
            .ok_or_else(|| eyre::eyre!("kea did not respond to HTTP boot request"))
    }

    /// Missing or malformed memfiles cannot prove that no lease was allocated.
    /// Keep the latest row per address because Kea appends lease updates.
    fn active_leases(&self) -> Result<BTreeMap<Ipv6Addr, String>, eyre::Report> {
        let contents = fs::read_to_string(&self.lease_path).wrap_err_with(|| {
            format!("failed to read kea memfile {}", self.lease_path.display())
        })?;
        let mut lines = contents.lines();
        eyre::ensure!(
            lines
                .next()
                .is_some_and(|header| header.starts_with("address,duid,valid_lifetime,")),
            "missing kea lease header in {}",
            self.lease_path.display()
        );
        let mut active = BTreeMap::new();
        for (row_index, line) in lines.enumerate() {
            let columns = line.split(',').collect::<Vec<_>>();
            eyre::ensure!(
                columns.len() >= 14,
                "malformed kea lease row {}: {line}",
                row_index + 2
            );
            let address = columns[0]
                .parse::<Ipv6Addr>()
                .wrap_err("invalid lease address")?;
            let lifetime = columns[2]
                .parse::<u32>()
                .wrap_err("invalid lease lifetime")?;
            let lease_type = columns[6].parse::<u32>().wrap_err("invalid lease type")?;
            let state = columns[13].parse::<u32>().wrap_err("invalid lease state")?;
            let duid = columns[1].replace([':', '-'], "").to_ascii_lowercase();
            eyre::ensure!(
                !duid.is_empty()
                    && duid.len().is_multiple_of(2)
                    && duid.bytes().all(|byte| byte.is_ascii_hexdigit()),
                "invalid lease DUID on row {}: {line}",
                row_index + 2
            );
            if lifetime > 0 && lease_type == 0 && state == 0 {
                active.insert(address, duid);
            } else {
                active.remove(&address);
            }
        }
        Ok(active)
    }

    /// Observe the committed identity instead of assuming a REPLY implies a save.
    fn wait_for_committed_lease(&self, idx: u8) -> Result<(), eyre::Report> {
        let address = DHCPv6Factory::mock_addr(idx);
        let duid = DHCPv6Factory::duid_ll_hex(idx);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.active_leases() {
                Ok(leases) if leases.get(&address) == Some(&duid) => return Ok(()),
                Ok(leases) if Instant::now() >= deadline => {
                    eyre::bail!("kea did not commit lease {address}/{duid}: {leases:?}");
                }
                Err(error) if Instant::now() >= deadline => return Err(error),
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

/// Use one enterprise across exchanges so cache tests change only boot inputs.
fn http_vendor_class() -> VendorClass {
    VendorClass {
        num: ENTERPRISE_NUMBER,
        data: vec![b"HTTPClient".to_vec()],
    }
}

/// Compare all copies, not just the first option, so duplicate FFI inserts fail.
fn boot_options(response: &Message) -> Vec<DhcpOption> {
    [
        OptionCode::VendorClass,
        OptionCode::OptBootfileUrl,
        OptionCode::ClientArchType,
    ]
    .into_iter()
    .flat_map(|code| {
        response
            .opts()
            .get_all(code)
            .unwrap_or_default()
            .iter()
            .cloned()
    })
    .collect()
}

/// API URLs omit architecture; generated defaults identify the selected image.
fn expected_boot_options(url: &str, architecture: Option<Architecture>) -> Vec<DhcpOption> {
    let mut options = vec![
        DhcpOption::VendorClass(http_vendor_class()),
        DhcpOption::Unknown(UnknownOption::new(
            OptionCode::OptBootfileUrl,
            url.as_bytes().to_vec(),
        )),
    ];
    if let Some(architecture) = architecture {
        options.push(DhcpOption::ClientArchType(vec![architecture]));
    }
    options
}

/// Adding boot options must preserve the existing DNS, NTP, and FQDN response.
fn assert_service_options(response: &Message) {
    assert_hook_v6_dns_servers(response);
    assert_api_v6_domain_search(response);
    assert_hook_v6_ntp_servers(response);
    let mut fqdn = vec![DHCPv6Factory::client_fqdn_payload()[0]];
    for label in DHCP_RESPONSE_FQDN.split('.') {
        fqdn.push(label.len() as u8);
        fqdn.extend_from_slice(label.as_bytes());
    }
    fqdn.push(0);
    assert_eq!(
        response
            .opts()
            .get_all(OptionCode::ClientFqdn)
            .unwrap_or_default(),
        &[DhcpOption::Unknown(UnknownOption::new(
            OptionCode::ClientFqdn,
            fqdn
        ))]
    );
}

/// Boot options must survive lease commit and follow the current packet even
/// when Core's address record is served from the existing lease cache.
#[test]
fn http_boot_lease_commit_and_cache_use_current_client_options() -> Result<(), eyre::Report> {
    let h = Harness::start()?;
    let idx = 0x60;
    let address = DHCPv6Factory::mock_addr(idx);
    let advertise = h.exchange(DHCPv6Factory::solicit_with_boot_options(
        idx,
        http_vendor_class(),
        &[Architecture::X64_Http],
        BOOT_REQUESTS,
    ))?;
    assert_eq!(advertise.msg_type(), MessageType::Advertise);
    assert_eq!(DHCPv6Factory::ia_addr(&advertise), Some(address));
    assert_eq!(
        boot_options(&advertise),
        expected_boot_options(X64_URL, Some(Architecture::X64_Http))
    );
    assert_service_options(&advertise);
    assert!(
        h.active_leases()?.is_empty(),
        "SOLICIT must not commit a lease"
    );
    assert_eq!(h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP), 1);
    let server_id = DHCPv6Factory::server_id(&advertise);

    let reply = h.exchange(DHCPv6Factory::request_with_boot_options(
        idx,
        server_id.clone(),
        address,
        http_vendor_class(),
        &[Architecture::X64_Http],
        BOOT_REQUESTS,
    ))?;
    assert_eq!(reply.msg_type(), MessageType::Reply);
    assert_eq!(DHCPv6Factory::ia_addr(&reply), Some(address));
    assert_eq!(
        boot_options(&reply),
        expected_boot_options(X64_URL, Some(Architecture::X64_Http))
    );
    assert_service_options(&reply);
    h.wait_for_committed_lease(idx)?;

    // Keep identity and vendor class fixed so only per-request boot inputs vary.
    check_cases(
        [
            Case {
                scenario: "cached address uses current ARM architecture",
                input: (Architecture::Arm64_Http, BOOT_REQUESTS),
                expect: Yields(expected_boot_options(
                    ARM_URL,
                    Some(Architecture::Arm64_Http),
                )),
            },
            Case {
                scenario: "cached address does not reuse an earlier ORO",
                input: (Architecture::Arm64_Http, &[OptionCode::VendorClass][..]),
                expect: Yields(vec![]),
            },
        ],
        |(architecture, requested)| -> Result<Vec<DhcpOption>, String> {
            let response = h
                .exchange(DHCPv6Factory::request_with_boot_options(
                    idx,
                    server_id.clone(),
                    address,
                    http_vendor_class(),
                    &[architecture],
                    requested,
                ))
                .map_err(|error| format!("{error:#}"))?;
            assert_eq!(response.msg_type(), MessageType::Reply);
            assert_eq!(DHCPv6Factory::ia_addr(&response), Some(address));
            assert_service_options(&response);
            assert_eq!(h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP), 1);
            Ok(boot_options(&response))
        },
    );
    assert_eq!(
        h.active_leases()?.get(&address),
        Some(&DHCPv6Factory::duid_ll_hex(idx))
    );
    Ok(())
}

/// An INFORMATION-REQUEST has no address binding to validate, but still needs
/// boot options. Reading the memfile must succeed before absence proves anything.
#[test]
fn information_request_returns_boot_options_without_committing_a_lease() -> Result<(), eyre::Report>
{
    let h = Harness::start()?;
    assert!(h.active_leases()?.is_empty());
    let response = h.exchange(DHCPv6Factory::information_request_with_boot_options(
        0x61,
        http_vendor_class(),
        &[Architecture::Arm64_Http],
        BOOT_REQUESTS,
    ))?;
    assert_eq!(response.msg_type(), MessageType::Reply);
    assert!(response.opts().get_all(OptionCode::IANA).is_none());
    assert_eq!(
        boot_options(&response),
        expected_boot_options(ARM_URL, Some(Architecture::Arm64_Http))
    );
    assert_service_options(&response);
    assert_eq!(
        h.api_server.discoveries()[0].message_kind,
        Some(rpc::MessageKind::V6InfoRequest as i32)
    );
    assert!(
        h.active_leases()?.is_empty(),
        "options-only reply committed a lease"
    );
    Ok(())
}

/// Core's refusal to allocate an address must also suppress its boot URL;
/// otherwise firmware receives a boot offer for a stateful request we rejected.
#[test]
fn refused_address_does_not_offer_the_api_boot_url() -> Result<(), eyre::Report> {
    let h = Harness::start()?;
    let idx = 0x62;
    let mac = DHCPv6Factory::mac_string(idx);
    h.api_server.set_address_override(&mac, "");
    h.api_server.set_booturl_override(&mac, API_URL);
    let response = h.exchange(DHCPv6Factory::solicit_with_boot_options(
        idx,
        http_vendor_class(),
        &[Architecture::X64_Http],
        BOOT_REQUESTS,
    ))?;
    assert_eq!(response.msg_type(), MessageType::Advertise);
    assert_eq!(DHCPv6Factory::ia_addr(&response), None);
    assert!(boot_options(&response).is_empty());
    assert_eq!(h.api_server.calls_for(ENDPOINT_DISCOVER_DHCP), 1);
    assert!(
        h.active_leases()?.is_empty(),
        "refused request committed a lease"
    );
    Ok(())
}

/// Verify that Core's optional URL reaches the loaded hook without losing
/// empty-string suppression at the protobuf, cache, or FFI boundaries.
#[test]
fn api_boot_url_override_and_empty_suppression_reach_the_wire() -> Result<(), eyre::Report> {
    let h = Harness::start()?;
    check_cases(
        [
            Case {
                scenario: "Core URL replaces an eligible default",
                input: (0x63, API_URL),
                expect: Yields(expected_boot_options(API_URL, None)),
            },
            Case {
                scenario: "Core empty URL disables an eligible default",
                input: (0x64, ""),
                expect: Yields(vec![]),
            },
        ],
        |(idx, url)| -> Result<Vec<DhcpOption>, String> {
            h.api_server
                .set_booturl_override(&DHCPv6Factory::mac_string(idx), url);
            let response = h
                .exchange(DHCPv6Factory::solicit_with_boot_options(
                    idx,
                    http_vendor_class(),
                    &[Architecture::X64_Http],
                    BOOT_REQUESTS,
                ))
                .map_err(|error| format!("{error:#}"))?;
            assert_eq!(response.msg_type(), MessageType::Advertise);
            assert_eq!(
                DHCPv6Factory::ia_addr(&response),
                Some(DHCPv6Factory::mock_addr(idx))
            );
            Ok(boot_options(&response))
        },
    );
    Ok(())
}
