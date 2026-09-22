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
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcId;
use rpc::forge::forge_server::Forge;
use rpc::forge::{NetworkSegment, NetworkSegmentCreationRequest, NetworkSegmentType};
// `PgConnection` is only used by the `#[cfg(test)]` `text_history` helper below.
#[cfg(test)]
use sqlx::PgConnection;
use tonic::Request;

use super::api_fixtures::TestEnv;
use crate::api::Api;
use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common::rpc_builder::VpcCreationRequest;

pub(in crate::tests) struct NetworkSegmentHelper {
    inner: NetworkSegmentCreationRequest,
}

impl NetworkSegmentHelper {
    pub(in crate::tests) fn new_with_tenant_prefix(
        prefix: &str,
        gateway: &str,
        vpc_id: VpcId,
    ) -> Self {
        let prefixes = vec![rpc::forge::NetworkPrefix {
            id: None,
            prefix: prefix.into(),
            gateway: Some(gateway.into()),
            reserve_first: 1,
            free_ip_count: 0,
            svi_ip: None,
            free_ip_count_v2: None,
            free_ip_count_saturated: false,
        }];
        let inner = NetworkSegmentCreationRequest {
            vpc_id: Some(vpc_id),
            name: "TEST_SEGMENT".into(),
            subdomain_id: None,
            mtu: Some(1500),
            prefixes,
            segment_type: NetworkSegmentType::Tenant as i32,
            id: None,
            infer_slaac_eui64_addresses: false,
        };
        Self { inner }
    }

    pub(in crate::tests) async fn create_with_api(
        self,
        api: &Api,
    ) -> Result<NetworkSegment, tonic::Status> {
        let request = self.inner;
        api.create_network_segment(Request::new(request))
            .await
            .map(|response| response.into_inner())
    }
}

pub(in crate::tests) async fn create_network_segment_with_api(
    env: &TestEnv,
    use_subdomain: bool,
    use_vpc: bool,
    id: Option<NetworkSegmentId>,
    segment_type: i32,
    num_reserved: i32,
) -> rpc::forge::NetworkSegment {
    let vpc_id = if use_vpc {
        env.api
            .create_vpc(VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID).tonic_request())
            .await
            .unwrap()
            .into_inner()
            .id
    } else {
        None
    };

    let request = rpc::forge::NetworkSegmentCreationRequest {
        id,
        mtu: Some(1500),
        name: "TEST_SEGMENT".to_string(),
        prefixes: vec![rpc::forge::NetworkPrefix {
            id: None,
            prefix: "192.0.2.0/24".to_string(),
            gateway: Some("192.0.2.1".to_string()),
            reserve_first: num_reserved,
            free_ip_count: 0,
            svi_ip: None,
            free_ip_count_v2: None,
            free_ip_count_saturated: false,
        }],
        subdomain_id: use_subdomain.then(|| env.domain.into()),
        vpc_id,
        segment_type,
        infer_slaac_eui64_addresses: false,
    };

    env.api
        .create_network_segment(Request::new(request))
        .await
        .expect("Unable to create network segment")
        .into_inner()
}

#[derive(serde::Deserialize)]
struct LifecycleStateJson {
    state: String,
}

/// Derive the deprecated `TenantState` enum from a `NetworkSegment` response.
///
/// Reads `status.lifecycle.state` (a JSON string) and maps it to the corresponding variant.
/// A segment with a deletion timestamp is immediately `Terminating`, mirroring the
/// api-model `TryFrom` override applied before the controller processes the deletion.
pub(in crate::tests) fn tenant_state_from_segment(
    segment: &rpc::forge::NetworkSegment,
) -> rpc::forge::TenantState {
    // A deletion timestamp means the API accepted the delete request; map to Terminating
    // immediately, even before the controller processes it. Mirrors the api-model TryFrom logic.
    if segment.deleted.is_some() {
        return rpc::forge::TenantState::Terminating;
    }
    let lifecycle = segment.status.as_ref().and_then(|s| s.lifecycle.as_ref());
    let state_str = lifecycle.map(|lc| lc.state.as_str()).unwrap_or("{}");
    let json: LifecycleStateJson =
        serde_json::from_str(state_str).unwrap_or_else(|_| LifecycleStateJson {
            state: String::new(),
        });
    match json.state.as_str() {
        "provisioning" => rpc::forge::TenantState::Provisioning,
        "ready" => rpc::forge::TenantState::Ready,
        "deleting" => rpc::forge::TenantState::Terminating,
        _ => rpc::forge::TenantState::default(),
    }
}

pub(in crate::tests) async fn get_segment_state(
    api: &Api,
    segment_id: NetworkSegmentId,
) -> rpc::forge::TenantState {
    let segment = api
        .find_network_segments_by_ids(Request::new(rpc::forge::NetworkSegmentsByIdsRequest {
            network_segments_ids: vec![segment_id],
            include_history: false,
            include_num_free_ips: false,
        }))
        .await
        .unwrap()
        .into_inner()
        .network_segments
        .remove(0);
    tenant_state_from_segment(&segment)
}

#[allow(dead_code)]
pub(in crate::tests) async fn get_segments(
    api: &Api,
    request: rpc::forge::NetworkSegmentsByIdsRequest,
) -> rpc::forge::NetworkSegmentList {
    api.find_network_segments_by_ids(Request::new(request))
        .await
        .unwrap()
        .into_inner()
}

#[cfg(test)]
pub(in crate::tests) async fn text_history(
    txn: &mut PgConnection,
    segment_id: NetworkSegmentId,
) -> Vec<String> {
    let entries = db::state_history::for_object(
        txn,
        db::state_history::StateHistoryTableId::NetworkSegment,
        &segment_id,
    )
    .await
    .unwrap();

    // // Check that version numbers are always incrementing by 1
    if !entries.is_empty() {
        let first_version = entries[0].state_version.version_nr();
        for (expected_version, entry) in ((first_version + 1)..).zip(&entries[1..]) {
            assert_eq!(entry.state_version.version_nr(), expected_version);
        }
    }

    let mut states = Vec::with_capacity(entries.len());
    for e in entries.into_iter() {
        states.push(e.state);
    }
    states
}
