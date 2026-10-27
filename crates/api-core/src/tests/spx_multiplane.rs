/*
 * SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
 * SPDX-License-Identifier: Apache-2.0
 */

use carbide_uuid::machine::{DpuMachineId, MachineId};
use model::expected_machine::ExpectedInterface;
use rpc::forge;
use rpc::forge::forge_server::Forge;
use tonic::Request;

use crate::cfg::file::EwEthersConfig;
use crate::tests::common::api_fixtures::{self, TestEnv, TestEnvOverrides, TestManagedHost};

async fn fixture(pool: sqlx::PgPool) -> (TestEnv, TestManagedHost) {
    fixture_with_dpus(pool, 1).await
}

async fn fixture_with_dpus(pool: sqlx::PgPool, count: usize) -> (TestEnv, TestManagedHost) {
    let mut config = api_fixtures::get_config();
    config.ewethers_config = Some(EwEthersConfig {
        enabled: true,
        astra_enabled: true,
        ..Default::default()
    });
    let env = api_fixtures::create_test_env_with_overrides(
        pool,
        TestEnvOverrides {
            config: Some(config),
            ..Default::default()
        },
    )
    .await;
    let mut host = api_fixtures::create_managed_host_multi_dpu(&env, count.max(1)).await;
    if count == 0 {
        // Retain valid host inventory while removing all associated DPUs. The
        // standard ingestion fixture requires a DPU to establish host DHCP.
        sqlx::query(
            "UPDATE machine_interfaces SET attached_dpu_machine_id=NULL WHERE machine_id=$1",
        )
        .bind(host.id)
        .execute(&env.pool)
        .await
        .unwrap();
        host.dpu_ids.clear();
    }
    // Exercise maintenance on discovered BF4 DPUs without invoking the separate
    // DPF provisioning path (which has its own single-Astra-DPU restriction).
    for dpu in &host.dpu_ids {
        sqlx::query("UPDATE machine_topologies SET topology=jsonb_set(topology, '{discovery_data,Info,dmi_data,product_name}', $2::jsonb) WHERE machine_id=$1")
            .bind(dpu).bind(serde_json::json!("BlueField-4 SmartNIC Main Card"))
            .execute(&env.pool).await.unwrap();
    }
    let mut txn = env.db_txn().await;
    let machine = host.host().db_machine(&mut txn).await;
    let mut expected = db::expected_machine::find_by_bmc_mac_address(
        &mut *txn,
        machine.status.bmc_info.mac.unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    for rail in 0..4 {
        for plane in 0..8 {
            expected.data.interfaces.push(ExpectedInterface {
                logical_ifname: Some(format!("eth_r{rail}_p{plane}")),
                physical_ifname: Some(
                    model::machine::astra::ASTRA_NIC_MAPPINGS[(rail * 8 + plane) as usize]
                        .physical_ifname
                        .to_string(),
                ),
                nic_type: Some("CX9".to_string()),
                ..Default::default()
            });
        }
    }
    db::expected_machine::update(&mut txn, &expected)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    (env, host)
}

fn update_request(
    id: MachineId,
    rail: Option<u32>,
    plane: u32,
    mode: bool,
    reference: Option<&str>,
) -> forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
    use forge::machine_spx_multiplane_maintenance_update_request::RailSelector;
    forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
        machine_id: Some(id),
        rail_selector: Some(
            rail.map(RailSelector::Rail)
                .unwrap_or(RailSelector::AllRails(())),
        ),
        plane_selector: Some(
            forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::Plane(plane),
        ),
        maintenance_mode: Some(mode),
        reference: reference.map(str::to_string),
    }
}

async fn persisted(env: &TestEnv, id: MachineId) -> serde_json::Value {
    sqlx::query_scalar("SELECT spx_multiplane FROM machines WHERE id=$1")
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

async fn persisted_dpu(env: &TestEnv, id: DpuMachineId) -> serde_json::Value {
    sqlx::query_scalar("SELECT spx_multiplane_status FROM machines WHERE id=$1")
        .bind(id)
        .fetch_one(&env.pool)
        .await
        .unwrap()
}

#[crate::sqlx_test]
async fn spx_multiplane_readback_persists_without_attachments_and_rejects_stale_status(
    pool: sqlx::PgPool,
) {
    let (env, host) = fixture(pool).await;
    let id: MachineId = host.id.into();
    // Before an operator request, readback is stored without configuring a mode
    // or treating an observed enabled plane as a mismatch.
    let initial = env
        .api
        .get_machine_spx_multiplane(Request::new(forge::MachineSpxMultiplaneRequest {
            machine_id: Some(id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(initial.interfaces[0].cfg_maintenance_mode.is_none());
    env.api
        .record_dpu_network_status(Request::new(forge::DpuNetworkStatus {
            dpu_machine_id: Some(host.dpu().id),
            dpu_health: Some(rpc::health::HealthReport {
                source: "forge-dpu-agent".to_string(),
                ..Default::default()
            }),
            astra_config_status: Some(forge::AstraConfigStatus {
                is_attachment_status_present: Some(false),
                is_maintenance_status_present: Some(true),
                multiplane_interfaces_status: vec![forge::AstraMultiplaneInterfaceStatus {
                    logical_ifname: "eth_r0_p0".to_string(),
                    revision: initial.interfaces[0].revision.clone(),
                    maintenance_mode: Some(false),
                    observed_at: Some(chrono::Utc::now().into()),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        }))
        .await
        .unwrap();
    let observed = env
        .api
        .get_machine_spx_multiplane(Request::new(forge::MachineSpxMultiplaneRequest {
            machine_id: Some(id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(observed.interfaces[0].cfg_maintenance_mode.is_none());
    assert_eq!(
        observed.dpu_statuses[0].interfaces[0].maintenance_mode,
        Some(false)
    );
    assert!(observed.dpu_statuses[0].interfaces[0].error.is_none());
    let requested = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(update_request(
            id,
            Some(0),
            0,
            false,
            None,
        )))
        .await
        .unwrap()
        .into_inner();
    let revision = requested.interfaces[0].revision.clone();
    let observed_at = chrono::Utc::now();
    let read = || {
        env.api
            .get_machine_spx_multiplane(Request::new(forge::MachineSpxMultiplaneRequest {
                machine_id: Some(id),
            }))
    };
    let report = |mode, revision: String, offset, is_maintenance_status_present| {
        env.api
            .record_dpu_network_status(Request::new(forge::DpuNetworkStatus {
                dpu_machine_id: Some(host.dpu().id),
                dpu_health: Some(rpc::health::HealthReport {
                    source: "forge-dpu-agent".to_string(),
                    ..Default::default()
                }),
                astra_config_status: Some(forge::AstraConfigStatus {
                    is_attachment_status_present: Some(false),
                    is_maintenance_status_present,
                    multiplane_interfaces_status: vec![forge::AstraMultiplaneInterfaceStatus {
                        logical_ifname: "eth_r0_p0".to_string(),
                        revision,
                        maintenance_mode: Some(mode),
                        netdev: Some("A56p0".to_string()),
                        observed_at: Some((observed_at + chrono::Duration::seconds(offset)).into()),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }))
    };
    report(false, revision.clone(), 0, Some(true))
        .await
        .unwrap();
    let applied = read().await.unwrap().into_inner();
    let port = &applied.dpu_statuses[0].interfaces[0];
    assert_eq!(port.maintenance_mode, Some(false));
    assert_eq!(port.revision, revision);
    assert_eq!(port.netdev.as_deref(), Some("A56p0"));
    assert!(port.error.is_none());
    assert_eq!(
        applied.interfaces[0].revision, revision,
        "readback does not bump desired revisions"
    );
    let stored = persisted_dpu(&env, host.dpu().id).await;
    assert_eq!(stored["interfaces"]["eth_r0_p0"]["maintenance_mode"], false);
    let host_stored = persisted(&env, id).await;
    assert!(
        host_stored["interfaces"]["eth_r0_p0"]
            .get("observation")
            .is_none()
    );

    // Presence flags preserve previous observations; omitted flags retain existing semantics.
    // Mismatch detection remains independent of an agent-provided error string.
    for (presence, expected_mode) in [(Some(false), false), (None, true)] {
        report(true, revision.clone(), 1, presence).await.unwrap();
        let result = read().await.unwrap().into_inner();
        let port = &result.dpu_statuses[0].interfaces[0];
        assert_eq!(port.maintenance_mode, Some(expected_mode));
        assert_eq!(port.error.is_some(), expected_mode);
    }
    report(false, revision.clone(), 2, Some(true))
        .await
        .unwrap();
    report(true, revision.clone(), 1, Some(true)).await.unwrap();
    let recovered = read().await.unwrap().into_inner();
    assert!(recovered.dpu_statuses[0].interfaces[0].error.is_none());
    assert_eq!(
        recovered.dpu_statuses[0].interfaces[0].maintenance_mode,
        Some(false)
    );

    let changed = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(update_request(
            id,
            Some(0),
            0,
            true,
            None,
        )))
        .await
        .unwrap()
        .into_inner();
    assert_ne!(changed.interfaces[0].revision, revision);
    assert_eq!(
        changed.dpu_statuses[0].interfaces[0].revision, revision,
        "old DPU readback remains visible while the new request is pending"
    );
    report(true, revision, 3, Some(true)).await.unwrap();
    let pending = read().await.unwrap().into_inner();
    assert_eq!(
        pending.dpu_statuses[0].interfaces[0].maintenance_mode,
        Some(false)
    );
    assert_ne!(
        pending.dpu_statuses[0].interfaces[0].revision,
        pending.interfaces[0].revision
    );
    // A new desired revision is accepted even if the reporting clock moved backwards.
    report(true, pending.interfaces[0].revision.clone(), 0, Some(true))
        .await
        .unwrap();
    let current = read().await.unwrap().into_inner();
    assert_eq!(
        current.dpu_statuses[0].interfaces[0].maintenance_mode,
        Some(true)
    );
    assert_eq!(
        current.dpu_statuses[0].interfaces[0].revision,
        current.interfaces[0].revision
    );

    // NIC discovery may start after maintenance delivery. A maintenance-only
    // report must preserve Weave's attachment observations once NICs exist.
    let mut txn = env.db_txn().await;
    let mac = "00:11:22:33:44:55".parse().unwrap();
    db::dpa_interface::persist(
        model::dpa_interface::NewDpaInterface {
            machine_id: *host.id,
            mac_address: mac,
            device_type: "ConnectX9".to_string(),
            pci_name: "0005:06:00.0".to_string(),
            device_description: None,
            interface_type: model::dpa_interface::DpaInterfaceType::Astra,
        },
        &mut txn,
    )
    .await
    .unwrap();
    let attachments = model::machine::spx::MachineSpxStatusObservation {
        spx_attachments: vec![model::machine::spx::MachineSpxAttachmentStatusObservation {
            mac_address: mac,
            ..Default::default()
        }],
        ..Default::default()
    };
    assert!(matches!(
        db::machine::update_spx_status_observation(&mut txn, &host.id, &attachments)
            .await
            .unwrap(),
        db::ConditionalWrite::Applied(())
    ));
    txn.commit().await.unwrap();
    report(true, current.interfaces[0].revision.clone(), 4, Some(true))
        .await
        .unwrap();
    let stored_attachments: serde_json::Value =
        sqlx::query_scalar("SELECT spx_status_observation FROM machines WHERE id=$1")
            .bind(id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert_eq!(
        stored_attachments,
        serde_json::to_value(&attachments).unwrap()
    );

    // Both an explicit present flag and omission by older agents retain full
    // attachment-report semantics: an observed empty list clears attachments.
    for presence in [Some(true), None] {
        let attachments = model::machine::spx::MachineSpxStatusObservation {
            observed_at: chrono::Utc::now(),
            ..attachments.clone()
        };
        let mut txn = env.db_txn().await;
        assert!(matches!(
            db::machine::update_spx_status_observation(&mut txn, &host.id, &attachments)
                .await
                .unwrap(),
            db::ConditionalWrite::Applied(())
        ));
        txn.commit().await.unwrap();
        env.api
            .record_dpu_network_status(Request::new(forge::DpuNetworkStatus {
                dpu_machine_id: Some(host.dpu().id),
                dpu_health: Some(rpc::health::HealthReport {
                    source: "forge-dpu-agent".to_string(),
                    ..Default::default()
                }),
                astra_config_status: Some(forge::AstraConfigStatus {
                    is_attachment_status_present: presence,
                    is_maintenance_status_present: Some(false),
                    ..Default::default()
                }),
                ..Default::default()
            }))
            .await
            .unwrap();
        let stored_attachments: serde_json::Value =
            sqlx::query_scalar("SELECT spx_status_observation FROM machines WHERE id=$1")
                .bind(id)
                .fetch_one(&env.pool)
                .await
                .unwrap();
        assert_eq!(stored_attachments["spx_attachments"], serde_json::json!([]));
    }
}

#[crate::sqlx_test]
async fn spx_multiplane_invalid_declarations_preserve_network_config_poll(pool: sqlx::PgPool) {
    let (env, host) = fixture(pool).await;
    let mut txn = env.db_txn().await;
    db::dpa_interface::persist(
        model::dpa_interface::NewDpaInterface {
            machine_id: *host.id,
            mac_address: "00:11:22:33:44:55".parse().unwrap(),
            device_type: "ConnectX9".to_string(),
            pci_name: "0005:06:00.0".to_string(),
            device_description: None,
            interface_type: model::dpa_interface::DpaInterfaceType::Astra,
        },
        &mut txn,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();
    let poll = || {
        env.api.get_managed_host_network_config(Request::new(
            forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(host.dpu().id),
            },
        ))
    };
    let mut expected_response = poll().await.unwrap().into_inner();
    let astra = expected_response.astra_config.as_mut().unwrap();
    assert_eq!(astra.astra_attachments.len(), 1);
    assert_eq!(astra.multiplane_interfaces.len(), 32);
    astra.multiplane_interfaces.clear();
    astra.is_maintenance_config_present = Some(false);

    let mut txn = env.db_txn().await;
    let machine = host.host().db_machine(&mut txn).await;
    let mut expected = db::expected_machine::find_by_bmc_mac_address(
        &mut *txn,
        machine.status.bmc_info.mac.unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    let duplicate = expected
        .data
        .interfaces
        .iter()
        .find(|interface| interface.logical_ifname.as_deref() == Some("eth_r0_p0"))
        .unwrap()
        .clone();
    expected.data.interfaces.push(duplicate);
    db::expected_machine::update(&mut txn, &expected)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    let response = poll().await.unwrap().into_inner();
    assert_eq!(response, expected_response);
    let id = host.id.into();
    let error = env
        .api
        .get_machine_spx_multiplane(Request::new(forge::MachineSpxMultiplaneRequest {
            machine_id: Some(id),
        }))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    let error = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(update_request(
            id,
            Some(0),
            0,
            false,
            None,
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
}

#[crate::sqlx_test]
async fn spx_multiplane_requests_persist_and_reach_dpu_without_attachments(pool: sqlx::PgPool) {
    let (env, host) = fixture(pool).await;
    let id: MachineId = host.id.into();
    let poll = || {
        env.api.get_managed_host_network_config(Request::new(
            forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(host.dpu().id),
            },
        ))
    };
    let initial = poll()
        .await
        .unwrap()
        .into_inner()
        .astra_config
        .expect("maintenance must be delivered before NIC discovery");
    assert!(initial.astra_attachments.is_empty());
    assert_eq!(initial.is_attachment_config_present, Some(false));
    assert_eq!(initial.is_maintenance_config_present, Some(true));
    assert_eq!(initial.multiplane_interfaces.len(), 32);
    assert!(
        initial
            .multiplane_interfaces
            .iter()
            .all(|entry| entry.cfg_maintenance_mode.is_none())
    );
    assert_eq!(
        persisted(&env, id).await["interfaces"]
            .as_object()
            .unwrap()
            .len(),
        32
    );

    let request = update_request(id, Some(2), 6, false, Some("SPX-42: replace cable"));
    let updated = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    let entry = updated
        .interfaces
        .iter()
        .find(|entry| entry.logical_ifname == "eth_r2_p6")
        .unwrap();
    assert_eq!(entry.cfg_maintenance_mode, Some(false));
    let pending = updated.dpu_statuses[0]
        .interfaces
        .iter()
        .find(|entry| entry.logical_ifname == "eth_r2_p6")
        .unwrap();
    assert!(pending.maintenance_mode.is_none());
    assert!(pending.observed_at.is_none());
    assert!(pending.netdev.is_none());
    let revision = entry.revision.clone();
    let delivered = poll().await.unwrap().into_inner().astra_config.unwrap();
    let entry = delivered
        .multiplane_interfaces
        .iter()
        .find(|entry| entry.logical_ifname == "eth_r2_p6")
        .unwrap();
    assert_eq!(entry.cfg_maintenance_mode, Some(false));
    assert_eq!(entry.revision, revision);
    assert_eq!(entry.reference.as_deref(), Some("SPX-42: replace cable"));
    assert_eq!(
        persisted(&env, id).await["interfaces"]["eth_r2_p6"]["cfg_maintenance_mode"],
        false
    );

    // Repeated requests preserve the revision, and omission preserves the URL.
    let preserved = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(update_request(
            id,
            Some(2),
            6,
            false,
            None,
        )))
        .await
        .unwrap()
        .into_inner();
    let entry = preserved
        .interfaces
        .iter()
        .find(|entry| entry.logical_ifname == "eth_r2_p6")
        .unwrap();
    assert_eq!(entry.revision, revision);
    assert_eq!(entry.reference.as_deref(), Some("SPX-42: replace cable"));
    let cleared = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(update_request(
            id,
            Some(2),
            6,
            false,
            Some(""),
        )))
        .await
        .unwrap()
        .into_inner();
    let entry = cleared
        .interfaces
        .iter()
        .find(|entry| entry.logical_ifname == "eth_r2_p6")
        .unwrap();
    assert!(entry.reference.is_none());
    assert_ne!(entry.revision, revision);

    let all = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(update_request(
            id, None, 1, false, None,
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        all.interfaces
            .iter()
            .filter(|entry| entry.plane == 1 && entry.cfg_maintenance_mode == Some(false))
            .count(),
        4
    );
    assert!(
        all.interfaces
            .iter()
            .filter(|entry| entry.plane == 5)
            .all(|entry| entry.cfg_maintenance_mode.is_none())
    );

    // Removing one declaration makes rail-all reject without any partial writes.
    let before = persisted(&env, id).await;
    let mut txn = env.db_txn().await;
    let machine = host.host().db_machine(&mut txn).await;
    let mut expected = db::expected_machine::find_by_bmc_mac_address(
        &mut *txn,
        machine.status.bmc_info.mac.unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    expected
        .data
        .interfaces
        .retain(|entry| entry.logical_ifname.as_deref() != Some("eth_r3_p7"));
    db::expected_machine::update(&mut txn, &expected)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let error = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(update_request(
            id, None, 7, false, None,
        )))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert_eq!(persisted(&env, id).await, before);

    let cases = [
        (
            "out of range plane",
            forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
                plane_selector: Some(forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::Plane(8)),
                ..request.clone()
            },
        ),
        (
            "out of range rail",
            update_request(id, Some(4), 0, false, None),
        ),
        (
            "missing maintenance mode",
            forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
                maintenance_mode: None,
                ..request.clone()
            },
        ),
        (
            "missing plane selector",
            forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
                plane_selector: None,
                ..request.clone()
            },
        ),
        (
            "all planes rejects an undeclared port atomically",
            forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
                plane_selector: Some(forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::AllPlanes(())),
                ..update_request(id, None, 0, false, None)
            },
        ),
        (
            "ranges reject an undeclared port atomically",
            forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
                rail_selector: Some(forge::machine_spx_multiplane_maintenance_update_request::RailSelector::RailRange(forge::SpxMultiplaneRange { start: Some(2), end: Some(3) })),
                plane_selector: Some(forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::PlaneRange(forge::SpxMultiplaneRange { start: Some(6), end: Some(7) })),
                ..request.clone()
            },
        ),
        (
            "missing rail",
            forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
                rail_selector: None,
                ..request
            },
        ),
    ];
    for (scenario, request) in cases {
        let error = env
            .api
            .update_machine_spx_multiplane_maintenance(Request::new(request))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument, "{scenario}");
        assert_eq!(persisted(&env, id).await, before, "{scenario}");
    }
}

#[crate::sqlx_test]
async fn spx_multiplane_concurrent_updates_preserve_other_rails(pool: sqlx::PgPool) {
    let (env, host) = fixture(pool).await;
    let id: MachineId = host.id.into();
    env.api
        .get_machine_spx_multiplane(Request::new(forge::MachineSpxMultiplaneRequest {
            machine_id: Some(id),
        }))
        .await
        .unwrap();
    let mut blocker = env.db_txn().await;
    db::machine::lock_spx_multiplane(&mut blocker, &host.id.into())
        .await
        .unwrap();
    let first_api = env.api.clone();
    let second_api = env.api.clone();
    let first = tokio::spawn(async move {
        first_api
            .update_machine_spx_multiplane_maintenance(Request::new(update_request(
                id,
                Some(0),
                0,
                false,
                None,
            )))
            .await
    });
    let second = tokio::spawn(async move {
        second_api
            .update_machine_spx_multiplane_maintenance(Request::new(update_request(
                id,
                Some(1),
                0,
                false,
                None,
            )))
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_stat_activity WHERE datname=current_database() AND wait_event_type='Lock' AND query LIKE 'SELECT spx_multiplane FROM machines%'")
                .fetch_one(&env.pool).await.unwrap();
            if waiting == 2 { break; }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("both writers contend for the host lock");
    blocker.commit().await.unwrap();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();
    let saved = persisted(&env, id).await;
    assert_eq!(
        saved["interfaces"]["eth_r0_p0"]["cfg_maintenance_mode"],
        false
    );
    assert_eq!(
        saved["interfaces"]["eth_r1_p0"]["cfg_maintenance_mode"],
        false
    );
    assert_eq!(
        saved["interfaces"]["eth_r2_p0"]["cfg_maintenance_mode"],
        serde_json::Value::Null
    );
}

#[crate::sqlx_test]
async fn spx_multiplane_broadcasts_to_bf4_dpus_and_keeps_readback_independent(pool: sqlx::PgPool) {
    let (env, host) = fixture_with_dpus(pool, 2).await;
    let id: MachineId = host.id.into();
    let requested = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(update_request(
            id,
            Some(0),
            0,
            false,
            None,
        )))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(requested.dpu_statuses.len(), 2);
    assert!(
        requested
            .dpu_statuses
            .iter()
            .all(|dpu| dpu.interfaces.len() == 32
                && dpu.interfaces.iter().all(|port| port.observed_at.is_none()))
    );
    let revision = requested.interfaces[0].revision.clone();
    // Unchanged config polls must complete even while a host writer holds the row lock.
    let mut blocker = env.db_txn().await;
    db::machine::lock_spx_multiplane(&mut blocker, &host.id.into())
        .await
        .unwrap();
    for dpu in &host.dpu_ids {
        let config = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            env.api.get_managed_host_network_config(Request::new(
                forge::ManagedHostNetworkConfigRequest {
                    dpu_machine_id: Some(*dpu),
                },
            )),
        )
        .await
        .expect("unchanged config poll must not wait for the host row lock")
        .unwrap()
        .into_inner()
        .astra_config
        .unwrap();
        assert_eq!(config.multiplane_interfaces.len(), 32);
        assert_eq!(config.multiplane_interfaces[0].revision, revision);
        assert_eq!(
            config.multiplane_interfaces[0].cfg_maintenance_mode,
            Some(false)
        );
    }
    blocker.rollback().await.unwrap();
    let observed_at = chrono::Utc::now();
    let report = |dpu, mode, error: Option<&str>, seconds| {
        env.api
            .record_dpu_network_status(Request::new(forge::DpuNetworkStatus {
                dpu_machine_id: Some(dpu),
                dpu_health: Some(rpc::health::HealthReport {
                    source: "forge-dpu-agent".into(),
                    ..Default::default()
                }),
                astra_config_status: Some(forge::AstraConfigStatus {
                    is_attachment_status_present: Some(false),
                    is_maintenance_status_present: Some(true),
                    multiplane_interfaces_status: vec![forge::AstraMultiplaneInterfaceStatus {
                        logical_ifname: "eth_r0_p0".into(),
                        revision: revision.clone(),
                        maintenance_mode: mode,
                        netdev: Some("A56p0".into()),
                        error: error.map(str::to_string),
                        observed_at: Some(
                            (observed_at + chrono::Duration::seconds(seconds)).into(),
                        ),
                    }],
                    ..Default::default()
                }),
                ..Default::default()
            }))
    };
    report(host.dpu_ids[0], Some(false), None, 0).await.unwrap();
    report(host.dpu_ids[1], None, Some("Xplane unavailable"), 1)
        .await
        .unwrap();
    let read = || {
        env.api
            .get_machine_spx_multiplane(Request::new(forge::MachineSpxMultiplaneRequest {
                machine_id: Some(id),
            }))
    };
    let result = read().await.unwrap().into_inner();
    let first = result
        .dpu_statuses
        .iter()
        .find(|dpu| dpu.dpu_machine_id == Some(host.dpu_ids[0]))
        .unwrap();
    let second = result
        .dpu_statuses
        .iter()
        .find(|dpu| dpu.dpu_machine_id == Some(host.dpu_ids[1]))
        .unwrap();
    assert_eq!(first.interfaces[0].maintenance_mode, Some(false));
    assert!(first.interfaces[0].error.is_none());
    assert_eq!(
        second.interfaces[0].error.as_deref(),
        Some("Xplane unavailable")
    );
    let first_stored = persisted_dpu(&env, host.dpu_ids[0]).await;
    let second_stored = persisted_dpu(&env, host.dpu_ids[1]).await;
    assert_ne!(first_stored, second_stored);
    assert!(
        persisted(&env, id).await["interfaces"]["eth_r0_p0"]
            .get("observation")
            .is_none()
    );
    let host_observation: Option<serde_json::Value> =
        sqlx::query_scalar("SELECT spx_multiplane_status FROM machines WHERE id=$1")
            .bind(id)
            .fetch_one(&env.pool)
            .await
            .unwrap();
    assert!(host_observation.is_none());
    report(host.dpu_ids[1], Some(false), None, 2).await.unwrap();
    assert_eq!(
        persisted_dpu(&env, host.dpu_ids[0]).await,
        first_stored,
        "another DPU's report must never overwrite this DPU"
    );
    let recovered = read().await.unwrap().into_inner();
    assert!(
        recovered
            .dpu_statuses
            .iter()
            .all(|dpu| dpu.interfaces[0].maintenance_mode == Some(false)
                && dpu.interfaces[0].error.is_none())
    );

    // A non-BF4 DPU on the same host is neither a recipient nor a status participant.
    sqlx::query("UPDATE machine_topologies SET topology=jsonb_set(topology, '{discovery_data,Info,dmi_data,product_name}', $2::jsonb) WHERE machine_id=$1")
        .bind(host.dpu_ids[1]).bind(serde_json::json!("BlueField-3 SmartNIC Main Card")).execute(&env.pool).await.unwrap();
    let other = env
        .api
        .get_managed_host_network_config(Request::new(forge::ManagedHostNetworkConfigRequest {
            dpu_machine_id: Some(host.dpu_ids[1]),
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(other.astra_config.is_none());
    let filtered = read().await.unwrap().into_inner();
    assert_eq!(filtered.dpu_statuses.len(), 1);
    assert_eq!(
        filtered.dpu_statuses[0].dpu_machine_id,
        Some(host.dpu_ids[0])
    );
    let before_ignored = persisted_dpu(&env, host.dpu_ids[1]).await;
    report(host.dpu_ids[1], Some(true), None, 3).await.unwrap();
    assert_eq!(persisted_dpu(&env, host.dpu_ids[1]).await, before_ignored);
}

#[crate::sqlx_test]
async fn spx_multiplane_accepts_host_requests_without_dpus(pool: sqlx::PgPool) {
    let (env, host) = fixture_with_dpus(pool, 0).await;
    let id = host.id.into();
    let result = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(update_request(
            id, None, 3, false, None,
        )))
        .await
        .unwrap()
        .into_inner();
    assert!(result.dpu_statuses.is_empty());
    assert_eq!(
        result
            .interfaces
            .iter()
            .filter(|port| port.plane == 3 && port.cfg_maintenance_mode == Some(false))
            .count(),
        4
    );
    let read = env
        .api
        .get_machine_spx_multiplane(Request::new(forge::MachineSpxMultiplaneRequest {
            machine_id: Some(id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read, result);

    let mut previous = result;
    for rail in [Some(2), None] {
        let request = forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
            plane_selector: Some(
                forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::AllPlanes(
                    (),
                ),
            ),
            ..update_request(id, rail, 0, false, None)
        };
        let result = env
            .api
            .update_machine_spx_multiplane_maintenance(Request::new(request))
            .await
            .unwrap()
            .into_inner();
        let mut selected = 0;
        for port in &result.interfaces {
            if rail.is_none_or(|rail| port.rail == rail) {
                assert_eq!(port.cfg_maintenance_mode, Some(false));
                selected += 1;
            } else {
                assert_eq!(
                    Some(port),
                    previous
                        .interfaces
                        .iter()
                        .find(|old| old.logical_ifname == port.logical_ifname)
                );
            }
        }
        assert_eq!(selected, if rail.is_some() { 8 } else { 32 });
        let read = env
            .api
            .get_machine_spx_multiplane(Request::new(forge::MachineSpxMultiplaneRequest {
                machine_id: Some(id),
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(read, result);
        previous = result;
    }

    // A range is one atomic RPC and selects the Cartesian product, including both endpoints.
    let request = forge::MachineSpxMultiplaneMaintenanceUpdateRequest {
        rail_selector: Some(
            forge::machine_spx_multiplane_maintenance_update_request::RailSelector::RailRange(
                forge::SpxMultiplaneRange {
                    start: Some(0),
                    end: Some(2),
                },
            ),
        ),
        plane_selector: Some(
            forge::machine_spx_multiplane_maintenance_update_request::PlaneSelector::PlaneRange(
                forge::SpxMultiplaneRange {
                    start: Some(1),
                    end: Some(4),
                },
            ),
        ),
        ..update_request(id, Some(0), 0, true, Some("range request"))
    };
    let result = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(request.clone()))
        .await
        .unwrap()
        .into_inner();
    for port in &result.interfaces {
        if port.rail <= 2 && (1..=4).contains(&port.plane) {
            assert_eq!(port.cfg_maintenance_mode, Some(true));
            assert_eq!(port.reference.as_deref(), Some("range request"));
        } else {
            assert_eq!(
                Some(port),
                previous
                    .interfaces
                    .iter()
                    .find(|old| old.logical_ifname == port.logical_ifname)
            );
        }
    }
    let repeated = env
        .api
        .update_machine_spx_multiplane_maintenance(Request::new(request))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(
        repeated, result,
        "repeating a range request must preserve revisions"
    );
    let read = env
        .api
        .get_machine_spx_multiplane(Request::new(forge::MachineSpxMultiplaneRequest {
            machine_id: Some(id),
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(read, result, "range updates must persist");
}
