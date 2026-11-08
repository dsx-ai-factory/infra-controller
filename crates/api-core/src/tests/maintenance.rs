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

use carbide_uuid::machine::MachineId;
use common::api_fixtures::create_test_env;
use common::api_fixtures::instance::{
    default_os_config, default_tenant_config, single_interface_network_config,
};
use model::machine::{FailureCause, FailureDetails, FailureSource, ManagedHostState};
use rpc::forge as rpcf;
use rpc::forge::forge_server::Forge;

use crate::test_support::fixture_config::FixtureDefault as _;
use crate::tests::common;
use crate::tests::common::api_fixtures::{create_managed_host, create_managed_host_multi_dpu};

#[crate::sqlx_test]
async fn test_maintenance_multi_dpu(db_pool: sqlx::PgPool) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool.clone()).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    // Create a machine
    let mh = create_managed_host_multi_dpu(&env, 2).await;

    // enable maintenance mode
    let req = rpcf::MaintenanceRequest {
        operation: rpcf::MaintenanceOperation::Enable.into(),
        host_id: Some(mh.host().id.into()),
        reference: Some("https://jira.example.com/ABC-123".to_string()),
    };
    env.api
        .set_maintenance(tonic::Request::new(req))
        .await
        .unwrap();

    let instance_config = rpcf::InstanceConfig {
        tenant: Some(default_tenant_config()),
        network: Some(single_interface_network_config(segment_id)),
        os: Some(default_os_config()),
        infiniband: None,
        nvlink: None,
        spxconfig: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        power_profile: None,
    };

    // allocate: should fail
    let req = rpcf::InstanceAllocationRequest {
        instance_id: None,
        machine_id: Some(mh.host().id),
        instance_type_id: None,
        config: Some(instance_config.clone()),
        metadata: Some(rpcf::Metadata {
            name: "test_instance".to_string(),
            description: "tests/maintenance".to_string(),
            labels: Vec::new(),
        }),
        allow_unhealthy_machine: false,
        external_validation_request_id: None,
    };
    match env.api.allocate_instance(tonic::Request::new(req)).await {
        Ok(_) => {
            panic!("Allocating an instance on host in maintenance mode should fail");
        }
        Err(status) if status.code() == tonic::Code::FailedPrecondition => {
            // Expected
        }
        Err(err) => {
            eyre::bail!("allocate_instance unexpected status {err}");
        }
    }

    // list: should be included
    let machine_ids = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            include_dpus: true,
            include_predicted_host: true,
            only_maintenance: true,
            ..Default::default()
        }))
        .await?
        .into_inner()
        .machine_ids;

    assert_eq!(machine_ids.len(), 1); // Host
    assert_eq!(
        machine_ids[0],
        mh.host().id.into(),
        "Listing maintenance machines return incorrectly machines"
    );

    // disable maintenance
    let req = tonic::Request::new(rpcf::MaintenanceRequest {
        operation: rpcf::MaintenanceOperation::Disable.into(),
        host_id: Some(mh.host().id.into()),
        reference: None,
    });
    env.api.set_maintenance(req).await.unwrap();

    // There are now no machines in maintenance mode
    let machines_ids = env
        .api
        .find_machine_ids(tonic::Request::new(rpc::forge::MachineSearchConfig {
            include_dpus: true,
            include_predicted_host: true,
            only_maintenance: true,
            ..Default::default()
        }))
        .await?
        .into_inner()
        .machine_ids;
    assert!(machines_ids.is_empty());

    let allocation = || rpcf::InstanceAllocationRequest {
        instance_id: None,
        machine_id: Some(mh.host().id),
        instance_type_id: None,
        config: Some(instance_config.clone()),
        metadata: Some(rpc::Metadata {
            name: "test_instance".to_string(),
            description: "tests/maintenance".to_string(),
            labels: Vec::new(),
        }),
        allow_unhealthy_machine: false,
        external_validation_request_id: None,
    };

    match env
        .api
        .allocate_instance(tonic::Request::new(allocation()))
        .await
    {
        Ok(_) => {
            panic!(
                "Allocating a host whose maintenance just ended should wait for re-verification"
            );
        }
        Err(status) if status.code() == tonic::Code::FailedPrecondition => {}
        Err(err) => {
            eyre::bail!("allocate_instance unexpected status {err}");
        }
    }

    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    assert!(
        matches!(
            host.current_state(),
            ManagedHostState::BootConfiguring { .. }
        ),
        "the next sweep should re-verify the host, got {:?}",
        host.current_state()
    );

    Ok(())
}

/// test: putting a machine into maintenance mode must suppress any stuck instance alerts.
///
/// We check a machine in maintenance mode for its contribution to the state-machine
/// SLA-breach signal that drives the `stuckInstanceCritical` Prometheus alert.
///
/// This makes use of the tactic from `test_state_sla` (force the machine into
/// `ManagedHostState::Failed`, which has a zero-second SLA, so the machine is
/// instantly "above SLA" without us having to wait out a real SLA window)
#[crate::sqlx_test]
async fn test_maintenance_suppresses_state_machine_sla_alert(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool.clone()).await;
    let (host_id, _dpu_id) = create_managed_host(&env).await.into();
    let rpc_host_id: MachineId = host_id.into();

    // force the host into Failed state (0-second SLA).
    // this is what would otherwise drive `carbide_machines_per_state_above_sla > 0`
    // and page on-call via `stuckInstanceCritical`.
    let mut txn = env.db_txn().await;
    db::machine::update_state(
        &mut txn,
        &host_id,
        &ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::NoError,
                failed_at: chrono::Utc::now(),
                source: FailureSource::NoError,
            },
            machine_id: host_id.into(),
            retry_count: 1,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // with no maintenance override, the machine reports as
    // above-SLA and would be counted by the stuck-instance alert metric.
    let machine = env.find_machine(&rpc_host_id).await.remove(0);
    let sla = machine.state_sla.as_ref().unwrap();
    assert!(
        sla.time_in_state_above_sla,
        "expected the Failed-state host to be above SLA before maintenance is enabled",
    );

    // enable maintenance mode
    env.api
        .set_maintenance(tonic::Request::new(rpcf::MaintenanceRequest {
            operation: rpcf::MaintenanceOperation::Enable.into(),
            host_id: Some(rpc_host_id),
            reference: Some("https://jira.example.com/ABC-123".to_string()),
        }))
        .await
        .unwrap();

    // SetMaintenance now adds the ExcludeFromStateMachineSla
    // classification, so state_sla() short-circuits to no_sla() and the
    // host stops contributing to the stuck-instance Prometheus metric.
    let machine = env.find_machine(&rpc_host_id).await.remove(0);
    let sla = machine.state_sla.as_ref().unwrap();
    assert!(
        !sla.time_in_state_above_sla,
        "maintenance mode must suppress state-machine SLA breach",
    );
    assert!(
        sla.sla.is_none(),
        "maintenance mode must produce a no-SLA result regardless of current state",
    );

    // disabling maintenance should re-expose the breach.
    env.api
        .set_maintenance(tonic::Request::new(rpcf::MaintenanceRequest {
            operation: rpcf::MaintenanceOperation::Disable.into(),
            host_id: Some(rpc_host_id),
            reference: None,
        }))
        .await
        .unwrap();

    let machine = env.find_machine(&rpc_host_id).await.remove(0);
    let sla = machine.state_sla.as_ref().unwrap();
    assert!(
        sla.time_in_state_above_sla,
        "disabling maintenance should re-expose the above-SLA condition",
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_ending_maintenance_reverifies_boot_configuration(
    db_pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(db_pool.clone()).await;
    let host = common::api_fixtures::site_explorer::new_host(
        &env,
        model::test_support::ManagedHostConfig::default(),
    )
    .await?;
    let host_id: carbide_uuid::machine::StableHostMachineId = host.host_snapshot.id.try_into()?;
    let rpc_host_id: MachineId = host.host_snapshot.id.into();
    let maintenance = |operation: rpcf::MaintenanceOperation| rpcf::MaintenanceRequest {
        operation: operation.into(),
        host_id: Some(rpc_host_id),
        reference: Some("https://jira.example.com/ABC-123".to_string()),
    };
    let queued = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM machine_state_controller_queued_objects WHERE object_id = $1",
        )
        .bind(host_id.to_string())
        .fetch_one(&env.pool)
        .await
    };
    let before = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("an explored host has a desired boot interface");

    env.api
        .set_maintenance(tonic::Request::new(maintenance(
            rpcf::MaintenanceOperation::Enable,
        )))
        .await?;
    let during = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("maintenance keeps the desired boot interface");
    assert_eq!(
        during.version, before.version,
        "starting maintenance must not touch the boot configuration",
    );
    sqlx::query("DELETE FROM machine_state_controller_queued_objects WHERE object_id = $1")
        .bind(host_id.to_string())
        .execute(&env.pool)
        .await?;

    env.api
        .set_maintenance(tonic::Request::new(maintenance(
            rpcf::MaintenanceOperation::Disable,
        )))
        .await?;
    let after = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("ending maintenance keeps the desired boot interface");
    assert_eq!(after.value, before.value);
    assert_ne!(
        after.version, before.version,
        "ending maintenance must open a fresh boot configuration generation",
    );
    assert_eq!(
        queued().await?,
        1,
        "an unassigned Ready host is woken to re-verify after maintenance",
    );

    env.api
        .set_maintenance(tonic::Request::new(maintenance(
            rpcf::MaintenanceOperation::Disable,
        )))
        .await?;
    let repeated = db::machine_desired_boot_interface::get(&env.pool, &host_id)
        .await?
        .expect("the desired boot interface stays");
    assert_eq!(
        repeated.version, after.version,
        "ending maintenance on a host that is not in maintenance changes nothing",
    );

    Ok(())
}
