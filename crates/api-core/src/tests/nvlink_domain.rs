/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use std::sync::Arc;

use carbide_nvlink_domain_controller::{
    NvLinkDomainStateControllerIO, NvLinkDomainStateHandler, NvLinkDomainStateHandlerServices,
};
use rpc::forge;
use rpc::forge::forge_server::Forge;
use state_controller::controller::StateController;
use tokio_util::sync::CancellationToken;
use tonic::{Code, Request};

use crate::tests::common::api_fixtures::create_test_env;

#[crate::sqlx_test()]
async fn expected_domain_api_lifecycle(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;
    sqlx::raw_sql(
        r#"INSERT INTO expected_rack_groups (rack_group_id, topology, racks)
        VALUES ('group', 'topology', '[{"rack_id":"rack","members":[
        {"type":"Compute","manufacturer":"NVIDIA","id":"compute","cartridge_index":0},
        {"type":"Switch","manufacturer":"NVIDIA","id":"switch","cartridge_index":0}]}]');"#,
    )
    .execute(&pool)
    .await
    .unwrap();
    let request = forge::CreateNvLinkDomainRequest {
        rack_group_id: Some("group".parse().unwrap()),
        name: Some("mini".into()),
        members: vec![forge::NvLinkDomainMember {
            rack_index: Some(99),
            slot_index: Some(99),
            component_type: "Compute".into(),
        }],
    };
    let domain = env
        .api
        .create_nv_link_domain(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    let lookup = forge::NvLinkDomainRequest {
        id: domain.id.clone(),
    };
    assert_eq!(
        env.api
            .get_nv_link_domain(Request::new(lookup.clone()))
            .await
            .unwrap()
            .into_inner(),
        domain
    );
    assert_eq!(domain.members, request.members);
    let id = domain.id.as_ref().unwrap().value.parse().unwrap();
    let snapshot = db::nvlink_domain::lifecycle::find_snapshot(&pool, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        snapshot.controller_state.value,
        model::nvlink_domain::NvLinkDomainState::Created
    );
    assert_eq!(
        env.api
            .create_nv_link_domain(Request::new(request.clone()))
            .await
            .unwrap_err()
            .code(),
        Code::AlreadyExists
    );
    assert_eq!(
        env.api
            .find_nv_link_domain_ids(Request::new(forge::NvLinkDomainSearchFilter::default()))
            .await
            .unwrap()
            .into_inner()
            .ids,
        vec![domain.id.clone().unwrap()]
    );
    assert_eq!(
        env.api
            .find_nv_link_domains_by_ids(Request::new(forge::NvLinkDomainsByIdsRequest {
                ids: vec![domain.id.clone().unwrap()]
            }))
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
                .find_nv_link_domains_by_ids(Request::new(forge::NvLinkDomainsByIdsRequest { ids }))
                .await
                .unwrap_err()
                .code(),
            Code::InvalidArgument
        );
    }
    env.api
        .delete_nv_link_domain(Request::new(lookup.clone()))
        .await
        .unwrap();
    let deleting = db::nvlink_domain::lifecycle::find_snapshot(&pool, id)
        .await
        .unwrap()
        .unwrap();
    assert!(deleting.deleted.is_some());
    assert_eq!(
        env.api
            .create_nv_link_domain(Request::new(request.clone()))
            .await
            .unwrap_err()
            .code(),
        Code::AlreadyExists
    );

    let mut controller = StateController::<NvLinkDomainStateControllerIO>::builder()
        .database(
            pool.clone(),
            env.api.work_lock_manager_handle.clone(),
            &env.api.instrumentation,
        )
        .processor_id("domain-api-test".into())
        .services(Arc::new(NvLinkDomainStateHandlerServices { db_pool: pool }))
        .state_handler(Arc::new(NvLinkDomainStateHandler))
        .build_for_manual_iterations(CancellationToken::new())
        .unwrap();
    controller.run_single_iteration().await;
    controller.run_single_iteration().await;
    assert_eq!(
        env.api
            .get_nv_link_domain(Request::new(lookup))
            .await
            .unwrap_err()
            .code(),
        Code::NotFound
    );
    env.api
        .create_nv_link_domain(Request::new(request))
        .await
        .unwrap();
}
