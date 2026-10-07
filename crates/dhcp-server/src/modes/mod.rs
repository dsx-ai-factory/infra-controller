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
use std::net::SocketAddrV4;
use std::sync::Arc;

use lru::LruCache;
use rpc::forge::{DhcpDiscovery, DhcpRecord, MessageKind};
use tokio::sync::Mutex;
use tonic::async_trait;

use crate::Config;
use crate::cache::CacheEntry;
use crate::errors::DhcpError;
use crate::packet_handler::{DecodedPacket, Packet};

pub mod controller;
pub mod dpu;

/// Result of resolving one DHCPv6 request against the selected serving mode.
#[derive(Debug, Clone)]
pub enum V6Outcome {
    /// A stateful address and configuration options are available.
    Stateful(DhcpRecord),
    /// Configuration options are available without an address assignment.
    OptionsOnly(DhcpRecord),
    /// The interface is IPv6-enabled but has no stateful address binding.
    NoAddress,
}

#[async_trait]
pub trait DhcpMode: Send + Sync + std::fmt::Debug {
    /// Method to determine IP address to be returned to client.
    async fn discover_dhcp(
        &self,
        discovery_request: DhcpDiscovery,
        config: &Config,
        machine_cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
    ) -> Result<DhcpRecord, DhcpError>;
    /// Resolve a DHCPv6 request without collapsing address-less outcomes into a record.
    async fn discover_dhcp_v6(
        &self,
        discovery_request: DhcpDiscovery,
        config: &Config,
        machine_cache: &mut Arc<Mutex<LruCache<String, CacheEntry>>>,
    ) -> Result<V6Outcome, DhcpError>;
    /// And at what address?
    fn get_destination_address(&self, packet: &Packet) -> SocketAddrV4 {
        packet.dst_address()
    }
    /// Get circuit id. For dpu-with-relay, circuit id is interface name.
    fn get_circuit_id(&self, packet: &DecodedPacket, _circuit_id: &str) -> Option<String> {
        packet.get_circuit_id()
    }
    /// Should be relayed? A controller mode will accept on relayed packet, while dpu with relay
    /// mode will never get a relayed packet.
    fn should_be_relayed(&self) -> bool {
        true
    }
}

/// Return the message kind carried by the discovery request built from the
/// DHCPv6 wire message.
fn v6_message_kind(discovery_request: &DhcpDiscovery) -> Result<MessageKind, DhcpError> {
    let message_kind = discovery_request.message_kind.ok_or_else(|| {
        DhcpError::InvalidInput("DHCPv6 discovery request is missing message kind".to_string())
    })?;
    MessageKind::try_from(message_kind).map_err(|_| {
        DhcpError::InvalidInput(format!(
            "DHCPv6 discovery request has unknown message kind {message_kind}"
        ))
    })
}
