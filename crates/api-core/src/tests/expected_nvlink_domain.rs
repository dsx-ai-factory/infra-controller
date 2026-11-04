/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use rpc::forge;
use rpc::forge::forge_server::Forge;
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::create_test_env;

#[crate::sqlx_test()]
async fn expected_domain_api_lifecycle(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;
    sqlx::raw_sql("INSERT INTO expected_rack_groups (rack_group_id, topology, racks) VALUES ('group', 'topology', '[{\"rack_id\":\"rack\",\"members\":[]}]');
        INSERT INTO expected_machines (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id) VALUES ('compute', '00:11:22:33:44:55', 'test', 'test', 'rack');
        INSERT INTO expected_switches (serial_number, bmc_mac_address, bmc_username, bmc_password, rack_id) VALUES ('switch', '00:11:22:33:44:66', 'test', 'test', 'rack');")
        .execute(&pool).await.unwrap();
    let request = forge::CreateExpectedNvLinkDomainRequest {
        rack_group_id: Some("group".parse().unwrap()),
        name: Some("mini".into()),
        member_bmc_mac_addresses: vec!["00:11:22:33:44:66".into(), "00:11:22:33:44:55".into()],
    };
    let domain = env
        .api
        .create_expected_nv_link_domain(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    let lookup = forge::ExpectedNvLinkDomainRequest {
        id: domain.id.clone(),
    };
    assert_eq!(
        env.api
            .get_expected_nv_link_domain(Request::new(lookup.clone()))
            .await
            .unwrap()
            .into_inner(),
        domain
    );
    assert_eq!(
        domain.member_bmc_mac_addresses,
        vec!["00:11:22:33:44:55", "00:11:22:33:44:66"]
    );
    assert_eq!(
        env.api
            .create_expected_nv_link_domain(Request::new(request.clone()))
            .await
            .unwrap_err()
            .code(),
        Code::AlreadyExists
    );
    assert_eq!(
        env.api
            .find_expected_nv_link_domain_ids(Request::new(
                forge::ExpectedNvLinkDomainSearchFilter::default()
            ))
            .await
            .unwrap()
            .into_inner()
            .ids,
        vec![domain.id.clone().unwrap()]
    );
    assert_eq!(
        env.api
            .find_expected_nv_link_domains_by_ids(Request::new(
                forge::ExpectedNvLinkDomainsByIdsRequest {
                    ids: vec![domain.id.clone().unwrap()]
                }
            ))
            .await
            .unwrap()
            .into_inner()
            .domains,
        vec![domain.clone()]
    );
    for ids in [
        vec![],
        vec![domain.id.clone().unwrap(); env.api.runtime_config.max_find_by_ids as usize + 1],
        vec![rpc::common::Uuid {
            value: "invalid".into(),
        }],
    ] {
        assert_eq!(
            env.api
                .find_expected_nv_link_domains_by_ids(Request::new(
                    forge::ExpectedNvLinkDomainsByIdsRequest { ids }
                ))
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
    }
    env.api
        .delete_expected_nv_link_domain(Request::new(lookup.clone()))
        .await
        .unwrap();
    assert_eq!(
        env.api
            .get_expected_nv_link_domain(Request::new(lookup))
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    // A failed competing create must not leave an orphan declaration behind.
    assert!(
        env.api
            .find_expected_nv_link_domain_ids(Request::new(
                forge::ExpectedNvLinkDomainSearchFilter::default()
            ))
            .await
            .unwrap()
            .into_inner()
            .ids
            .is_empty()
    );
    env.api
        .create_expected_nv_link_domain(Request::new(request))
        .await
        .unwrap();
}
