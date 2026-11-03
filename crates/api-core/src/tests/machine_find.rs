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
use std::error::Error as _;
use std::net::IpAddr;

use carbide_uuid::machine::{
    MACHINE_ID_PREFIX_LENGTH, MachineId, MachineIdSource, MachineType, StableHostMachineId,
};
use carbide_uuid::rack::RackId;
use common::api_fixtures::dpu::create_dpu_machine;
use common::api_fixtures::{create_managed_host, create_test_env, site_explorer};
use db::{AnnotatedSqlxError, ObjectFilter};
use health_report::{
    HealthAlertClassification, HealthProbeAlert, HealthReport, HealthReportApplyMode,
};
use itertools::Itertools;
use mac_address::MacAddress;
use model::expected_machine::ExpectedMachineData;
use model::hardware_info::HardwareInfo;
use model::machine::ManagedHostState;
use model::machine::machine_id::host_id_from_dpu_hardware_info;
use model::machine::machine_search_config::MachineSearchConfig;
use model::test_support::ManagedHostConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::{AssociateMachinesWithInstanceTypeRequest, FindInstanceTypeIdsRequest};

use crate::errors::{CarbideError, CarbideErrorWithLocation};
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::test_support::mac_address_pool::DPU_OOB_MAC_ADDRESS_POOL;
use crate::tests::common;
use crate::tests::common::api_fixtures::site_explorer::TestRackDbBuilder;
use crate::tests::common::api_fixtures::{
    create_managed_host_multi_dpu, create_managed_host_with_config,
};
use crate::tests::sku::tests::FULL_SKU_DATA;

#[crate::sqlx_test]
async fn test_update_machine_metadata_preserves_snapshot_decode_error(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

    // Corrupt persisted state so `load_machine` fails after acquiring its transaction.
    let updated = sqlx::query("UPDATE machines SET controller_state = '{}'::jsonb WHERE id = $1")
        .bind(dpu_machine_id)
        .execute(&env.pool)
        .await
        .unwrap();
    assert_eq!(updated.rows_affected(), 1);

    let error = env
        .api
        .update_machine_metadata(tonic::Request::new(
            rpc::forge::MachineMetadataUpdateRequest {
                machine_id: Some(dpu_machine_id.into()),
                metadata: Some(rpc::forge::Metadata {
                    name: "machine-with-invalid-state".to_string(),
                    ..Default::default()
                }),
                if_version_match: None,
            },
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::Internal);
    assert_eq!(
        error.metadata().get("nico-error-code").unwrap(),
        "NICO-API-500"
    );
    let source = error
        .source()
        .expect("status should retain its error source")
        .downcast_ref::<CarbideErrorWithLocation>()
        .expect("status source should retain the CarbideError");
    // `Machine::from_row` decodes the whole snapshot from column 0.
    assert!(matches!(
        &source.error,
        CarbideError::DBError(AnnotatedSqlxError {
            source: sqlx::Error::ColumnDecode { index, .. },
            ..
        }) if index == "0"
    ));
    assert_eq!(
        error.metadata().get("nico-error-text").unwrap(),
        error.message()
    );
}

#[crate::sqlx_test]
async fn test_update_machine_metadata_preserves_not_found(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let missing_machine_id = MachineId::new(MachineIdSource::Tpm, [255; 32], MachineType::Dpu);

    let error = env
        .api
        .update_machine_metadata(tonic::Request::new(
            rpc::forge::MachineMetadataUpdateRequest {
                machine_id: Some(missing_machine_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "missing-machine".to_string(),
                    ..Default::default()
                }),
                if_version_match: None,
            },
        ))
        .await
        .unwrap_err();

    assert_eq!(error.code(), tonic::Code::NotFound);
    assert_eq!(
        error.message(),
        format!("machine not found: {missing_machine_id}")
    );
    assert_eq!(
        error.metadata().get("nico-error-code").unwrap(),
        "NICO-API-404"
    );
    assert_eq!(
        error.metadata().get("nico-error-text").unwrap(),
        error.message()
    );
}

#[crate::sqlx_test]
async fn test_find_machine_by_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let mut txn = env.pool.begin().await.unwrap();

    let machine = db::machine::find_by_query(&mut txn, &dpu_machine_id.to_string())
        .await
        .unwrap()
        .expect("expect DPU to be found");
    assert_eq!(machine.id, dpu_machine_id.into());
    assert!(machine.is_dpu());

    // We shouldn't find a machine that doesn't exist
    let mut new_id = dpu_machine_id.to_string();
    // SAFETY: machine IDs are rendered entirely as ASCII. Replacing one byte with another
    // ASCII byte preserves the string's UTF-8 invariant, while `get_mut` still checks the index.
    match unsafe { new_id.as_bytes_mut().get_mut(MACHINE_ID_PREFIX_LENGTH + 1) } {
        Some(c) if *c == b'a' => *c = b'b',
        Some(c) => *c = b'a',
        None => panic!("Not expected"),
    }
    let id2: MachineId = new_id.parse().unwrap();
    assert!(
        db::machine::find_by_query(&mut txn, &id2.to_string())
            .await
            .unwrap()
            .is_none()
    );
}

#[crate::sqlx_test]
async fn test_find_machines_by_ids_returns_unassociated_dpus(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_id = create_dpu_machine(&env, &host_config).await;
    let machine_id: MachineId = dpu_id.into();
    let missing_id = MachineId::new(MachineIdSource::Tpm, [255; 32], MachineType::Dpu);

    let neighbor = model::lldp::LldpNeighbor {
        local_mac_address: "02:00:00:00:00:42".parse().unwrap(),
        local_port: "p0".to_string(),
        chassis_id_type: "mac".to_string(),
        chassis_id_value: "02:00:00:00:00:43".to_string(),
        remote_port_type: "ifname".to_string(),
        remote_port_value: "swp1".to_string(),
        system_name: "switch".to_string(),
        system_description: String::new(),
        management_addresses: Vec::new(),
        med_serial: None,
        med_manufacturer: None,
        med_model: None,
    };

    let mut txn = env.pool.begin().await.unwrap();

    // Remove the fixture's predicted-host association while retaining the DPU row.
    sqlx::query("UPDATE machine_interfaces SET attached_dpu_machine_id = NULL WHERE attached_dpu_machine_id = $1 AND machine_id != $1")
        .bind(dpu_id)
        .execute(&mut *txn)
        .await
        .unwrap();

    let dpu = db::machine::find_one(&mut *txn, &dpu_id, MachineSearchConfig::default())
        .await
        .unwrap()
        .unwrap();

    db::machine::advance(&dpu, &mut txn, &ManagedHostState::Created, None)
        .await
        .unwrap();

    db::machine_lldp_neighbor::replace_all(&mut txn, &machine_id, std::slice::from_ref(&neighbor))
        .await
        .unwrap();

    assert!(
        db::machine::lookup_host_machine_ids_by_dpu_ids(&mut *txn, &[dpu_id])
            .await
            .unwrap()
            .is_empty()
    );

    txn.commit().await.unwrap();

    let ids = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            include_dpus: true,
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machine_ids;

    assert!(ids.contains(&machine_id));

    let mut exclusion = HealthReport::empty("test-sla-exclusion".to_string());

    exclusion.alerts.push(HealthProbeAlert {
        classifications: vec![HealthAlertClassification::exclude_from_state_machine_sla()],
        ..HealthProbeAlert::missing_report()
    });

    let cases = [
        ("history omitted", false, None, true),
        ("history included", true, None, true),
        (
            "merged SLA exclusion",
            true,
            Some((HealthReportApplyMode::Merge, exclusion)),
            false,
        ),
        (
            "replacement overrides merged exclusion",
            true,
            Some((
                HealthReportApplyMode::Replace,
                HealthReport::empty("test-replace".to_string()),
            )),
            true,
        ),
    ];

    for (label, include_history, health_override, has_sla) in cases {
        if let Some((mode, report)) = health_override {
            let mut txn = env.pool.begin().await.unwrap();

            db::machine::insert_health_report(&mut txn, &machine_id, mode, &report, false)
                .await
                .unwrap();

            txn.commit().await.unwrap();
        }

        let machines = env
            .api
            .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
                machine_ids: vec![machine_id, missing_id],
                include_history,
            }))
            .await
            .unwrap()
            .into_inner()
            .machines;

        assert_eq!(machines.len(), 1, "{label}");

        let standalone = machines
            .iter()
            .find(|machine| machine.id == Some(machine_id))
            .expect(label);

        let status = standalone.status.as_ref().unwrap();

        assert_eq!(status.associated_host_machine_id, None, "{label}");
        assert_eq!(!standalone.events.is_empty(), include_history, "{label}");

        assert_eq!(
            status.lldp_neighbors,
            vec![rpc::forge::InterfaceLldp::from(neighbor.clone())],
            "{label}"
        );

        let sla = standalone.state_sla.as_ref().expect(label);

        assert_eq!(
            status.lifecycle.as_ref().unwrap().sla.as_ref(),
            Some(sla),
            "{label}"
        );

        assert_eq!(sla.sla.is_some(), has_sla, "{label}");
    }
}

#[crate::sqlx_test]
async fn test_find_machine_by_ip(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_machine = db::machine::find_one(
        txn.as_mut(),
        &dpu_machine_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let ip = &dpu_machine.status.interfaces[0].addresses[0];

    let machine = db::machine::find_by_query(&mut txn, &ip.to_string())
        .await
        .unwrap()
        .expect("expect DPU to be found");
    assert_eq!(machine.id, dpu_machine_id.into());
    assert_eq!(&machine.status.interfaces[0].addresses[0], ip);

    // We shouldn't find a machine that doesn't exist
    let ip2: IpAddr = "254.254.254.254".parse().unwrap();
    assert!(
        db::machine::find_by_query(&mut txn, &ip2.to_string())
            .await
            .unwrap()
            .is_none()
    );
}

#[crate::sqlx_test]
// test_find_machine_by_ipv6 is a super basic unit test to just
// ensure `find_by_query` changes to accommodate `IpAddr` (and
// as such IPv6), work as expected. This compliments the unit
// test "test_find_machine_by_ip" above, which is IPv4.
async fn test_find_machine_by_ipv6(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_machine = db::machine::find_one(
        txn.as_mut(),
        &dpu_machine_id,
        MachineSearchConfig::default(),
    )
    .await
    .unwrap()
    .unwrap();
    let interface_id = dpu_machine.status.interfaces[0].id;

    // Add an IPv6 address to the interface.
    let ipv6: IpAddr = "fd00::100".parse().unwrap();
    sqlx::query("INSERT INTO machine_interface_addresses (interface_id, address) VALUES ($1, $2)")
        .bind(interface_id)
        .bind(ipv6)
        .execute(&mut *txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Look up the machine by its IPv6 address.
    let mut txn = pool.begin().await.unwrap();
    let machine = db::machine::find_by_query(&mut txn, "fd00::100")
        .await
        .unwrap()
        .expect("should find machine by IPv6 address");
    assert_eq!(machine.id, dpu_machine_id.into());
}

#[crate::sqlx_test]
async fn test_find_machine_without_sku(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let mut txn = env.pool.begin().await.unwrap();

    let machine = mh.host().db_machine(&mut txn).await;

    assert_eq!(machine.config.hw_sku, None);
}

#[crate::sqlx_test]
async fn test_find_machine_with_sku(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let sku = serde_json::de::from_str::<rpc::forge::Sku>(FULL_SKU_DATA)
        .unwrap()
        .into();

    let mut txn = env.pool.begin().await.unwrap();
    db::sku::create(&mut txn, &sku).await.unwrap();
    txn.commit().await.unwrap();

    let sku_id = "sku id".to_string();
    let host_config =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            sku_id: Some(sku_id.clone()),
            ..Default::default()
        });
    let mh = create_managed_host_with_config(&env, host_config).await;

    let machine = mh.host().rpc_machine().await;
    assert_eq!(
        machine.config.as_ref().and_then(|c| c.hw_sku.as_ref()),
        Some(&sku_id)
    );
}

#[crate::sqlx_test]
async fn test_find_machine_without_rack_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;

    let machine = mh.host().rpc_machine().await;
    assert!(machine.rack_id.is_none());

    let machines = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            rack_id: Some("rack1".to_string().into()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machine_ids;
    assert!(machines.is_empty());
}

#[crate::sqlx_test]
async fn test_find_machine_by_rack_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let rack_id: RackId = "Rack1".parse().unwrap();

    let mut txn = env.pool.acquire().await.unwrap();
    TestRackDbBuilder::new()
        .with_rack_id(rack_id.clone())
        .persist(&mut txn)
        .await
        .unwrap();
    drop(txn);

    let host_config =
        ManagedHostConfig::default().with_expected_machine_data(ExpectedMachineData {
            rack_id: rack_id.clone().into(),
            ..Default::default()
        });
    let mh = create_managed_host_with_config(&env, host_config).await;

    let machine = mh.host().rpc_machine().await;
    let machine_id = mh.id;
    assert_eq!(machine.rack_id.as_ref(), Some(&rack_id));

    let machine_ids = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            rack_id: Some(rack_id),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machine_ids;

    assert_eq!(machine_ids.len(), 1);
    assert_eq!(machine_ids[0], machine_id.into());
}

#[crate::sqlx_test]
async fn test_find_machine_by_mac(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_machine = db::machine::find_one(
        txn.as_mut(),
        &dpu_machine_id,
        MachineSearchConfig {
            include_history: true,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let mac = &dpu_machine.status.interfaces[0].mac_address;

    let machine = db::machine::find_by_query(&mut txn, &mac.to_string())
        .await
        .unwrap()
        .expect("expect DPU to be found");
    assert_eq!(machine.id, dpu_machine_id.into());
    assert_eq!(&machine.status.interfaces[0].mac_address, mac);
    assert!(DPU_OOB_MAC_ADDRESS_POOL.contains(machine.status.interfaces[0].mac_address));

    // We shouldn't find a machine that doesn't exist
    let mut mac2 = mac.bytes();
    // Previously just set to 0xFF, but that could be the actual value
    mac2[5] ^= 0xFF;
    let mac2 = MacAddress::from(mac2);
    assert!(
        db::machine::find_by_query(&mut txn, &mac2.to_string())
            .await
            .unwrap()
            .is_none()
    );
}

#[crate::sqlx_test]
async fn test_find_machine_by_hostname(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;

    let mut txn = env.pool.begin().await.unwrap();
    let dpu_machine = db::machine::find_one(
        txn.as_mut(),
        &dpu_machine_id,
        MachineSearchConfig {
            include_history: true,
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .unwrap();
    let hostname = &dpu_machine.status.interfaces[0].hostname.clone();

    let machine = db::machine::find_by_query(&mut txn, hostname)
        .await
        .unwrap()
        .expect("expect DPU to be found");
    assert_eq!(machine.id, dpu_machine_id.into());
    assert_eq!(&machine.status.interfaces[0].hostname, hostname);

    // We shouldn't find a machine that doesn't exist
    let hostname2 = format!("a{hostname}");
    assert!(
        db::machine::find_by_query(&mut txn, &hostname2)
            .await
            .unwrap()
            .is_none()
    );
}

#[crate::sqlx_test]
async fn test_find_machine_ids_with_and_without_dpus(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let (_host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    // With DPUs
    let machine_ids = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            include_dpus: true,
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machine_ids;
    assert_eq!(machine_ids.len(), 2); // 1 host and 1 DPU

    let machine_types = machine_ids
        .into_iter()
        .map(|x| x.machine_type())
        .collect_vec();

    assert!(machine_types.contains(&MachineType::Host));
    assert!(machine_types.contains(&MachineType::Dpu));

    // No DPUs
    let machine_ids = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            include_dpus: false,
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machine_ids;
    assert_eq!(machine_ids.len(), 1); // 1 host

    assert_eq!(machine_ids[0].machine_type(), MachineType::Host);
}

#[crate::sqlx_test]
async fn test_find_all_machines_when_there_arent_any(pool: sqlx::PgPool) {
    let machines = db::machine::find(
        &pool,
        ObjectFilter::<MachineId>::All,
        crate::tests::machine_find::MachineSearchConfig {
            include_history: true,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert!(machines.is_empty());
}

#[crate::sqlx_test]
async fn test_find_machine_ids(pool: sqlx::PgPool) {
    let config = crate::tests::machine_find::MachineSearchConfig {
        include_dpus: true,
        include_predicted_host: true,
        ..Default::default()
    };

    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_machine_id = host_id_from_dpu_hardware_info(&HardwareInfo::from(
        host_config.get_and_assert_single_dpu(),
    ))
    .unwrap();
    let mut txn = env.pool.begin().await.unwrap();

    let machine_ids = db::machine::find_machine_ids::<MachineId>(txn.as_mut(), config)
        .await
        .unwrap();

    assert_eq!(machine_ids.len(), 2);
    assert!(machine_ids.contains(&dpu_machine_id));
    assert!(machine_ids.contains(&host_machine_id));

    // Create a managed host
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    // Find an existing instance type in the test env
    let instance_type_id = env
        .api
        .find_instance_type_ids(tonic::Request::new(FindInstanceTypeIdsRequest {}))
        .await
        .unwrap()
        .into_inner()
        .instance_type_ids
        .first()
        .unwrap()
        .to_owned();

    // Associate the machine with the instance type
    let _ = env
        .api
        .associate_machines_with_instance_type(tonic::Request::new(
            AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![host_machine_id.to_string()],
            },
        ))
        .await
        .unwrap();

    // Create a config to test searching by instance type id
    let config = crate::tests::machine_find::MachineSearchConfig {
        instance_type_id: Some(instance_type_id.parse().unwrap()),
        ..Default::default()
    };

    // Try to find machines for the instance type.
    let machine_ids = db::machine::find_machine_ids::<StableHostMachineId>(txn.as_mut(), config)
        .await
        .unwrap();

    assert_eq!(machine_ids.len(), 1);
    assert_eq!(machine_ids[0], host_machine_id);
}

#[crate::sqlx_test]
async fn test_find_dpu_machine_ids(pool: sqlx::PgPool) {
    let config = crate::tests::machine_find::MachineSearchConfig {
        include_dpus: true,
        exclude_hosts: true,
        ..Default::default()
    };

    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_machine_id = host_id_from_dpu_hardware_info(&HardwareInfo::from(
        host_config.get_and_assert_single_dpu(),
    ))
    .unwrap();
    let mut txn = env.pool.begin().await.unwrap();

    let machine_ids = db::machine::find_machine_ids::<MachineId>(txn.as_mut(), config)
        .await
        .unwrap();

    assert_eq!(machine_ids.len(), 1);
    assert!(machine_ids.contains(&dpu_machine_id));
    assert!(!machine_ids.contains(&host_machine_id));
}

#[crate::sqlx_test]
async fn test_find_predicted_host_machine_ids(pool: sqlx::PgPool) {
    let config = crate::tests::machine_find::MachineSearchConfig {
        include_predicted_host: true,
        ..Default::default()
    };

    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let host_machine_id = host_id_from_dpu_hardware_info(&HardwareInfo::from(
        host_config.get_and_assert_single_dpu(),
    ))
    .unwrap();
    let mut txn = env.pool.begin().await.unwrap();

    let machine_ids = db::machine::find_machine_ids::<MachineId>(txn.as_mut(), config)
        .await
        .unwrap();

    assert_eq!(machine_ids.len(), 1);
    assert!(!machine_ids.contains(&dpu_machine_id));
    assert!(machine_ids.contains(&host_machine_id));
}

#[crate::sqlx_test]
async fn test_find_host_machine_ids_when_predicted(pool: sqlx::PgPool) {
    let config = crate::tests::machine_find::MachineSearchConfig::default();

    let env = create_test_env(pool).await;
    let host_config = env.managed_host_config();
    let _dpu_machine_id = create_dpu_machine(&env, &host_config).await;
    let mut txn = env.pool.begin().await.unwrap();

    let machine_ids = db::machine::find_machine_ids::<MachineId>(txn.as_mut(), config)
        .await
        .unwrap();

    assert!(machine_ids.is_empty());
}

#[crate::sqlx_test]
async fn test_find_host_machine_ids(pool: sqlx::PgPool) {
    let config = crate::tests::machine_find::MachineSearchConfig::default();

    let env = create_test_env(pool).await;
    let (host_machine_id, _) = create_managed_host(&env).await.into();

    let mut txn = env.pool.begin().await.unwrap();

    tracing::info!("finding machine ids");
    let machine_ids = db::machine::find_machine_ids(txn.as_mut(), config)
        .await
        .unwrap();
    assert_eq!(machine_ids.len(), 1);
    assert!(machine_ids.contains(&host_machine_id));
}

#[crate::sqlx_test]
async fn test_find_mixed_host_machine_ids(pool: sqlx::PgPool) {
    let config = crate::tests::machine_find::MachineSearchConfig {
        include_predicted_host: true,
        ..Default::default()
    };

    let env = create_test_env(pool).await;
    let (host_machine_id, _) = create_managed_host(&env).await.into();

    let host_config2 = env.managed_host_config();
    create_dpu_machine(&env, &host_config2).await;
    let predicted_host_machine_id = host_id_from_dpu_hardware_info(&HardwareInfo::from(
        host_config2.get_and_assert_single_dpu(),
    ))
    .unwrap();

    let mut txn = env.pool.begin().await.unwrap();

    tracing::info!("finding machine ids");
    let machine_ids = db::machine::find_machine_ids::<MachineId>(txn.as_mut(), config)
        .await
        .unwrap();
    assert_eq!(machine_ids.len(), 2);
    assert!(machine_ids.contains(&host_machine_id));
    assert!(machine_ids.contains(&predicted_host_machine_id));
}

#[crate::sqlx_test]
async fn test_attached_dpu_machine_ids_multi_dpu(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;

    // Now host1 should have two DPUs.
    let host_machine = mh.host().rpc_machine().await;
    let dpu_ids = host_machine.status.unwrap().associated_dpu_machine_ids;
    assert_eq!(
        dpu_ids.len(),
        2,
        "host machine should have had 2 DPU IDs, got {}",
        dpu_ids.len()
    );

    for n in 0..2 {
        let dpu_id = mh.dpu_n(n).id;
        assert!(
            dpu_ids.contains(&dpu_id),
            "host machine is missing associated_dpu_machine_id {dpu_id}"
        );
    }
}

// The empty-list and over-max guards for `find_machines_by_ids` are shared
// API-layer code, proven once across representative RPCs in
// `tests::find_by_ids_guards`.

#[crate::sqlx_test]
async fn test_machine_capabilities_response(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Create a new managed host in the DB and get the snapshot.
    let mh = site_explorer::new_host(&env, ManagedHostConfig::default())
        .await
        .unwrap();

    // Convert the caps of the Machine to the proto representation
    // for later comparison.
    let mut caps = mh.host_snapshot.to_capabilities().unwrap();

    // Make sure we have at least _something_ in the capabilities.
    // CPU should be a safe one to rely on.  If we don't have CPUs,
    // we've got bad test data.
    assert!(!caps.cpu.is_empty());

    caps.sort();
    let caps_from_machine = rpc::protos::forge::MachineCapabilitiesSet::from(caps);

    // Find the new host through the API
    let machine = env
        .api
        .find_machines_by_ids(tonic::Request::new(rpc::forge::MachinesByIdsRequest {
            include_history: false,
            machine_ids: vec![mh.host_snapshot.id.into()],
        }))
        .await
        .unwrap()
        .into_inner()
        .machines
        .pop()
        .unwrap();

    let caps_from_rpc_call = machine.status.unwrap().capabilities.unwrap();

    // Check the gRPC response and the original machine agree
    assert_eq!(caps_from_rpc_call, caps_from_machine);

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_machine_by_instance_type(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    // Find the existing instance types in the test env
    let existing_instance_type_ids = env
        .api
        .find_instance_type_ids(tonic::Request::new(
            rpc::forge::FindInstanceTypeIdsRequest {},
        ))
        .await
        .unwrap()
        .into_inner()
        .instance_type_ids;

    let existing_instance_types = env
        .api
        .find_instance_types_by_ids(tonic::Request::new(
            rpc::forge::FindInstanceTypesByIdsRequest {
                instance_type_ids: existing_instance_type_ids,
                include_allocation_stats: false,
                tenant_organization_id: None,
            },
        ))
        .await
        .unwrap()
        .into_inner()
        .instance_types;

    // Our known fixture instance type
    let instance_type_id = existing_instance_types[0].id.clone();

    let (tmp_machine_id, _) = create_managed_host(&env).await.into();

    // Find the new host through the API
    let machines = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            instance_type_id: Some(instance_type_id.clone()),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machine_ids;

    // We should find nothing because we haven't associated our machine with
    // an instance type
    assert!(machines.is_empty());

    // Associate the machine with the instance type
    let _ = env
        .api
        .associate_machines_with_instance_type(tonic::Request::new(
            rpc::forge::AssociateMachinesWithInstanceTypeRequest {
                instance_type_id: instance_type_id.clone(),
                machine_ids: vec![tmp_machine_id.to_string()],
            },
        ))
        .await
        .unwrap();

    // Find the new host through the API
    let machines = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            instance_type_id: Some(instance_type_id),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner()
        .machine_ids;

    // We should now find our machine
    assert_eq!(machines.len(), 1);

    // Confirm that what we found is the right
    // machine
    assert_eq!(machines[0], tmp_machine_id.into());

    Ok(())
}
