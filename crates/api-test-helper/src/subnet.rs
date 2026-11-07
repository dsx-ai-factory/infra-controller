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

use std::net::SocketAddr;

use eyre::{ContextCompat, WrapErr};
use rpc::forge::{
    NetworkPrefix, NetworkSegmentCreationRequest, NetworkSegmentType, NetworkSegmentsByIdsRequest,
    TenantState,
};

use crate::api_client;

pub async fn create(
    carbide_api_addrs: &[SocketAddr],
    vpc_id: &str,
    domain_id: &str,
    prefix_octet: u8,
    host_inband_network: bool,
) -> eyre::Result<String> {
    tracing::info!("Creating network segment");

    let request = NetworkSegmentCreationRequest {
        vpc_id: Some(vpc_id.parse()?),
        name: "tenant1".to_string(),
        subdomain_id: Some(domain_id.parse()?),
        prefixes: vec![NetworkPrefix {
            prefix: format!("10.10.{prefix_octet}.0/24"),
            gateway: Some(format!("10.10.{prefix_octet}.1")),
            reserve_first: 10,
            ..Default::default()
        }],
        segment_type: if host_inband_network {
            NetworkSegmentType::HostInband as i32
        } else {
            NetworkSegmentType::Tenant as i32
        },
        ..Default::default()
    };
    let segment = api_client::call(
        carbide_api_addrs,
        "CreateNetworkSegment",
        |mut client| async move { client.create_network_segment(request).await },
    )
    .await?;
    let segment_id = segment
        .id
        .context("CreateNetworkSegment response has no network segment ID")?
        .to_string();
    tracing::info!(
        network_segment_id = %segment_id,
        "Network segment created",
    );

    wait_for_network_segment_state(carbide_api_addrs, &segment_id, "READY").await?;

    tracing::info!(
        network_segment_id = %segment_id,
        "Network segment is ready",
    );
    Ok(segment_id)
}

// Waits for a network segment to reach a certain state
pub async fn wait_for_network_segment_state(
    addrs: &[SocketAddr],
    segment_id: &str,
    target_state: &str,
) -> eyre::Result<()> {
    const MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
    let start = std::time::Instant::now();

    let mut latest_state = "not observed".to_string();

    tracing::info!(
        network_segment_id = segment_id,
        target_state,
        "Waiting for Network Segment state",
    );
    while start.elapsed() < MAX_WAIT {
        let request = NetworkSegmentsByIdsRequest {
            network_segments_ids: vec![segment_id.parse()?],
            ..Default::default()
        };
        let response =
            api_client::call(addrs, "FindNetworkSegmentsByIds", |mut client| async move {
                client.find_network_segments_by_ids(request).await
            })
            .await?;
        let segment = response
            .network_segments
            .first()
            .context("FindNetworkSegmentsByIds returned no network segments")?;
        let status = segment
            .status
            .as_ref()
            .context("FindNetworkSegmentsByIds returned a network segment without status")?;
        latest_state = TenantState::try_from(status.tenant_state)
            .wrap_err("FindNetworkSegmentsByIds returned an unknown tenant state")?
            .as_str_name()
            .to_string();
        if latest_state.contains(target_state) {
            return Ok(());
        }
        tracing::info!(
            network_segment_state = %latest_state,
            "\tCurrent network segment state",
        );
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    eyre::bail!(
        "even after {MAX_WAIT:?}, {segment_id} did not reach state {target_state}; latest state: {latest_state}"
    );
}

pub async fn create_dual_stack(
    carbide_api_addrs: &[SocketAddr],
    vpc_id: &str,
    domain_id: &str,
    prefix_octet: u8,
) -> eyre::Result<String> {
    tracing::info!("Creating dual-stack network segment");

    let request = NetworkSegmentCreationRequest {
        vpc_id: Some(vpc_id.parse()?),
        name: "tenant1_dual_stack".to_string(),
        subdomain_id: Some(domain_id.parse()?),
        prefixes: vec![
            NetworkPrefix {
                prefix: format!("10.10.{prefix_octet}.0/24"),
                gateway: Some(format!("10.10.{prefix_octet}.1")),
                reserve_first: 10,
                ..Default::default()
            },
            NetworkPrefix {
                prefix: format!("2001:db8:{prefix_octet}::/112"),
                reserve_first: 1,
                ..Default::default()
            },
        ],
        segment_type: NetworkSegmentType::Tenant as i32,
        ..Default::default()
    };
    let segment = api_client::call(
        carbide_api_addrs,
        "CreateNetworkSegment",
        |mut client| async move { client.create_network_segment(request).await },
    )
    .await?;
    let segment_id = segment
        .id
        .context("CreateNetworkSegment response has no network segment ID")?
        .to_string();
    tracing::info!(
        network_segment_id = %segment_id,
        "Dual-stack network segment created",
    );

    wait_for_network_segment_state(carbide_api_addrs, &segment_id, "READY").await?;

    tracing::info!(
        network_segment_id = %segment_id,
        "Dual-stack network segment is ready",
    );
    Ok(segment_id)
}
