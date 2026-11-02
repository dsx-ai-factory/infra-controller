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
use std::net::IpAddr;
use std::str::FromStr;
use std::sync::Arc;

use carbide_secrets::credentials::Credentials;
use carbide_secrets::test_support::credentials::TestCredentialManager;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use model::test_support::ManagedHostConfig;

#[test]
fn bmc_info_accepts_ipv6_from_proto() {
    let bmc_info = model::bmc_info::BmcInfo::try_from(rpc::forge::BmcInfo {
        ip: Some("2001:db8::1".into()),
        ..Default::default()
    })
    .unwrap();

    assert_eq!(bmc_info.ip, Some("2001:db8::1".parse().unwrap()));
}

#[test]
fn bmc_info_rejects_invalid_proto_ip() {
    let err = model::bmc_info::BmcInfo::try_from(rpc::forge::BmcInfo {
        ip: Some("not-an-ip".into()),
        ..Default::default()
    })
    .unwrap_err();

    assert!(err.to_string().contains("Invalid BMC IP"));
}

async fn init(pool: PgPool) -> (TestHarness, TestManagedHost) {
    let host_config = ManagedHostConfig::default();
    let env = TestHarness::builder(pool)
        .with_api_builder_fn(|builder| {
            builder.with_credential_manager(Arc::new(TestCredentialManager::new(
                Credentials::UsernamePassword {
                    username: "root".to_string(),
                    password: "notforprod".to_string(),
                },
            )))
        })
        .build()
        .await;
    let domain = env.test_domain().await;
    let network_controller = env.network_controller();
    let underlay_segment = network_controller.create_underlay_segment(&domain).await;
    network_controller.create_admin_segment(&domain).await;
    let site_explorer = env.default_test_site_explorer();
    let (mh, _) = env
        .managed_host_builder(&site_explorer, underlay_segment)
        .with_config(host_config)
        .build()
        .await;
    (env, mh)
}

#[sqlx_test]
async fn fetch_bmc_credentials(pool: PgPool) {
    let (env, mh) = init(pool).await;
    let host_bmc_mac = mh.host.bmc_mac;
    let host_machine = mh.host.rpc_machine().await;
    let bmc_info = host_machine.bmc_info.clone().unwrap();
    assert_eq!(bmc_info.mac, Some(host_bmc_mac.to_string()));
    let host_bmc_ip = bmc_info.ip.clone().expect("Host BMC IP must be available");

    for request in vec![
        rpc::forge::BmcMetaDataGetRequest {
            machine_id: host_machine.id,
            request_type: rpc::forge::BmcRequestType::Redfish.into(),
            role: rpc::forge::UserRoles::Administrator.into(),
            bmc_endpoint_request: None,
        },
        rpc::forge::BmcMetaDataGetRequest {
            machine_id: None,
            request_type: rpc::forge::BmcRequestType::Redfish.into(),
            role: rpc::forge::UserRoles::Administrator.into(),
            bmc_endpoint_request: Some(rpc::forge::BmcEndpointRequest {
                ip_address: host_bmc_ip.clone(),
                mac_address: None,
            }),
        },
    ]
    .into_iter()
    {
        let lookup_selector = if request.machine_id.is_some() {
            "machine_id"
        } else {
            "bmc_endpoint"
        };
        tracing::info!(lookup_selector, "Looking up BMC credentials");
        let metadata = env
            .api()
            .get_bmc_meta_data(tonic::Request::new(request))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(metadata.ip, host_bmc_ip);
        assert_eq!(metadata.port, None);
        assert_eq!(metadata.serial_console_ssh_port, None);
        assert_eq!(metadata.ipmi_port, None);
        assert_eq!(metadata.mac, host_bmc_mac.to_string());
        assert!(!metadata.password.is_empty());
        assert!(!metadata.user.is_empty());
    }
}

#[sqlx_test]
async fn test_fetch_ssh_serial_console_metadata(pool: PgPool) {
    let (env, mh) = init(pool).await;
    let host_machine = mh.host.rpc_machine().await;
    let host_bmc_ip = host_machine
        .bmc_info
        .as_ref()
        .and_then(|bmc_info| bmc_info.ip.clone())
        .expect("Host BMC IP must be available");

    let mut txn = env.db_txn().await;
    sqlx::query(
        "UPDATE explored_endpoints SET exploration_report = \
         jsonb_set(exploration_report, '{Systems,0,SerialConsoleSshPort}', '2222'::jsonb, true) \
         WHERE address = $1",
    )
    .bind(IpAddr::from_str(&host_bmc_ip).expect("invalid host IP"))
    .execute(txn.as_mut())
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let metadata = env
        .api()
        .get_bmc_meta_data(tonic::Request::new(rpc::forge::BmcMetaDataGetRequest {
            machine_id: host_machine.id,
            request_type: rpc::forge::BmcRequestType::Redfish.into(),
            role: rpc::forge::UserRoles::Administrator.into(),
            bmc_endpoint_request: None,
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(metadata.ip, host_bmc_ip);
    assert_eq!(metadata.serial_console_ssh_port, Some(2222));
}

#[sqlx_test]
async fn test_fetch_ssh_serial_console_metadata_ignores_invalid_ports(pool: PgPool) {
    let (env, mh) = init(pool).await;
    let host_machine = mh.host.rpc_machine().await;
    let host_bmc_ip = host_machine
        .bmc_info
        .as_ref()
        .and_then(|bmc_info| bmc_info.ip.clone())
        .expect("Host BMC IP must be available");
    let host_bmc_ip = IpAddr::from_str(&host_bmc_ip).expect("invalid host IP");

    for invalid_port in [r#""not-a-port""#, "0", "65536", "2147483648"] {
        let mut txn = env.db_txn().await;
        sqlx::query(
            "UPDATE explored_endpoints SET exploration_report = \
             jsonb_set(exploration_report, '{Systems,0,SerialConsoleSshPort}', $2::jsonb, true) \
             WHERE address = $1",
        )
        .bind(host_bmc_ip)
        .bind(invalid_port)
        .execute(txn.as_mut())
        .await
        .unwrap();
        txn.commit().await.unwrap();

        let metadata = env
            .api()
            .get_bmc_meta_data(tonic::Request::new(rpc::forge::BmcMetaDataGetRequest {
                machine_id: host_machine.id,
                request_type: rpc::forge::BmcRequestType::Redfish.into(),
                role: rpc::forge::UserRoles::Administrator.into(),
                bmc_endpoint_request: None,
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(
            metadata.serial_console_ssh_port, None,
            "stored port: {invalid_port}"
        );
    }
}

#[sqlx_test]
async fn test_fetch_ipmi_metadata(pool: PgPool) {
    let (env, mh) = init(pool).await;
    let host_bmc_mac = mh.host.bmc_mac;
    let host_machine = mh.host.rpc_machine().await;
    let bmc_info = host_machine.bmc_info.clone().unwrap();
    assert_eq!(bmc_info.mac, Some(host_bmc_mac.to_string()));
    let host_bmc_ip = bmc_info.ip.clone().expect("Host BMC IP must be available");

    let mut txn = env.db_txn().await;
    sqlx::query(
        "UPDATE explored_endpoints SET exploration_report = \
         jsonb_set(exploration_report, '{Managers,0,IpmiPort}', '1623'::jsonb, true) \
         WHERE address = $1",
    )
    .bind(IpAddr::from_str(&host_bmc_ip).expect("invalid host IP"))
    .execute(txn.as_mut())
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let metadata = env
        .api()
        .get_bmc_meta_data(tonic::Request::new(rpc::forge::BmcMetaDataGetRequest {
            machine_id: host_machine.id,
            request_type: rpc::forge::BmcRequestType::Ipmi.into(),
            role: rpc::forge::UserRoles::Administrator.into(),
            bmc_endpoint_request: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(metadata.ip, host_bmc_ip);
    assert_eq!(metadata.port, None);
    assert_eq!(metadata.ipmi_port, Some(1623));
    assert_eq!(metadata.mac, host_bmc_mac.to_string());
    assert!(!metadata.password.is_empty());
    assert!(!metadata.user.is_empty());
    assert!(metadata.vendor.is_some_and(|v| !v.is_empty()));
}

#[sqlx_test]
async fn test_fetch_ipmi_metadata_ignores_invalid_ports(pool: PgPool) {
    let (env, mh) = init(pool).await;
    let host_machine = mh.host.rpc_machine().await;
    let host_bmc_ip = host_machine
        .bmc_info
        .as_ref()
        .and_then(|bmc_info| bmc_info.ip.clone())
        .expect("Host BMC IP must be available");
    let host_bmc_ip = IpAddr::from_str(&host_bmc_ip).expect("invalid host IP");

    for invalid_port in [r#""not-a-port""#, "65536", "2147483648"] {
        let mut txn = env.db_txn().await;
        sqlx::query(
            "UPDATE explored_endpoints SET exploration_report = \
             jsonb_set(exploration_report, '{Managers,0,IpmiPort}', $2::jsonb, true) \
             WHERE address = $1",
        )
        .bind(host_bmc_ip)
        .bind(invalid_port)
        .execute(txn.as_mut())
        .await
        .unwrap();
        txn.commit().await.unwrap();

        let metadata = env
            .api()
            .get_bmc_meta_data(tonic::Request::new(rpc::forge::BmcMetaDataGetRequest {
                machine_id: host_machine.id,
                request_type: rpc::forge::BmcRequestType::Ipmi.into(),
                role: rpc::forge::UserRoles::Administrator.into(),
                bmc_endpoint_request: None,
            }))
            .await
            .unwrap()
            .into_inner();

        assert_eq!(metadata.ipmi_port, None, "stored port: {invalid_port}");
        assert!(metadata.vendor.is_some_and(|vendor| !vendor.is_empty()));
    }
}

#[sqlx_test]
async fn test_fetch_ipmi_metadata_null_vendor(pool: PgPool) {
    let (env, mh) = init(pool).await;
    let host_bmc_mac = mh.host.bmc_mac;
    let host_machine = mh.host.rpc_machine().await;
    let bmc_info = host_machine.bmc_info.clone().unwrap();
    assert_eq!(bmc_info.mac, Some(host_bmc_mac.to_string()));
    let host_bmc_ip = bmc_info.ip.clone().expect("Host BMC IP must be available");

    // Set the Vendor to a null string to test handling
    let query = "UPDATE explored_endpoints SET exploration_report = jsonb_set(exploration_report, '{Vendor}', 'null'::jsonb) WHERE address = $1";
    let mut txn = env.db_txn().await;
    sqlx::query(query)
        .bind(IpAddr::from_str(&host_bmc_ip).expect("invalid host IP"))
        .execute(txn.as_mut())
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let metadata = env
        .api()
        .get_bmc_meta_data(tonic::Request::new(rpc::forge::BmcMetaDataGetRequest {
            machine_id: host_machine.id,
            request_type: rpc::forge::BmcRequestType::Ipmi.into(),
            role: rpc::forge::UserRoles::Administrator.into(),
            bmc_endpoint_request: None,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(metadata.ip, host_bmc_ip);
    assert_eq!(metadata.port, None);
    assert_eq!(metadata.mac, host_bmc_mac.to_string());
    assert!(!metadata.password.is_empty());
    assert!(!metadata.user.is_empty());
    assert!(metadata.vendor.is_none());
}
