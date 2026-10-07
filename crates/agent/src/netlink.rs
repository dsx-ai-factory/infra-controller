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

use std::collections::HashMap;
use std::error::Error;

use futures_util::TryStreamExt;
use rpc::forge as rpc;
use rtnetlink;
use rtnetlink::packet_route::link::{
    LinkAttribute, LinkLayerType, LinkMessage, State as LinkState,
};
use tokio;

#[derive(Clone, Debug)]
// Most of the fields are Option<T> because the netlink protocol allows them
// to be absent (even though we have no reason to believe they'd ever actually
// be missing).
pub struct InterfaceLinkData {
    pub link_type: LinkLayerType,
    pub address: Option<Vec<u8>>,
    pub state: Option<LinkState>,
    pub mtu: Option<usize>,
    pub carrier_up_count: Option<u32>,
    pub carrier_down_count: Option<u32>,
}

impl InterfaceLinkData {
    pub fn link_is_up(&self) -> Option<bool> {
        self.state.map(|state| matches!(state, LinkState::Up))
    }

    pub fn is_ethernet(&self) -> bool {
        matches!(self.link_type, LinkLayerType::Ether)
    }
}

impl From<LinkMessage> for InterfaceLinkData {
    fn from(link_message: LinkMessage) -> Self {
        let link_type = link_message.header.link_layer_type;
        let address = link_message
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                LinkAttribute::Address(address) => Some(address.to_owned()),
                _ => None,
            });
        let state = link_message
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                LinkAttribute::OperState(state) => Some(*state),
                _ => None,
            });
        let mtu = link_message
            .attributes
            .iter()
            .find_map(|attribute| match attribute {
                LinkAttribute::Mtu(size) => Some(*size as usize),
                _ => None,
            });
        let carrier_up_count =
            link_message
                .attributes
                .iter()
                .find_map(|attribute| match attribute {
                    LinkAttribute::CarrierUpCount(count) => Some(*count),
                    _ => None,
                });
        let carrier_down_count =
            link_message
                .attributes
                .iter()
                .find_map(|attribute| match attribute {
                    LinkAttribute::CarrierDownCount(count) => Some(*count),
                    _ => None,
                });
        InterfaceLinkData {
            link_type,
            address,
            state,
            mtu,
            carrier_up_count,
            carrier_down_count,
        }
    }
}

impl From<&InterfaceLinkData> for rpc::LinkData {
    fn from(link_data: &InterfaceLinkData) -> Self {
        let link_type = Some(match link_data.link_type {
            LinkLayerType::Ether => "ethernet".to_owned(),
            LinkLayerType::Infiniband => "infiniband".to_owned(),
            LinkLayerType::Loopback => "loopback".to_owned(),
            other => {
                let code: u16 = other.into();
                format!("unhandled link type (code {code})")
            }
        });
        let state = link_data.state.map(|state| match state {
            LinkState::Up => "up".to_owned(),
            LinkState::Down => "down".to_owned(),
            LinkState::Unknown => "unknown".to_owned(),
            LinkState::NotPresent => "not present".to_owned(),
            LinkState::LowerLayerDown => "lower layer down".to_owned(),
            LinkState::Testing => "testing".to_owned(),
            LinkState::Dormant => "dormant".to_owned(),
            other => {
                let code: u8 = other.into();
                format!("unknown link state (code {code})")
            }
        });
        let carrier_up = link_data.link_is_up();
        // We originally converted this from a u32, so it shouldn't have
        // truncation issues.
        let mtu = link_data.mtu.map(|mtu| mtu as u32);
        let carrier_up_count = link_data.carrier_up_count;
        let carrier_down_count = link_data.carrier_down_count;

        rpc::LinkData {
            link_type,
            state,
            carrier_up,
            mtu,
            carrier_up_count,
            carrier_down_count,
        }
    }
}

#[derive(Debug)]
pub struct LinkDataError {
    kind: LinkDataErrorKind,
    interface: Option<String>,
}

impl LinkDataError {
    pub fn connection(connection_error: std::io::Error) -> Self {
        let kind = LinkDataErrorKind::Connection(connection_error);
        let interface = None;
        Self { kind, interface }
    }

    pub fn communication(communication_error: rtnetlink::Error) -> Self {
        let kind = LinkDataErrorKind::Communication(communication_error);
        let interface = None;
        Self { kind, interface }
    }

    pub fn interface_communication(communication_error: rtnetlink::Error, interface: &str) -> Self {
        let kind = LinkDataErrorKind::Communication(communication_error);
        let interface = Some(interface.to_owned());
        Self { kind, interface }
    }

    pub fn empty_response(interface: &str) -> Self {
        let kind = LinkDataErrorKind::EmptyResponse;
        let interface = Some(String::from(interface));
        Self { kind, interface }
    }
}

impl std::fmt::Display for LinkDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let err_message = "couldn't get interface link data";
        if let Some(interface) = self.interface.as_ref() {
            write!(f, "{err_message} for {interface}")
        } else {
            write!(f, "{err_message}")
        }
    }
}

impl Error for LinkDataError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self.kind {
            LinkDataErrorKind::Connection(ref e) => Some(e),
            LinkDataErrorKind::Communication(ref e) => Some(e),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum LinkDataErrorKind {
    Connection(std::io::Error),
    Communication(rtnetlink::Error),
    EmptyResponse,
}

// Retrieve the link data (state, MTU, etc.) for all interfaces, and return
// them as a HashMap keyed by interface name. This is roughly equivalent to `ip
// link show` since we're using the same netlink interface under the hood as
// that command.
pub async fn get_all_interface_links() -> Result<HashMap<String, InterfaceLinkData>, LinkDataError>
{
    let (netlink_connection, rtnetlink_handle, _receiver) =
        rtnetlink::new_connection().map_err(LinkDataError::connection)?;

    // We have to spawn off the netlink connection because of the architecture
    // of `netlink_proto::Connection`, which runs in the background and owns
    // the socket. We communicate with it via channel messages, and it will exit
    // when both `rtnetlink_handle` and `_receiver` go out of scope.
    tokio::spawn(netlink_connection);

    let responses = rtnetlink_handle.link().get().execute();
    responses
        .try_filter_map(|link_message| async {
            let maybe_interface_data = match extract_interface_name(&link_message) {
                Some(interface_name) => {
                    Some((interface_name, InterfaceLinkData::from(link_message)))
                }
                None => {
                    let idx = link_message.header.index;
                    tracing::warn!(
                        interface_index = idx,
                        "Network interface doesn't have a name (no IfName attribute)"
                    );
                    None
                }
            };
            Ok(maybe_interface_data)
        })
        .try_collect()
        .await
        .map_err(LinkDataError::communication)
}

fn extract_interface_name(link_message: &LinkMessage) -> Option<String> {
    link_message
        .attributes
        .iter()
        .find_map(|attribute| match attribute {
            LinkAttribute::IfName(name) => Some(name.clone()),
            _ => None,
        })
}

#[cfg(test)]
mod test {
    use super::*;

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn test_get_all_interface_links() {
        let all_interfaces = get_all_interface_links().await;
        let all_interfaces = all_interfaces.expect("get_all_interface_links() returned an error");
        let lo_data = all_interfaces
            .get("lo")
            .expect("get_all_interface_links() didn't contain data for the 'lo' interface");
        assert_eq!(lo_data.link_type, LinkLayerType::Loopback);
        assert_eq!(lo_data.mtu, Some(65536));
        assert_eq!(
            lo_data.address.as_deref(),
            Some([0, 0, 0, 0, 0, 0].as_slice())
        );
    }
}
