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
use carbide_dhcp_server::modes::controller::Controller;
use carbide_dhcp_server::modes::dpu::Dpu;
use carbide_dhcp_server::packet_handler_v6::process_packet;
use carbide_rpc_utils::dhcp::InterfaceInfoV6;
use dhcproto::v6::{MessageType, OptionCode};
use rpc::forge::{AddressFamily, MessageKind};

mod common;

use common::{
    DUID_LL, MockDiscoverDhcpApi, client_message, controller_config, dpu_config, encode,
    machine_cache, relay_forward, relay_option,
};

const INTERFACE: &str = "eth0";
const OPTION79: &[u8] = &[0, 1, 2, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];

/// Verify DPU and controller modes render the same stateful DHCPv6 response body.
///
/// The modes have different identity and data sources, but once both resolve
/// the same binding their client-visible DHCPv6 contract must not drift.
#[tokio::test]
async fn stateful_solicit_is_equivalent_across_dpu_and_controller_modes() {
    let request = encode(&client_message(MessageType::Solicit, DUID_LL, None, None));

    // Resolve the direct request from the DPU-delivered interface binding.
    let dpu_config = dpu_config(
        INTERFACE,
        InterfaceInfoV6 {
            address: Some("2001:db8::ee".parse().unwrap()),
            prefix: "2001:db8::/64".to_string(),
        },
    );
    let mut dpu_cache = machine_cache();
    let dpu_response = process_packet(
        &request,
        "fe80::100".parse().unwrap(),
        &dpu_config,
        INTERFACE,
        &Dpu {},
        &mut dpu_cache,
    )
    .await
    .expect("direct DPU SOLICIT is valid")
    .expect("direct DPU SOLICIT is served");

    // Resolve the equivalent relayed request from the authoritative API.
    let api = MockDiscoverDhcpApi::start().await;
    let controller_config = controller_config(api.url());
    let relayed_request = relay_forward(&request, INTERFACE.as_bytes(), OPTION79);
    let mut controller_cache = machine_cache();
    let controller_response = process_packet(
        &relayed_request,
        "fe80::200".parse().unwrap(),
        &controller_config,
        INTERFACE,
        &Controller {},
        &mut controller_cache,
    )
    .await
    .expect("relayed controller SOLICIT is valid")
    .expect("relayed controller SOLICIT is served");
    let discoveries = api.shutdown().await;

    // Strip the controller-only relay envelope and compare the exact client payload.
    let controller_inner = relay_option(controller_response.encoded_packet(), OptionCode::RelayMsg);
    assert_eq!(dpu_response.encoded_packet(), controller_inner);

    // The controller source must have been one complete family-aware API lookup.
    assert_eq!(discoveries.len(), 1);
    assert_eq!(
        discoveries[0].address_family,
        Some(AddressFamily::V6 as i32)
    );
    assert_eq!(
        discoveries[0].message_kind,
        Some(MessageKind::V6Solicit as i32)
    );
    assert_eq!(discoveries[0].duid.as_deref(), Some(DUID_LL));
    assert_eq!(discoveries[0].mac_address, "02:aa:bb:cc:dd:ee");
}
