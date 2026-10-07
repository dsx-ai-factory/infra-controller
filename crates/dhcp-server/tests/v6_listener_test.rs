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
use std::net::{Ipv6Addr, SocketAddr, SocketAddrV6};
use std::sync::Arc;
use std::time::Duration;

use carbide_dhcp_server::modes::dpu::Dpu;
use carbide_dhcp_server::packet_handler_v6::process_packet;
use carbide_dhcp_server::util::get_socket_v6;
use carbide_rpc_utils::dhcp::{InterfaceInfo, InterfaceInfoV6};
use carbide_test_support::Outcome::Yields;
use carbide_test_support::{Case, check_cases_async};
use dhcproto::v6::{
    Architecture, DhcpOption, IANA, MessageType, NtpSuboption, ORO, OptionCode, Status,
    UnknownOption, VendorClass,
};
use tokio::net::UdpSocket;
use tokio::time::timeout;

mod common;

use common::{
    DUID_LL, IAID, SERVER_IDENTIFIER, assert_ia_na_failure, base_dhcp_config, client_message,
    decode_message, decode_response, dpu_config, dpu_config_with_options, encode, machine_cache,
    response_ia_na, response_status,
};

const INTERFACE: &str = "eth0";

#[tokio::test]
async fn boot_options_follow_configuration_and_client_requests() {
    const BOOT_URL: &str = "http://[2001:db8::10]/bootx64.efi";
    const HTTPS_URL: &str = "https://[2001:db8::10]/bootx64.efi";
    const TFTP_URL: &str = "tftp://[2001:db8::10]/bootx64.efi";
    const HOSTNAME_URL: &str = "http://boot.example/bootx64.efi";
    const IPV4_URL: &str = "http://192.0.2.10/bootx64.efi";
    const DEFAULT_X64_URL: &str =
        "http://[2001:db8::10]:8080/public/blobs/internal/x86_64/ipxe.efi";
    const DEFAULT_ARM64_URL: &str =
        "http://[2001:db8::10]:8080/public/blobs/internal/aarch64/ipxe.efi";
    const HTTP_CLIENT: &str = "HTTPClient:Arch:00016:UNDI:003016";

    let boot_options = || Some(vec![OptionCode::VendorClass, OptionCode::OptBootfileUrl]);
    let expected_url = || Some(BOOT_URL.as_bytes().to_vec());
    let expected_vendor = |num| {
        Some(VendorClass {
            num,
            data: vec![b"HTTPClient".to_vec()],
        })
    };

    struct Input {
        message_type: MessageType,
        boot_url: Option<&'static str>,
        requested_options: Option<Vec<OptionCode>>,
        vendor_classes: &'static [(u32, &'static [&'static str])],
        has_ipv6_dns: bool,
        provisioning_address: Option<&'static str>,
        architectures: &'static [Architecture],
        has_address: bool,
    }

    impl Default for Input {
        fn default() -> Self {
            Self {
                message_type: MessageType::Solicit,
                boot_url: None,
                requested_options: Some(vec![OptionCode::VendorClass, OptionCode::OptBootfileUrl]),
                vendor_classes: &[(343, &[HTTP_CLIENT])],
                has_ipv6_dns: false,
                provisioning_address: Some("2001:db8::10"),
                architectures: &[Architecture::X64_Http],
                has_address: true,
            }
        }
    }

    check_cases_async(
        [
            Case {
                scenario: "explicit URL works without default URL prerequisites",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(BOOT_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[(343, &["Other", HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    architectures: &[],
                    provisioning_address: None,
                    ..Input::default()
                },
                expect: Yields((expected_url(), expected_vendor(343), None)),
            },
            Case {
                scenario: "stateful reply preserves enterprise number for bare HTTPClient",
                input: Input {
                    message_type: MessageType::Request,
                    boot_url: Some(HTTPS_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[(5703, &["HTTPClient"])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((
                    Some(HTTPS_URL.as_bytes().to_vec()),
                    expected_vendor(5703),
                    None,
                )),
            },
            Case {
                scenario: "stateless reply identifies HTTP boot",
                input: Input {
                    message_type: MessageType::InformationRequest,
                    boot_url: Some(BOOT_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((expected_url(), expected_vendor(343), None)),
            },
            Case {
                scenario: "generate default x86_64 URL when no override is configured",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: None,
                    requested_options: boot_options(),
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    architectures: &[Architecture::X64_Http, Architecture::Arm64_Http],
                    ..Input::default()
                },
                expect: Yields((
                    Some(DEFAULT_X64_URL.as_bytes().to_vec()),
                    expected_vendor(343),
                    Some(vec![Architecture::X64_Http]),
                )),
            },
            Case {
                scenario: "choose first supported architecture in client preference order",
                input: Input {
                    architectures: &[
                        Architecture::X64,
                        Architecture::Arm64_Http,
                        Architecture::X64_Http,
                    ],
                    ..Input::default()
                },
                expect: Yields((
                    Some(DEFAULT_ARM64_URL.as_bytes().to_vec()),
                    expected_vendor(343),
                    Some(vec![Architecture::Arm64_Http]),
                )),
            },
            Case {
                scenario: "stateless request receives default boot URL",
                input: Input {
                    message_type: MessageType::InformationRequest,
                    ..Input::default()
                },
                expect: Yields((
                    Some(DEFAULT_X64_URL.as_bytes().to_vec()),
                    expected_vendor(343),
                    Some(vec![Architecture::X64_Http]),
                )),
            },
            Case {
                scenario: "legacy config without IPv6 provisioning address",
                input: Input {
                    provisioning_address: None,
                    ..Input::default()
                },
                expect: Yields((None, None, None)),
            },
            Case {
                scenario: "vendor string architecture cannot replace the architecture option",
                input: Input {
                    architectures: &[],
                    ..Input::default()
                },
                expect: Yields((None, None, None)),
            },
            Case {
                scenario: "no supported HTTP boot architecture",
                input: Input {
                    architectures: &[Architecture::X64],
                    ..Input::default()
                },
                expect: Yields((None, None, None)),
            },
            Case {
                scenario: "default URL requires HTTPClient vendor class",
                input: Input {
                    vendor_classes: &[(343, &["HTTPClientOther"])],
                    ..Input::default()
                },
                expect: Yields((None, None, None)),
            },
            Case {
                scenario: "no address available does not advertise default boot URL",
                input: Input {
                    has_address: false,
                    ..Input::default()
                },
                expect: Yields((None, None, None)),
            },
            Case {
                scenario: "empty configured URL",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(""),
                    requested_options: boot_options(),
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((None, None, None)),
            },
            Case {
                scenario: "no option request",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(BOOT_URL),
                    requested_options: None,
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((None, None, None)),
            },
            Case {
                scenario: "vendor class requested without boot URL",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(BOOT_URL),
                    requested_options: Some(vec![OptionCode::VendorClass]),
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((None, None, None)),
            },
            Case {
                scenario: "boot URL requested without vendor class",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(BOOT_URL),
                    requested_options: Some(vec![OptionCode::OptBootfileUrl]),
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((expected_url(), None, None)),
            },
            Case {
                scenario: "client has no vendor class",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(BOOT_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((expected_url(), None, None)),
            },
            Case {
                scenario: "lookalike vendor class is not HTTPClient",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(BOOT_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[(343, &["HTTPClientOther"])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((expected_url(), None, None)),
            },
            Case {
                scenario: "HTTPClient follows another enterprise's vendor class",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(BOOT_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[(5703, &["Other"]), (343, &[HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((expected_url(), expected_vendor(343), None)),
            },
            Case {
                scenario: "hostname URL without IPv6 DNS leaves HTTP boot identification unchanged",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(HOSTNAME_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((Some(HOSTNAME_URL.as_bytes().to_vec()), None, None)),
            },
            Case {
                scenario: "hostname URL with IPv6 DNS identifies HTTP boot",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(HOSTNAME_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: true,
                    ..Input::default()
                },
                expect: Yields((
                    Some(HOSTNAME_URL.as_bytes().to_vec()),
                    expected_vendor(343),
                    None,
                )),
            },
            Case {
                scenario: "non-HTTP URL is not identified as HTTP boot even with IPv6 DNS",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(TFTP_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: true,
                    ..Input::default()
                },
                expect: Yields((Some(TFTP_URL.as_bytes().to_vec()), None, None)),
            },
            Case {
                scenario: "IPv4 URL does not qualify as an IPv6 literal without DNS",
                input: Input {
                    message_type: MessageType::Solicit,
                    boot_url: Some(IPV4_URL),
                    requested_options: boot_options(),
                    vendor_classes: &[(343, &[HTTP_CLIENT])],
                    has_ipv6_dns: false,
                    ..Input::default()
                },
                expect: Yields((Some(IPV4_URL.as_bytes().to_vec()), None, None)),
            },
        ],
        |input| async move {
            let mut dhcp_config = base_dhcp_config(None);
            dhcp_config.carbide_provisioning_server_ipv6 = input
                .provisioning_address
                .map(|address| address.parse().unwrap());
            if !input.has_ipv6_dns {
                dhcp_config.carbide_nameservers_v6.clear();
            }
            let config = dpu_config_with_options(
                BTreeMap::from([(
                    INTERFACE.to_string(),
                    InterfaceInfo {
                        address: Some("192.0.2.20".parse().unwrap()),
                        gateway: Some("192.0.2.1".parse().unwrap()),
                        prefix: Some("192.0.2.0/24".to_string()),
                        fqdn: "host.example.com".to_string(),
                        booturl: input.boot_url.map(str::to_owned),
                        mtu: Some(9000),
                        ipv6: Some(InterfaceInfoV6 {
                            address: (input.has_address
                                && input.message_type != MessageType::InformationRequest)
                                .then(|| "2001:db8::20".parse().unwrap()),
                            prefix: "2001:db8::/64".to_string(),
                        }),
                    },
                )]),
                dhcp_config,
            );
            let server_id =
                (input.message_type == MessageType::Request).then(|| SERVER_IDENTIFIER.to_vec());
            let mut request = client_message(input.message_type, DUID_LL, None, server_id);
            if !input.architectures.is_empty() {
                request
                    .opts_mut()
                    .insert(DhcpOption::ClientArchType(input.architectures.to_vec()));
            }
            if let Some(opts) = input.requested_options {
                request.opts_mut().insert(DhcpOption::ORO(ORO { opts }));
            }
            for (enterprise, classes) in input.vendor_classes.iter().rev() {
                // dhcproto 0.15's typed VendorClass encoder writes an incorrect option length.
                let mut data = enterprise.to_be_bytes().to_vec();
                for class in *classes {
                    data.extend_from_slice(&u16::try_from(class.len()).unwrap().to_be_bytes());
                    data.extend_from_slice(class.as_bytes());
                }
                request
                    .opts_mut()
                    .insert(DhcpOption::Unknown(UnknownOption::new(
                        OptionCode::VendorClass,
                        data,
                    )));
            }
            let mut cache = machine_cache();
            let packet = process_packet(
                &encode(&request),
                "fe80::20".parse().unwrap(),
                &config,
                INTERFACE,
                &Dpu {},
                &mut cache,
            )
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| "expected a DHCPv6 response".to_string())?;
            let response = decode_response(&packet);
            let boot_url = match response.opts().get(OptionCode::OptBootfileUrl) {
                Some(DhcpOption::Unknown(option)) => Some(option.data().to_vec()),
                None => None,
                other => panic!("expected raw boot-file URL option, got {other:?}"),
            };
            let vendor_class = match response.opts().get(OptionCode::VendorClass) {
                Some(DhcpOption::VendorClass(vendor)) => Some(vendor.clone()),
                None => None,
                other => panic!("expected decoded vendor-class option, got {other:?}"),
            };
            let architecture = match response.opts().get(OptionCode::ClientArchType) {
                Some(DhcpOption::ClientArchType(architectures)) => Some(architectures.clone()),
                None => None,
                other => panic!("expected architecture option, got {other:?}"),
            };
            Ok::<_, String>((boot_url, vendor_class, architecture))
        },
    )
    .await;
}

/// Verifies a stateful SOLICIT preserves IAID and returns the configured address and server ID.
#[tokio::test]
async fn stateful_solicit_returns_advertise_with_binding_identity() {
    // Loopback exercises the production bind-to-device and multicast setup
    // without depending on an environment-specific interface name.
    let config = dpu_config(
        "lo",
        InterfaceInfoV6 {
            address: Some("2001:db8::20".parse().unwrap()),
            prefix: "2001:db8::/64".to_string(),
        },
    );
    let request = client_message(MessageType::Solicit, DUID_LL, None, None);
    let listener = Arc::new(
        get_socket_v6(
            SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0),
            "lo",
            false,
        )
        .await
        .expect("DHCPv6 listener binds to loopback"),
    );
    let client = UdpSocket::bind(SocketAddrV6::new(Ipv6Addr::LOCALHOST, 0, 0, 0))
        .await
        .expect("DHCPv6 client binds");
    let destination = SocketAddrV6::new(
        Ipv6Addr::LOCALHOST,
        listener.local_addr().expect("listener address").port(),
        0,
        0,
    );

    // Send through the production socket setup so bind-to-device and
    // multicast membership are exercised before packet handling.
    client
        .send_to(&encode(&request), destination)
        .await
        .expect("client sends SOLICIT");
    let mut request_buffer = vec![0; 2048];
    let (length, source) = timeout(
        Duration::from_secs(5),
        listener.recv_from(&mut request_buffer),
    )
    .await
    .expect("listener receives before timeout")
    .expect("listener receives SOLICIT");
    let SocketAddr::V6(source) = source else {
        panic!("DHCPv6 listener received a non-IPv6 source");
    };
    let mut cache = machine_cache();

    let packet = process_packet(
        &request_buffer[..length],
        *source.ip(),
        &config,
        "lo",
        &Dpu {},
        &mut cache,
    )
    .await
    .expect("stateful SOLICIT is valid")
    .expect("stateful SOLICIT receives a response");
    listener
        .send_to(packet.encoded_packet(), source)
        .await
        .expect("listener sends ADVERTISE");

    // Decode what the client actually received from the UDP socket.
    let mut response_buffer = vec![0; 2048];
    let (length, _) = timeout(
        Duration::from_secs(5),
        client.recv_from(&mut response_buffer),
    )
    .await
    .expect("client receives before timeout")
    .expect("client receives ADVERTISE");
    let response = decode_message(&response_buffer[..length]);

    // The response binds the client's IAID to the configured /128 and identifies this server.
    assert_eq!(response.msg_type(), MessageType::Advertise);
    assert_eq!(
        response.opts().get(OptionCode::ServerId),
        Some(&DhcpOption::ServerId(SERVER_IDENTIFIER.to_vec()))
    );
    assert_eq!(
        response.opts().get(OptionCode::Preference),
        Some(&DhcpOption::Preference(255))
    );
    let association = response_ia_na(&response);
    assert_eq!(association.id, IAID);
    match association.opts.get(OptionCode::IAAddr) {
        Some(DhcpOption::IAAddr(address)) => {
            assert_eq!(address.addr, "2001:db8::20".parse::<Ipv6Addr>().unwrap());
            assert_eq!(address.preferred_life, 3600);
            assert_eq!(address.valid_life, 7200);
        }
        other => panic!("expected advertised IAADDR, got {other:?}"),
    }
}

/// Verifies stateful and options-only replies use trusted names and client-controlled flags.
///
/// This prevents client-supplied hostnames and absent option-39 requests from influencing replies.
#[tokio::test]
async fn fqdn_options_follow_client_negotiation_and_api_ownership() {
    // The configured name is trusted and has a parent distinct from the client-supplied name.
    let config = dpu_config(
        INTERFACE,
        InterfaceInfoV6 {
            address: Some("2001:db8::20".parse().unwrap()),
            prefix: "2001:db8::/64".to_string(),
        },
    );

    check_cases_async(
        [
            // Without option 39, only the API-owned search domain is returned.
            Case {
                scenario: "absent client FQDN",
                input: (MessageType::Solicit, None),
                expect: Yields((vec!["example.com.".to_string()], None)),
            },
            // An empty option 39 cannot request an echoed client FQDN.
            Case {
                scenario: "empty client FQDN",
                input: (MessageType::InformationRequest, Some(Vec::new())),
                expect: Yields((vec!["example.com.".to_string()], None)),
            },
            // A requested option 39 preserves flags but replaces the untrusted name.
            Case {
                scenario: "requested client FQDN",
                input: (
                    MessageType::InformationRequest,
                    Some(b"\x01\x06client\x07invalid\0".to_vec()),
                ),
                expect: Yields((
                    vec!["example.com.".to_string()],
                    Some(b"\x01\x04host\x07example\x03com\0".to_vec()),
                )),
            },
        ],
        |(message_type, client_fqdn)| {
            let config = config.clone();
            async move {
                // Vary only client option 39 so the API-owned name remains authoritative.
                let mut request = client_message(message_type, DUID_LL, None, None);
                if let Some(client_fqdn) = client_fqdn {
                    request
                        .opts_mut()
                        .insert(DhcpOption::Unknown(UnknownOption::new(
                            OptionCode::ClientFqdn,
                            client_fqdn,
                        )));
                }
                let mut cache = machine_cache();

                let response = process_packet(
                    &encode(&request),
                    "fe80::20".parse().unwrap(),
                    &config,
                    INTERFACE,
                    &Dpu {},
                    &mut cache,
                )
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{message_type:?} produced no DHCPv6 response"))?;
                let response = decode_response(&response);

                // Return decoded wire values so every case pins both naming options.
                let domain_search = match response.opts().get(OptionCode::DomainSearchList) {
                    Some(DhcpOption::DomainSearchList(domains)) => domains
                        .iter()
                        .map(|domain| domain.to_ascii())
                        .collect::<Vec<_>>(),
                    other => panic!("expected domain-search option, got {other:?}"),
                };
                let client_fqdn = match response.opts().get(OptionCode::ClientFqdn) {
                    Some(DhcpOption::Unknown(option)) => Some(option.data().to_vec()),
                    None => None,
                    other => panic!("expected raw client-FQDN option, got {other:?}"),
                };

                Ok::<_, String>((domain_search, client_fqdn))
            }
        },
    )
    .await;
}

/// Verifies DPU CONFIRM replies only when its configured prefix provides link knowledge.
#[tokio::test]
async fn dpu_confirm_with_known_prefix_returns_success() {
    let config = dpu_config(
        INTERFACE,
        InterfaceInfoV6 {
            address: Some("2001:db8::20".parse().unwrap()),
            prefix: "2001:db8::/64".to_string(),
        },
    );
    let request = client_message(
        MessageType::Confirm,
        DUID_LL,
        Some("2001:db8::99".parse().unwrap()),
        None,
    );
    let mut cache = machine_cache();

    // A configured DPU prefix is authoritative for the receiving link.
    let response = process_packet(
        &encode(&request),
        "fe80::20".parse().unwrap(),
        &config,
        INTERFACE,
        &Dpu {},
        &mut cache,
    )
    .await
    .expect("CONFIRM is valid")
    .expect("known-link CONFIRM receives a response");
    assert_eq!(
        response_status(&decode_response(&response)),
        Status::Success
    );
}

/// Verifies a SLAAC-only interface refuses stateful allocation but still serves v6 options.
#[tokio::test]
async fn slaac_only_interface_separates_stateful_and_information_requests() {
    let config = dpu_config(
        INTERFACE,
        InterfaceInfoV6 {
            address: None,
            prefix: "2001:db8::/64".to_string(),
        },
    );

    // A stateful SOLICIT cannot invent a /128 from the interface's SLAAC prefix.
    let solicit = client_message(MessageType::Solicit, DUID_LL, None, None);
    let mut cache = machine_cache();
    let advertise = process_packet(
        &encode(&solicit),
        "fe80::20".parse().unwrap(),
        &config,
        INTERFACE,
        &Dpu {},
        &mut cache,
    )
    .await
    .expect("SLAAC-only SOLICIT is valid")
    .expect("SLAAC-only SOLICIT receives a status response");
    let advertise = decode_response(&advertise);
    assert_eq!(advertise.msg_type(), MessageType::Advertise);
    assert_ia_na_failure(&advertise, Status::NoAddrsAvail);

    // INFORMATION-REQUEST remains useful and carries both configured v6 option families.
    let information = client_message(MessageType::InformationRequest, DUID_LL, None, None);
    let response = process_packet(
        &encode(&information),
        "fe80::20".parse().unwrap(),
        &config,
        INTERFACE,
        &Dpu {},
        &mut cache,
    )
    .await
    .expect("SLAAC-only information request is valid")
    .expect("SLAAC-only information request is served");
    let response = decode_response(&response);
    assert_eq!(response.msg_type(), MessageType::Reply);
    assert!(response.opts().get(OptionCode::Preference).is_none());
    assert!(response.opts().get(OptionCode::IANA).is_none());
    assert_eq!(
        response.opts().get(OptionCode::DomainNameServers),
        Some(&DhcpOption::DomainNameServers(vec![
            "2001:db8::53".parse().unwrap()
        ]))
    );
    assert_eq!(
        response.opts().get(OptionCode::NtpServer),
        Some(&DhcpOption::NtpServer(vec![NtpSuboption::ServerAddress(
            "2001:db8::123".parse().unwrap()
        )]))
    );
}

/// Verifies RENEW for a different /128 returns NoBinding instead of replacing the binding.
#[tokio::test]
async fn renew_for_unknown_address_returns_no_binding() {
    let config = dpu_config(
        INTERFACE,
        InterfaceInfoV6 {
            address: Some("2001:db8::20".parse().unwrap()),
            prefix: "2001:db8::/64".to_string(),
        },
    );
    let renew = client_message(
        MessageType::Renew,
        DUID_LL,
        Some("2001:db8::99".parse().unwrap()),
        Some(SERVER_IDENTIFIER.to_vec()),
    );
    let mut cache = machine_cache();

    // The wire message type remains available after API lookup and selects NoBinding.
    let response = process_packet(
        &encode(&renew),
        "fe80::20".parse().unwrap(),
        &config,
        INTERFACE,
        &Dpu {},
        &mut cache,
    )
    .await
    .expect("unknown RENEW is valid")
    .expect("unknown RENEW receives a status response");
    assert_ia_na_failure(&decode_response(&response), Status::NoBinding);
}

/// Verifies empty IA_NA refreshes return NoBinding because no address identifies a binding.
#[tokio::test]
async fn addressless_renew_and_rebind_return_no_binding() {
    // Configure one authoritative address shared by both refresh message types.
    let config = dpu_config(
        INTERFACE,
        InterfaceInfoV6 {
            address: Some("2001:db8::20".parse().unwrap()),
            prefix: "2001:db8::/64".to_string(),
        },
    );
    check_cases_async(
        [
            // RENEW selects this server but carries no address to identify a binding.
            Case {
                scenario: "Renew",
                input: (MessageType::Renew, Some(SERVER_IDENTIFIER.to_vec())),
                expect: Yields(()),
            },
            // REBIND omits ServerId and likewise carries no address-bearing binding.
            Case {
                scenario: "Rebind",
                input: (MessageType::Rebind, None),
                expect: Yields(()),
            },
        ],
        |(message_type, server_id)| {
            let config = config.clone();
            async move {
                // An empty IA_NA carries an IAID but identifies no renewable address.
                let mut refresh = client_message(message_type, DUID_LL, None, server_id);
                refresh.opts_mut().insert(DhcpOption::IANA(IANA {
                    id: IAID,
                    t1: 0,
                    t2: 0,
                    opts: Default::default(),
                }));
                let mut cache = machine_cache();

                let response = process_packet(
                    &encode(&refresh),
                    "fe80::20".parse().unwrap(),
                    &config,
                    INTERFACE,
                    &Dpu {},
                    &mut cache,
                )
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{message_type:?} produced no DHCPv6 response"))?;
                assert_ia_na_failure(&decode_response(&response), Status::NoBinding);
                Ok::<_, String>(())
            }
        },
    )
    .await;
}

/// Verifies SLAAC-only state reports the message-specific IA_NA failure
/// without fabricating an IAAddr.
#[tokio::test]
async fn slaac_only_binding_returns_message_specific_failure() {
    let config = dpu_config(
        INTERFACE,
        InterfaceInfoV6 {
            address: None,
            prefix: "2001:db8::/64".to_string(),
        },
    );
    let server_id = SERVER_IDENTIFIER.to_vec();

    check_cases_async(
        [
            // Initial allocation cannot provide an address from SLAAC-only state.
            Case {
                scenario: "Request returns NoAddrsAvail",
                input: (MessageType::Request, Status::NoAddrsAvail),
                expect: Yields(()),
            },
            // Refresh cannot match an address-bearing binding in SLAAC-only state.
            Case {
                scenario: "Renew returns NoBinding",
                input: (MessageType::Renew, Status::NoBinding),
                expect: Yields(()),
            },
        ],
        |(message_type, expected_status)| {
            let config = config.clone();
            let server_id = server_id.clone();
            async move {
                let request = client_message(
                    message_type,
                    DUID_LL,
                    Some("2001:db8::99".parse().unwrap()),
                    Some(server_id),
                );
                let mut cache = machine_cache();

                let response = process_packet(
                    &encode(&request),
                    "fe80::20".parse().unwrap(),
                    &config,
                    INTERFACE,
                    &Dpu {},
                    &mut cache,
                )
                .await
                .map_err(|error| error.to_string())?
                .ok_or_else(|| format!("{message_type:?} produced no DHCPv6 response"))?;
                assert_ia_na_failure(&decode_response(&response), expected_status);
                Ok::<_, String>(())
            }
        },
    )
    .await;
}
