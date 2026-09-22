/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::rack::RackGroupId;
use rpc::forge::forge_server::Forge;
use rpc::forge::{ExpectedRackGroup, ExpectedRackGroupRequest, Metadata};
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::create_test_env;

#[crate::sqlx_test()]
async fn expected_rack_group_duplicate_create(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let group = ExpectedRackGroup {
        rack_group_id: Some(RackGroupId::new("group")),
        topology: "gb200_nvl72r1_c2g4".to_string(),
        metadata: Some(Metadata {
            name: "n".repeat(256),
            description: "d".repeat(1024),
            labels: vec![],
        }),
        ..Default::default()
    };
    env.api
        .add_expected_rack_group(Request::new(group.clone()))
        .await
        .unwrap();
    let error = env
        .api
        .add_expected_rack_group(Request::new(group.clone()))
        .await
        .unwrap_err();
    assert_eq!(error.code(), Code::AlreadyExists);
    let stored = env
        .api
        .get_expected_rack_group(Request::new(ExpectedRackGroupRequest {
            rack_group_id: "group".to_string(),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(stored, group);
    let ids = env
        .api
        .find_expected_rack_group_ids(Request::new(rpc::forge::ExpectedRackGroupSearchFilter {}))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(ids.rack_group_ids, vec![RackGroupId::new("group")]);
    let result = env
        .api
        .find_expected_rack_groups_by_ids(Request::new(
            rpc::forge::ExpectedRackGroupsByIdsRequest {
                rack_group_ids: vec![RackGroupId::new("unknown"), RackGroupId::new("group")],
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(result.expected_rack_groups, vec![group]);
    for ids in [
        vec![],
        vec![RackGroupId::new("group"); env.api.runtime_config.max_find_by_ids as usize + 1],
        vec![RackGroupId::new("group"), RackGroupId::new(" ")],
    ] {
        assert_eq!(
            env.api
                .find_expected_rack_groups_by_ids(Request::new(
                    rpc::forge::ExpectedRackGroupsByIdsRequest {
                        rack_group_ids: ids
                    }
                ))
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
    }
    for id in [" ".to_string(), "x".repeat(500)] {
        let request = ExpectedRackGroupRequest { rack_group_id: id };
        assert_eq!(
            env.api
                .get_expected_rack_group(Request::new(request.clone()))
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
        assert_eq!(
            env.api
                .delete_expected_rack_group(Request::new(request))
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
    }
}
