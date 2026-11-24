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

use carbide_uuid::machine::{HostMachineId, MachineId};
use common::api_fixtures::{create_managed_host, create_managed_host_multi_dpu, create_test_env};
use db::{DatabaseError, TransactionVending as _};
use model::dpu_machine_update::DpuMachineUpdate;
use model::machine::network::MachineNetworkStatusObservation;
use model::machine::{DpuMachine, LoadSnapshotOptions, ManagedHostStateSnapshot};
use sqlx::PgConnection;

use super::common::api_fixtures::TestEnv;
use crate::CarbideResult;
use crate::tests::common;
use crate::tests::common::api_fixtures::dpu::create_dpu_machine_in_waiting_for_network_install;

pub(in crate::tests) async fn update_nic_firmware_version(
    txn: &mut PgConnection,
    machine_id: &MachineId,
    version: &str,
) -> CarbideResult<()> {
    let query = r#"UPDATE machine_topologies SET topology =
                jsonb_set(topology, '{discovery_data, Info, dpu_info, firmware_version}', $1) 
                WHERE machine_id=$2"#;

    sqlx::query(query)
        .bind(sqlx::types::Json(version))
        .bind(machine_id)
        .execute(txn)
        .await
        .map_err(|e| DatabaseError::query(query, e))?;

    Ok(())
}

async fn create_machines(
    test_env: &TestEnv,
    machine_count: usize,
) -> HashMap<HostMachineId, ManagedHostStateSnapshot> {
    let mut machines = Vec::default();
    for _ in 0..machine_count {
        let machine = create_managed_host(test_env).await;
        machines.push(machine);
    }
    let mut txn = test_env.pool.txn_begin().await.unwrap();

    for m in &machines {
        update_nic_firmware_version(&mut txn, &m.dpu().id, "11.10.1000")
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();

    db::managed_host::load_by_machine_ids(
        &mut test_env.db_reader(),
        &machines
            .iter()
            .map(|machine| machine.id)
            .collect::<Vec<_>>(),
        LoadSnapshotOptions {
            include_history: false,
            include_instance_data: false,
            host_health_config: test_env.config.host_health,
        },
    )
    .await
    .expect("Failed to load snapshots")
    .into_iter()
    .map(|(id, snapshot)| (id.into(), snapshot))
    .collect()
}

pub(in crate::tests) async fn get_all_snapshots(
    test_env: &TestEnv,
) -> HashMap<HostMachineId, ManagedHostStateSnapshot> {
    let machine_ids = db::managed_host::load_host_ids(&test_env.pool)
        .await
        .unwrap();

    db::managed_host::load_by_machine_ids(
        &mut test_env.db_reader(),
        &machine_ids,
        LoadSnapshotOptions {
            include_history: false,
            include_instance_data: false,
            host_health_config: test_env.config.host_health,
        },
    )
    .await
    .unwrap()
}

#[crate::sqlx_test]
async fn test_find_available_outdated_dpus(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let dpu_count: usize = 2;
    let snapshots = create_machines(&env, dpu_count).await;
    let dpus = DpuMachineUpdate::find_available_outdated_dpus(
        None,
        &env.config.dpu_config.dpu_nic_firmware_update_versions,
        &snapshots,
        &[],
    )?;

    assert_eq!(dpus.len(), dpu_count);
    Ok(())
}

#[crate::sqlx_test]
async fn test_find_available_outdated_dpus_with_unhealthy(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let snapshots = create_machines(&env, 2).await;
    let unhealthy_dpu_machine_id = snapshots.iter().next().unwrap().1.dpu_snapshots[0].id;

    let machine_obs = MachineNetworkStatusObservation {
        machine_id: unhealthy_dpu_machine_id,
        agent_version: None,
        observed_at: chrono::Utc::now(),
        network_config_version: None,
        client_certificate_expiry: None,
        agent_version_superseded_at: None,
        instance_network_observation: None,
        fabric_interfaces: vec![],
        accepted_lldp_snapshot_fingerprint: None,
        service_vpc_network_observation: None,
    };

    let health_report = health_report::HealthReport {
        source: "forge-dpu-agent".to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![health_report::HealthProbeAlert {
            id: "TestFailed".parse().unwrap(),
            target: Some("t1".to_string()),
            in_alert_since: Some(chrono::Utc::now()),
            message: "Test Failed".to_string(),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::prevent_host_state_changes(),
            ],
        }],
    };
    let mut txn = env
        .pool
        .txn_begin()
        .await
        .expect("Failed to create transaction");

    assert_eq!(
        db::machine::update_network_status_observation(
            &mut txn,
            &unhealthy_dpu_machine_id,
            &machine_obs
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    db::machine::update_dpu_agent_health_report(
        &mut txn,
        &unhealthy_dpu_machine_id,
        &health_report,
    )
    .await?;

    txn.commit().await.unwrap();

    let snapshots = get_all_snapshots(&env).await;

    let dpus = DpuMachineUpdate::find_available_outdated_dpus(
        None,
        &env.config.dpu_config.dpu_nic_firmware_update_versions,
        &snapshots,
        &[],
    )?;

    assert_eq!(dpus.len(), 1);
    assert_ne!(dpus[0].dpu_machine_id, unhealthy_dpu_machine_id);
    Ok(())
}

#[crate::sqlx_test]
async fn test_find_available_outdated_dpus_limit(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let snapshots = create_machines(&env, 2).await;
    let dpus = DpuMachineUpdate::find_available_outdated_dpus(
        Some(1),
        &env.config.dpu_config.dpu_nic_firmware_update_versions,
        &snapshots,
        &[],
    )?;

    assert_eq!(dpus.len(), 1);
    Ok(())
}

#[crate::sqlx_test]
async fn test_find_available_outdated_dpus_skips_dpf_ingested(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let dpu_count: usize = 2;
    let snapshots = create_machines(&env, dpu_count).await;

    let mut txn = env.pool.txn_begin().await?;
    let host_machine_id = snapshots.keys().collect::<Vec<_>>()[0];
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, host_machine_id).await?;
    txn.commit().await?;

    let snapshots = get_all_snapshots(&env).await;

    let dpus = DpuMachineUpdate::find_available_outdated_dpus(
        None,
        &env.config.dpu_config.dpu_nic_firmware_update_versions,
        &snapshots,
        &[],
    )?;

    assert_eq!(dpus.len(), 1);
    Ok(())
}

#[crate::sqlx_test]
async fn test_find_unavailable_outdated_dpus_when_none(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let snapshots = create_machines(&env, 1).await;

    let dpus = DpuMachineUpdate::find_unavailable_outdated_dpus(
        &env.config.dpu_config.dpu_nic_firmware_update_versions,
        &snapshots,
    )?;

    assert_eq!(dpus.len(), 0);
    Ok(())
}

#[crate::sqlx_test]
async fn test_find_unavailable_outdated_dpus(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mut txn = env.pool.txn_begin().await?;

    let host_config = env.managed_host_config();
    let mh = create_dpu_machine_in_waiting_for_network_install(&env, &host_config).await;
    update_nic_firmware_version(&mut txn, &mh.dpu().id, "11.10.1000").await?;
    txn.commit().await.unwrap();

    create_machines(&env, 2).await;
    let snapshots = get_all_snapshots(&env).await;

    let dpus = DpuMachineUpdate::find_unavailable_outdated_dpus(
        &env.config.dpu_config.dpu_nic_firmware_update_versions,
        &snapshots,
    )?;

    assert_eq!(dpus.len(), 1);
    assert_eq!(dpus.first().unwrap().dpu_machine_id, mh.dpu_ids[0]);
    assert_eq!(dpus.first().unwrap().host_machine_id, mh.id.into());

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_available_outdated_dpus_multidpu(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mh = create_managed_host_multi_dpu(&env, 2).await;
    let mut txn = env.pool.txn_begin().await?;
    let all_dpus = mh.dpu_db_machines(&mut txn).await;

    for dpu in &all_dpus {
        update_nic_firmware_version(&mut txn, &dpu.id, "1.11.1000").await?;
    }

    let snapshots = db::managed_host::load_by_machine_ids(
        txn.as_mut(),
        &[mh.id],
        LoadSnapshotOptions {
            include_history: false,
            include_instance_data: false,
            host_health_config: env.config.host_health,
        },
    )
    .await
    .expect("Failed to load snapshots")
    .into_iter()
    .map(|(id, snapshot)| (id.into(), snapshot))
    .collect();

    txn.commit().await?;

    let dpus = DpuMachineUpdate::find_available_outdated_dpus(
        None,
        &env.config.dpu_config.dpu_nic_firmware_update_versions,
        &snapshots,
        &[],
    )?;

    assert_eq!(dpus.len(), all_dpus.len());
    Ok(())
}

#[crate::sqlx_test]
async fn test_find_available_outdated_dpus_multidpu_one_under_reprov(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mh = create_managed_host_multi_dpu(&env, 2).await;

    let mut txn = env.pool.txn_begin().await?;
    db::dpu_machine_update::trigger_reprovisioning_for_managed_host(
        &mut txn,
        &[DpuMachineUpdate {
            host_machine_id: mh.id.into(),
            dpu_machine_id: mh.dpu_ids[0],
            firmware_version: "test_version".to_string(),
            dpf_managed: false,
        }],
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let snapshots = db::managed_host::load_by_machine_ids(
        &mut env.db_reader(),
        &[mh.id],
        LoadSnapshotOptions {
            include_history: false,
            include_instance_data: false,
            host_health_config: env.config.host_health,
        },
    )
    .await
    .unwrap()
    .into_iter()
    .map(|(id, snapshot)| (id.into(), snapshot))
    .collect();

    let dpus = DpuMachineUpdate::find_available_outdated_dpus(
        None,
        &env.config.dpu_config.dpu_nic_firmware_update_versions,
        &snapshots,
        &[],
    )?;

    assert!(dpus.is_empty());

    let all_dpus = mh.dpu_db_machines(&env.pool).await;

    let (dpu_under_reprov, dpu_not_under_reprov): (Vec<DpuMachine>, Vec<DpuMachine>) = all_dpus
        .into_iter()
        .partition(|x| x.reprovision_requested.is_some());
    assert_eq!(dpu_under_reprov.len(), 1);
    assert_eq!(dpu_not_under_reprov.len(), 1);
    assert_eq!(dpu_under_reprov[0].id, mh.dpu_n(0).id);

    Ok(())
}

#[crate::sqlx_test]
async fn test_find_available_outdated_dpus_multidpu_both_under_reprov(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    let mh = create_managed_host_multi_dpu(&env, 2).await;

    let mut txn = env.pool.txn_begin().await?;
    let all_dpus = mh.dpu_db_machines(&mut txn).await;
    db::dpu_machine_update::trigger_reprovisioning_for_managed_host(
        &mut txn,
        &[
            DpuMachineUpdate {
                host_machine_id: mh.id.into(),
                dpu_machine_id: all_dpus[1].id,
                firmware_version: "test_version".to_string(),
                dpf_managed: false,
            },
            DpuMachineUpdate {
                host_machine_id: mh.id.into(),
                dpu_machine_id: all_dpus[0].id,
                firmware_version: "test_version".to_string(),
                dpf_managed: false,
            },
        ],
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let snapshots = db::managed_host::load_by_machine_ids(
        &mut env.db_reader(),
        &[mh.id],
        LoadSnapshotOptions {
            include_history: false,
            include_instance_data: false,
            host_health_config: env.config.host_health,
        },
    )
    .await
    .unwrap()
    .into_iter()
    .map(|(id, snapshot)| (id.into(), snapshot))
    .collect();

    let dpus = DpuMachineUpdate::find_available_outdated_dpus(
        None,
        &env.config.dpu_config.dpu_nic_firmware_update_versions,
        &snapshots,
        &[],
    )?;

    assert!(dpus.is_empty());

    let all_dpus = mh.dpu_db_machines(&env.pool).await;

    let (dpu_under_reprov, dpu_not_under_reprov): (Vec<DpuMachine>, Vec<DpuMachine>) = all_dpus
        .into_iter()
        .partition(|x| x.reprovision_requested.is_some());
    assert_eq!(dpu_under_reprov.len(), 2);
    assert_eq!(dpu_not_under_reprov.len(), 0);
    Ok(())
}

#[crate::sqlx_test]
async fn test_completed_dpu_updates_require_all_dpus_and_ready_host(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    use carbide_machine_controller::health_report::create_host_update_health_report_dpufw;
    use model::machine::{
        FailureCause, FailureDetails, FailureSource, InstanceState, ManagedHostState,
        ReprovisionState,
    };

    let env = create_test_env(pool).await;
    let host = create_managed_host_multi_dpu(&env, 2).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let _instance = host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let unmarked = create_managed_host(&env).await;
    let ready = ManagedHostState::Assigned {
        instance_state: InstanceState::Ready,
    };
    let mut txn = env.pool.txn_begin().await?;
    // A snapshot that this binary cannot decode must not block cleanup of a different host.
    sqlx::query("UPDATE machines SET controller_state = '{}'::jsonb WHERE id = $1")
        .bind(unmarked.id)
        .execute(&mut *txn)
        .await?;
    db::machine::insert_health_report(
        &mut txn,
        &host.id,
        health_report::HealthReportApplyMode::Merge,
        &create_host_update_health_report_dpufw(),
        false,
    )
    .await?;
    txn.commit().await?;

    for (scenario, state, pending_secondary, expected_count) in [
        ("secondary DPU still requested", ready.clone(), true, 0),
        (
            "host still reprovisioning after requests cleared",
            ManagedHostState::Assigned {
                instance_state: InstanceState::DPUReprovision {
                    dpu_states: model::machine::DpuReprovisionStates {
                        states: host
                            .dpu_ids
                            .iter()
                            .map(|id| (*id, ReprovisionState::RebootHost))
                            .collect(),
                    },
                },
            },
            false,
            0,
        ),
        (
            "host failed after requests cleared",
            ManagedHostState::Assigned {
                instance_state: InstanceState::Failed {
                    details: FailureDetails {
                        cause: FailureCause::DpfProvisioning {
                            err: "test update failed".to_string(),
                        },
                        failed_at: chrono::Utc::now(),
                        source: FailureSource::NoError,
                    },
                    machine_id: host.dpu().id.into(),
                },
            },
            false,
            0,
        ),
        ("both DPUs complete", ready, false, 2),
    ] {
        let mut txn = env.pool.txn_begin().await?;
        db::machine::update_state(&mut txn, &host.id, &state).await?;
        db::machine::clear_dpu_reprovisioning_request(&mut txn, &host.dpu_n(1).id, false).await?;
        if pending_secondary {
            db::machine::trigger_dpu_reprovisioning_request(
                &host.dpu_n(1).id,
                &mut txn,
                model::machine_update_module::AutomaticFirmwareUpdateReference::REF_NAME,
                true,
            )
            .await?;
        }
        txn.commit().await?;

        let updated = db::dpu_machine_update::get_updated_machines(
            &mut env.db_reader(),
            env.config.host_health,
        )
        .await?;
        assert_eq!(updated.len(), expected_count, "{scenario}");
        assert!(
            updated
                .iter()
                .all(|dpu| dpu.host_machine_id == host.id.into()),
            "{scenario}"
        );
    }

    Ok(())
}
