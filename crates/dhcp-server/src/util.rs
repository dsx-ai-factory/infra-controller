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
use std::net::{Ipv6Addr, SocketAddrV6};
use std::time::Duration;

use carbide_dhcp_common::{MachineArchitecture, VendorClass};
use carbide_instrument::emit;
use rpc::forge::DhcpRecord;
use tokio::net::UdpSocket;

use crate::Config;
use crate::errors::DhcpError;
use crate::metrics::{
    DhcpInterfaceBindFailed, DhcpSocketSetupFailed, SocketSetupNextAction, SocketSetupOperation,
};

const SOCKET_SETUP_ATTEMPTS: i32 = 10;
const INTERFACE_BIND_ATTEMPTS: i32 = 10;
const DHCPV6_LINK_SCOPED_GROUP: Ipv6Addr = Ipv6Addr::new(0xff02, 0, 0, 0, 0, 0, 1, 2);
const DHCPV6_SITE_SCOPED_GROUP: Ipv6Addr = Ipv6Addr::new(0xff05, 0, 0, 0, 0, 0, 1, 3);

fn open_configured_socket(
    listen_address: core::net::SocketAddrV4,
) -> Result<socket2::Socket, (SocketSetupOperation, std::io::Error)> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .map_err(|error| (SocketSetupOperation::Create, error))?;

    socket
        .set_reuse_address(true)
        .map_err(|error| (SocketSetupOperation::ReuseAddress, error))?;
    socket
        .set_nonblocking(true)
        .map_err(|error| (SocketSetupOperation::SetNonblocking, error))?;
    socket
        .bind(&listen_address.into())
        .map_err(|error| (SocketSetupOperation::BindAddress, error))?;
    // SO_BROADCAST permits sends to broadcast addresses; receiving does not require it.
    socket
        .set_broadcast(true)
        .map_err(|error| (SocketSetupOperation::SetBroadcast, error))?;

    Ok(socket)
}

fn socket_setup_next_action(retry: i32) -> SocketSetupNextAction {
    if retry + 1 == SOCKET_SETUP_ATTEMPTS {
        SocketSetupNextAction::Panic
    } else {
        SocketSetupNextAction::Retry
    }
}

fn interface_bind_next_action(retries_left: i32) -> SocketSetupNextAction {
    if retries_left == 0 {
        SocketSetupNextAction::Panic
    } else {
        SocketSetupNextAction::Retry
    }
}

/// Select the multicast groups required by one DHCPv6 listener policy.
fn dhcp_v6_multicast_groups(join_site_scoped_group: bool) -> impl Iterator<Item = Ipv6Addr> {
    [
        Some(DHCPV6_LINK_SCOPED_GROUP),
        join_site_scoped_group.then_some(DHCPV6_SITE_SCOPED_GROUP),
    ]
    .into_iter()
    .flatten()
}

pub(super) fn u8_to_mac(data: &[u8]) -> String {
    data.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<String>>()
        .join(":")
}

pub(super) fn u8_to_hex_string(data: &[u8]) -> Result<String, DhcpError> {
    Ok(std::str::from_utf8(data)?.to_string())
}

pub(super) fn machine_get_filename(
    dhcp_response: &DhcpRecord,
    vendor_class: &VendorClass,
    config: &Config,
) -> Vec<u8> {
    // If the API sent us the URL we should boot from, just use it.
    let url = if let Some(url) = &dhcp_response.booturl {
        url.to_string()
    } else {
        if !vendor_class.is_netboot() {
            return vec![];
        }

        let VendorClass { arch, .. } = vendor_class;

        let base_url = config.dhcp_config.carbide_provisioning_server_ipv4;
        match arch {
            MachineArchitecture::EfiX64 => {
                format!("http://{base_url}:8080/public/blobs/internal/x86_64/ipxe.efi")
            }
            MachineArchitecture::Arm64 => {
                format!("http://{base_url}:8080/public/blobs/internal/aarch64/ipxe.efi")
            }
            MachineArchitecture::BiosX86 => {
                tracing::warn!(
                    "Matched an HTTP client on a Legacy BIOS client, cannot provide HTTP boot URL"
                );
                return vec![];
            }
            MachineArchitecture::Unknown => {
                tracing::warn!("Matched an unknown architecture, cannot provide HTTP boot URL",);
                return vec![];
            }
        }
    };

    url.into_bytes().to_vec()
}

/// Create a UDP socket and set non_blocking, broadcast and other options flag on it.
pub async fn get_socket(listen_address: core::net::SocketAddrV4, interface: String) -> UdpSocket {
    for retry in 0..SOCKET_SETUP_ATTEMPTS {
        // Create a socket2 socket because std and Tokio sockets do not expose
        // the options that must be set before binding.
        let socket = match open_configured_socket(listen_address) {
            Ok(socket) => socket,
            Err((operation, error)) => {
                emit(DhcpSocketSetupFailed::new(
                    operation,
                    socket_setup_next_action(retry),
                    retry,
                    error.to_string(),
                ));
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                continue;
            }
        };

        let mut retries_left = INTERFACE_BIND_ATTEMPTS;
        while retries_left > 0 {
            let Err(error) = socket.bind_device(Some(interface.as_bytes())) else {
                break;
            };
            retries_left -= 1;
            emit(DhcpInterfaceBindFailed::new(
                interface_bind_next_action(retries_left),
                interface.clone(),
                retries_left,
                error.to_string(),
            ));
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        if retries_left == 0 {
            panic!("Cannot bind interface {interface}.");
        }

        // Now create tokio UDPSocket from socket2, which has all needed advanced options set.
        return UdpSocket::from_std(socket.into()).unwrap();
    }
    panic!("Could not create socket successfully.");
}

const DHCPV6_SOCKET_SETUP_ATTEMPTS: usize = 10;
const DHCPV6_SOCKET_RETRY_DELAY: Duration = Duration::from_secs(2);

/// Create a DHCPv6 socket, retrying boundedly while its interface becomes ready.
///
/// Every socket joins the link-scoped client/relay/server group. When
/// `join_site_scoped_group` is true, it also joins the site-scoped server group
/// used by relay agents.
pub async fn get_socket_v6(
    listen_address: SocketAddrV6,
    interface: &str,
    join_site_scoped_group: bool,
) -> Result<UdpSocket, DhcpError> {
    let mut attempts_remaining = DHCPV6_SOCKET_SETUP_ATTEMPTS;
    loop {
        match open_socket_v6(listen_address, interface, join_site_scoped_group) {
            Ok(socket) => return Ok(socket),
            Err(error) => {
                attempts_remaining -= 1;
                if attempts_remaining == 0 {
                    return Err(error);
                }

                // Interface creation and multicast readiness can race server startup.
                tracing::warn!(
                    interface_name = interface,
                    attempts_remaining,
                    error = %error,
                    "DHCPv6 socket setup failed, retrying"
                );
                tokio::time::sleep(DHCPV6_SOCKET_RETRY_DELAY).await;
            }
        }
    }
}

/// Perform one DHCPv6 socket setup attempt for the selected interface.
fn open_socket_v6(
    listen_address: SocketAddrV6,
    interface: &str,
    join_site_scoped_group: bool,
) -> Result<UdpSocket, DhcpError> {
    let socket = socket2::Socket::new(
        socket2::Domain::IPV6,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )?;
    socket.set_reuse_address(true)?;
    socket.set_only_v6(true)?;
    socket.set_nonblocking(true)?;
    socket.bind_device(Some(interface.as_bytes()))?;

    // SO_BINDTODEVICE scopes this wildcard listener to the selected interface.
    // The interface index below selects the same interface for multicast.
    let interface_index = if_index_for(interface)?;
    let listen_address = SocketAddrV6::new(
        *listen_address.ip(),
        listen_address.port(),
        listen_address.flowinfo(),
        interface_index,
    );
    socket.bind(&listen_address.into())?;

    // Controller-mode relays may discover servers through the site-scoped
    // All_DHCP_Servers group. DPU listeners deliberately remain link-local.
    for group in dhcp_v6_multicast_groups(join_site_scoped_group) {
        socket.join_multicast_v6(&group, interface_index)?;
    }

    Ok(UdpSocket::from_std(socket.into())?)
}

/// Resolve a Linux interface name to the scope index required by IPv6 sockets.
fn if_index_for(interface: &str) -> Result<u32, DhcpError> {
    let interface = CString::new(interface)
        .map_err(|_| DhcpError::InvalidInput("interface name contains a NUL byte".to_string()))?;

    // SAFETY: CString guarantees a valid NUL-terminated pointer for the duration
    // of the call, and if_nametoindex does not retain it.
    let interface_index = unsafe { libc::if_nametoindex(interface.as_ptr()) };
    if interface_index == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(interface_index)
    }
}

#[cfg(test)]
mod tests {
    use carbide_test_support::value_scenarios;

    use super::*;

    #[test]
    fn u8_to_mac_zero_pads_octets() {
        assert_eq!(
            u8_to_mac(&[0x00, 0x00, 0x5e, 0x00, 0x53, 0x01]),
            "00:00:5e:00:53:01"
        );
    }

    #[test]
    fn socket_setup_failures_report_whether_the_next_step_retries_or_panics() {
        value_scenarios!(socket_setup_next_action:
            "another socket attempt follows" {
                0 => SocketSetupNextAction::Retry,
                8 => SocketSetupNextAction::Retry,
            }

            "the outer retry budget is exhausted" {
                9 => SocketSetupNextAction::Panic,
            }
        );

        value_scenarios!(interface_bind_next_action:
            "another interface bind follows" {
                9 => SocketSetupNextAction::Retry,
                1 => SocketSetupNextAction::Retry,
            }

            "the interface bind budget is exhausted" {
                0 => SocketSetupNextAction::Panic,
            }
        );
    }

    /// DHCPv6 listeners always admit link-local discovery, while site-scoped
    /// relay discovery is included only when requested by the serving mode.
    #[test]
    fn multicast_groups_follow_listener_policy() {
        for (scenario, join_site_scoped_group, expected) in [
            // Direct-only DPU listeners join only the link-scoped group.
            ("link scope only", false, vec![DHCPV6_LINK_SCOPED_GROUP]),
            // Controller listeners also join the relay-to-server group.
            (
                "link and site scopes",
                true,
                vec![DHCPV6_LINK_SCOPED_GROUP, DHCPV6_SITE_SCOPED_GROUP],
            ),
        ] {
            assert_eq!(
                dhcp_v6_multicast_groups(join_site_scoped_group).collect::<Vec<_>>(),
                expected,
                "{scenario}"
            );
        }
    }
}
