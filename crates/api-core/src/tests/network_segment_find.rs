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

#![allow(deprecated)]

use ::rpc::forge as rpc;
use rpc::forge_server::Forge;

use crate::tests::common::api_fixtures::network_segment::create_network_segment;
use crate::tests::common::api_fixtures::vpc::create_vpc;
use crate::tests::common::api_fixtures::{TestEnvOverrides, create_test_env_with_overrides};

#[crate::sqlx_test]
async fn test_find_network_segment_ids(pool: sqlx::PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;

    for i in 0..4 {
        let mut tenant_org_id = "tenant_org_1";
        if i % 2 != 0 {
            tenant_org_id = "tenant_org_2";
        }
        let (vpc_id, _vpc) = create_vpc(
            &env,
            format!("vpc_{i}"),
            Some(tenant_org_id.to_string()),
            None,
        )
        .await;
        create_network_segment(
            &env.api,
            format!("segment_{i}").as_str(),
            format!("192.0.{}.0/24", i + 1).as_str(),
            format!("192.0.{}.1", i + 1).as_str(),
            rpc::NetworkSegmentType::Underlay,
            Some(vpc_id),
            true,
        )
        .await;
    }

    // test getting all ids
    let request_all = tonic::Request::new(rpc::NetworkSegmentSearchFilter {
        name: None,
        tenant_org_id: None,
    });

    let ids_all = env
        .api
        .find_network_segment_ids(request_all)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_all.network_segments_ids.len(), 4);

    // test getting ids based on name
    let request_name = tonic::Request::new(rpc::NetworkSegmentSearchFilter {
        name: Some("segment_2".to_string()),
        tenant_org_id: None,
    });

    let ids_name = env
        .api
        .find_network_segment_ids(request_name)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_name.network_segments_ids.len(), 1);

    // test search by tenant_org_id
    let request_tenant = tonic::Request::new(rpc::NetworkSegmentSearchFilter {
        name: None,
        tenant_org_id: Some("tenant_org_2".to_string()),
    });

    let ids_tenant = env
        .api
        .find_network_segment_ids(request_tenant)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_tenant.network_segments_ids.len(), 2);

    // test search by tenant_org_id and name
    let request_tenant_name = tonic::Request::new(rpc::NetworkSegmentSearchFilter {
        name: Some("segment_3".to_string()),
        tenant_org_id: Some("tenant_org_2".to_string()),
    });

    let ids_tenant_name = env
        .api
        .find_network_segment_ids(request_tenant_name)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_tenant_name.network_segments_ids.len(), 1);
}

#[crate::sqlx_test]
async fn test_find_network_segment_by_ids(pool: sqlx::PgPool) {
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::no_network_segments()).await;

    for i in 0..4 {
        let mut tenant_org_id = "tenant_org_1";
        if i % 2 != 0 {
            tenant_org_id = "tenant_org_2";
        }
        let (vpc_id, _vpc) = create_vpc(
            &env,
            format!("vpc_{i}"),
            Some(tenant_org_id.to_string()),
            None,
        )
        .await;
        create_network_segment(
            &env.api,
            format!("segment_{i}").as_str(),
            format!("192.0.{}.0/24", i + 1).as_str(),
            format!("192.0.{}.1", i + 1).as_str(),
            rpc::NetworkSegmentType::Underlay,
            Some(vpc_id),
            true,
        )
        .await;
    }

    let request_ids = tonic::Request::new(rpc::NetworkSegmentSearchFilter {
        name: None,
        tenant_org_id: Some("tenant_org_2".to_string()),
    });

    let ids_list = env
        .api
        .find_network_segment_ids(request_ids)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(ids_list.network_segments_ids.len(), 2);

    let seg_request = tonic::Request::new(rpc::NetworkSegmentsByIdsRequest {
        network_segments_ids: ids_list.network_segments_ids.clone(),
        include_history: false,
        include_num_free_ips: false,
    });

    let seg_list = env
        .api
        .find_network_segments_by_ids(seg_request)
        .await
        .map(|response| response.into_inner())
        .unwrap();

    assert_eq!(seg_list.network_segments.len(), 2);
    for segment in seg_list.network_segments {
        let prefixes = &segment
            .config
            .as_ref()
            .expect("segment config must be present")
            .prefixes;
        assert!(!prefixes.is_empty());
        assert!(prefixes.iter().all(|prefix| {
            prefix.free_ip_count == 0
                && prefix.free_ip_count_v2.is_none()
                && !prefix.free_ip_count_saturated
        }));
    }

    let seg_request = tonic::Request::new(rpc::NetworkSegmentsByIdsRequest {
        network_segments_ids: ids_list.network_segments_ids.clone(),
        include_history: true,
        include_num_free_ips: true,
    });

    let seg_list = env
        .api
        .find_network_segments_by_ids(seg_request)
        .await
        .map(|response| response.into_inner())
        .unwrap();

    assert_eq!(seg_list.network_segments.len(), 2);

    for segment in seg_list.network_segments {
        assert!(
            !segment
                .config
                .as_ref()
                .expect("segment config must be present")
                .prefixes
                .is_empty()
        );
        let prefix = segment
            .config
            .as_ref()
            .and_then(|config| config.prefixes.first())
            .expect("segment prefix must be present");
        assert!(prefix.free_ip_count > 0);
        assert!(prefix.free_ip_count_v2.is_some_and(|count| count > 0));
        assert!(!prefix.free_ip_count_saturated);
    }
}

// The empty-list and over-max guards for `find_network_segments_by_ids` are
// shared API-layer code, proven once across representative RPCs in
// `tests::find_by_ids_guards`.
