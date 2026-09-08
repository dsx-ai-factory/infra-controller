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
use std::ops::DerefMut;

use ::rpc::forge as rpc;
use model::firmware::FirmwareComponentType;
use rpc::forge_server::Forge;
use tonic::Code;

use crate::tests::common;
use crate::tests::common::api_fixtures::{create_managed_host, create_test_env};

#[crate::sqlx_test()]
async fn test_find_explored_endpoint_ids(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    let mut txn = env.pool.begin().await?;
    for i in 1..6 {
        common::endpoint::insert_endpoint_version(
            &mut txn,
            format!("141.219.24.{i}").as_str(),
            "1.0",
        )
        .await?;
    }
    txn.commit().await?;

    let id_list = env
        .api
        .find_explored_endpoint_ids(tonic::Request::new(
            ::rpc::site_explorer::ExploredEndpointSearchFilter {},
        ))
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(id_list.endpoint_ids.len(), 5);

    Ok(())
}

#[crate::sqlx_test()]
async fn test_find_explored_endpoints_by_ids(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    let mut txn = env.pool.begin().await?;
    for i in 1..6 {
        common::endpoint::insert_endpoint_version(
            &mut txn,
            format!("141.219.24.{i}").as_str(),
            "1.0",
        )
        .await?;
    }
    txn.commit().await?;

    let id_list = env
        .api
        .find_explored_endpoint_ids(tonic::Request::new(
            ::rpc::site_explorer::ExploredEndpointSearchFilter {},
        ))
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(id_list.endpoint_ids.len(), 5);

    let request = tonic::Request::new(::rpc::site_explorer::ExploredEndpointsByIdsRequest {
        endpoint_ids: id_list.endpoint_ids.clone(),
    });

    let endpoint_list = env
        .api
        .find_explored_endpoints_by_ids(request)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(endpoint_list.endpoints.len(), 5);

    // validate we got endpoints with specified ids
    let mut endpoints_copy = endpoint_list.endpoints;
    for _ in 0..5 {
        let ep = endpoints_copy.remove(0);
        let ep_id = ep.address;
        assert!(id_list.endpoint_ids.contains(&ep_id));
    }

    Ok(())
}

// The empty-list and over-max guards for `find_explored_endpoints_by_ids` are
// shared API-layer code, proven once across representative RPCs in
// `tests::find_by_ids_guards`.

// This test intentionally exercises the deprecated top-level `machine_id`
// field to prove it is still accepted alongside the new `device_id` target.
#[allow(deprecated)]
#[crate::sqlx_test]
async fn test_admin_bmc_reset(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    // Setup
    let env = create_test_env(db_pool.clone()).await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();
    let host_machine = env.find_machine(&host_machine_id).await.remove(0);

    let bmc_ip = host_machine.bmc_info.as_ref().unwrap().ip();

    // Check that we find full BMC details based only on BMC IP
    let req = tonic::Request::new(rpc::AdminBmcResetRequest {
        bmc_endpoint_request: Some(rpc::BmcEndpointRequest {
            ip_address: bmc_ip.to_string(),
            mac_address: None,
        }),
        use_ipmitool: false,
        ..Default::default()
    });
    let api_result = env.api.admin_bmc_reset(req).await;
    assert!(api_result.is_ok());

    // Check that we find full BMC details based only on machine_id
    let req = tonic::Request::new(rpc::AdminBmcResetRequest {
        bmc_endpoint_request: None,
        machine_id: Some(host_machine_id.to_string()),
        use_ipmitool: false,
        ..Default::default()
    });
    let api_result = env.api.admin_bmc_reset(req).await;
    assert!(api_result.is_ok());

    // Check that we find BMC details but things fail because actual and expected BMC MAC are different
    let req = tonic::Request::new(rpc::AdminBmcResetRequest {
        bmc_endpoint_request: Some(rpc::BmcEndpointRequest {
            ip_address: bmc_ip.to_string(),
            mac_address: Some("00:DE:AD:BE:EF:00".to_string()),
        }),
        use_ipmitool: false,
        ..Default::default()
    });
    let api_result = env.api.admin_bmc_reset(req).await;
    let e = api_result.unwrap_err();
    assert!(e.code() == Code::InvalidArgument);
    // Note: The MAC address is generated from a sequence so we can't include it in the expected error
    assert!(e.message().contains("192.0.1.4 resolves to "));
    assert!(e.message().contains(" not 00:DE:AD:BE:EF:00"));

    // Check that we don't find what we're looking for.
    let req = tonic::Request::new(rpc::AdminBmcResetRequest {
        bmc_endpoint_request: Some(rpc::BmcEndpointRequest {
            ip_address: "0.0.0.0".to_string(),
            mac_address: None,
        }),
        use_ipmitool: false,
        ..Default::default()
    });
    let api_result = env.api.admin_bmc_reset(req).await;
    let e = api_result.unwrap_err();
    assert!(e.code() == Code::NotFound);

    // The topology copy may be missing the MAC, but machine_id lookup should
    // still recover it from the linked BMC machine_interface.
    let mut txn = db_pool.begin().await?;

    let query = "UPDATE machine_topologies SET topology = jsonb_set(topology, '{bmc_info}', $2::jsonb, false) WHERE machine_id = $1";
    let bmc_info = serde_json::json!({
        "ip": bmc_ip,
        "port": null,
        "version": "1",
        "firmware_version": "5.10",
    });
    let _ = sqlx::query(query)
        .bind(host_machine_id.to_string())
        .bind(sqlx::types::Json(bmc_info))
        .execute(txn.deref_mut())
        .await?;
    txn.commit().await?;

    let req = tonic::Request::new(rpc::AdminBmcResetRequest {
        bmc_endpoint_request: None,
        machine_id: Some(host_machine_id.to_string()),
        use_ipmitool: false,
        ..Default::default()
    });
    let api_result = env.api.admin_bmc_reset(req).await;
    assert!(api_result.is_ok());

    // The topology copy may be missing the IP or contain stale MAC data, but
    // machine_id lookup should still recover the current endpoint data from
    // the linked BMC machine_interface.
    let mut txn = db_pool.begin().await?;

    let query = "UPDATE machine_topologies SET topology = jsonb_set(topology, '{bmc_info}',  '{\"mac\": \"C8:4B:D6:7A:DB:66\", \"port\": null, \"version\": \"1\", \"firmware_version\": \"5.10\"}', false) WHERE machine_id = $1";
    let _ = sqlx::query(query)
        .bind(host_machine_id.to_string())
        .execute(txn.deref_mut())
        .await?;
    txn.commit().await?;

    let req = tonic::Request::new(rpc::AdminBmcResetRequest {
        bmc_endpoint_request: None,
        machine_id: Some(host_machine_id.to_string()),
        use_ipmitool: false,
        ..Default::default()
    });
    let api_result = env.api.admin_bmc_reset(req).await;
    assert!(api_result.is_ok());

    Ok(())
}

#[crate::sqlx_test()]
async fn test_find_explored_endpoint_firmware_versions(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool.clone()).await;

    let versions = HashMap::from([
        (FirmwareComponentType::Bmc, "25.06-2_NV_WW_02".to_string()),
        (FirmwareComponentType::Uefi, "00000083".to_string()),
        (FirmwareComponentType::HGXBmc, "97.00.B9.00.76".to_string()),
        (FirmwareComponentType::Cx7, "28.47.2682".to_string()),
    ]);

    let mut txn = env.pool.begin().await?;
    common::endpoint::insert_endpoint_with_firmware_versions(
        &mut txn,
        "141.219.24.1",
        versions.clone(),
    )
    .await?;
    txn.commit().await?;

    let id_list = env
        .api
        .find_explored_endpoint_ids(tonic::Request::new(
            ::rpc::site_explorer::ExploredEndpointSearchFilter {},
        ))
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(id_list.endpoint_ids.len(), 1);

    let request = tonic::Request::new(::rpc::site_explorer::ExploredEndpointsByIdsRequest {
        endpoint_ids: id_list.endpoint_ids,
    });

    let endpoint_list = env
        .api
        .find_explored_endpoints_by_ids(request)
        .await
        .map(|response| response.into_inner())
        .unwrap();
    assert_eq!(endpoint_list.endpoints.len(), 1);

    let report = endpoint_list.endpoints[0].report.as_ref().unwrap();
    let fw_versions = &report.firmware_versions;
    assert_eq!(fw_versions.len(), 4);
    assert_eq!(fw_versions.get("bmc").unwrap(), "25.06-2_NV_WW_02");
    assert_eq!(fw_versions.get("uefi").unwrap(), "00000083");
    assert_eq!(fw_versions.get("hgxbmc").unwrap(), "97.00.B9.00.76");
    assert_eq!(fw_versions.get("cx7").unwrap(), "28.47.2682");

    Ok(())
}

#[crate::sqlx_test]
async fn test_admin_bmc_reset_rejects_malformed_ip_address(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let req = tonic::Request::new(rpc::AdminBmcResetRequest {
        bmc_endpoint_request: Some(rpc::BmcEndpointRequest {
            ip_address: "not-an-ip".to_string(),
            mac_address: None,
        }),
        use_ipmitool: false,
        ..Default::default()
    });

    let err = env
        .api
        .admin_bmc_reset(req)
        .await
        .expect_err("expected malformed ip_address to be rejected");

    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(
        err.message().contains("invalid ip_address"),
        "message was: {}",
        err.message()
    );

    Ok(())
}
