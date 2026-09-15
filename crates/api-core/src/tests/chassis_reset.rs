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

use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::machine::MachineId;
use config_version::ConfigVersion;
use model::machine::{
    DecommissioningState, FailureCause, FailureDetails, FailureSource, MachineMaintenanceOperation,
    ManagedHostState, ReadyBootConfigState, StateMachineArea,
};
use model::machine_boot_interface::MachineBootInterfaceTarget;
use rpc::forge::admin_power_control_request::SystemPowerControl;
use rpc::forge::forge_server::Forge;
use rpc::forge::{AdminChassisResetRequest, MaintenanceOperation, MaintenanceRequest};
use tonic::Request;

use crate::tests::common::api_fixtures::{create_managed_host, create_test_env};

fn reset_request(machine_id: MachineId) -> Request<AdminChassisResetRequest> {
    Request::new(AdminChassisResetRequest {
        machine_id: Some(machine_id),
        chassis_id: "HGX_Chassis_0".into(),
        action: SystemPowerControl::ForceRestart as i32,
    })
}

#[crate::sqlx_test]
async fn admin_chassis_reset_requires_and_preserves_operator_maintenance(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let host = create_managed_host(&env).await.host();
    let mut txn = env.db_txn().await;
    db::machine::update_state(&mut txn, &host.id, &ManagedHostState::Ready).await?;
    let bmc_access = host.bmc_access(&mut txn).await;
    txn.commit().await?;
    let redfish_timepoint = env.redfish_sim.timepoint();
    let operation = MachineMaintenanceOperation::ChassisReset {
        chassis_id: "HGX_Chassis_0".into(),
    };

    let error = env
        .api
        .admin_chassis_reset(reset_request(host.id.into()))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    env.api
        .set_maintenance(Request::new(MaintenanceRequest {
            operation: MaintenanceOperation::Enable.into(),
            host_id: Some(host.id.into()),
            reference: Some("chassis reset recovery".into()),
        }))
        .await?;
    env.api
        .admin_chassis_reset(reset_request(host.id.into()))
        .await?;

    let mut txn = env.db_txn().await;
    let machine = host.db_machine(&mut txn).await;
    assert_eq!(machine.current_state(), &ManagedHostState::Ready);
    assert_eq!(
        machine
            .machine_maintenance_requested
            .as_ref()
            .map(|request| &request.operation),
        Some(&operation)
    );
    txn.rollback().await?;
    assert!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .for_host(&bmc_access.host)
            .is_empty()
    );

    let machine = host.next_iteration_machine(&env).await;
    assert_eq!(
        machine.current_state(),
        &ManagedHostState::Maintenance { operation }
    );
    let machine = host.next_iteration_machine(&env).await;
    assert_eq!(machine.current_state(), &ManagedHostState::Ready);
    assert!(machine.machine_maintenance_requested.is_none());
    assert!(machine.health_reports.maintenance_override().is_some());
    assert_eq!(
        env.redfish_sim
            .actions_since(&redfish_timepoint)
            .for_host(&bmc_access.host),
        vec![RedfishSimAction::ChassisReset {
            chassis_id: "HGX_Chassis_0".into(),
            reset_type: libredfish::SystemPowerControl::ForceRestart,
        }]
    );
    Ok(())
}

#[crate::sqlx_test]
async fn admin_chassis_reset_rejects_live_instance_even_if_machine_state_is_ready(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let managed_host = create_managed_host(&env).await;
    let host = managed_host.host();
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let _instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    // Allocation committed before the controller observed the live instance.
    let mut txn = env.db_txn().await;
    db::machine::update_state(&mut txn, &host.id, &ManagedHostState::Ready).await?;
    txn.commit().await?;
    let error = env
        .api
        .admin_chassis_reset(reset_request(host.id.into()))
        .await
        .unwrap_err();
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "host is assigned to a tenant; a chassis reset is not allowed"
    );
    let mut txn = env.db_txn().await;
    assert!(
        host.db_machine(&mut txn)
            .await
            .machine_maintenance_requested
            .is_none()
    );
    Ok(())
}

#[crate::sqlx_test]
async fn admin_chassis_reset_rejects_non_ready_hosts(
    db_pool: sqlx::PgPool,
) -> Result<(), eyre::Report> {
    let env = create_test_env(db_pool).await;
    let host = create_managed_host(&env).await.host();
    env.api
        .set_maintenance(Request::new(MaintenanceRequest {
            operation: MaintenanceOperation::Enable.into(),
            host_id: Some(host.id.into()),
            reference: Some("chassis reset state preconditions".into()),
        }))
        .await?;

    let boot_interface = "02:00:00:00:00:01".parse()?;
    let boot_configuring = |boot_config_state| ManagedHostState::BootConfiguring {
        desired_version: ConfigVersion::initial(),
        desired_boot_interface: MachineBootInterfaceTarget::MacOnly(boot_interface),
        post_lock_verification_retry_count: 0,
        boot_config_state,
    };
    for state in [
        ManagedHostState::Created,
        ManagedHostState::Decommissioning {
            decommissioning_state: DecommissioningState::Decommissioned,
        },
        ManagedHostState::Failed {
            details: FailureDetails {
                cause: FailureCause::NVMECleanFailed {
                    err: "initial storage cleanup failed".into(),
                },
                failed_at: chrono::Utc::now(),
                source: FailureSource::StateMachineArea(StateMachineArea::HostInit),
            },
            machine_id: host.id.into(),
            retry_count: 0,
        },
        boot_configuring(ReadyBootConfigState::Prepare),
        boot_configuring(ReadyBootConfigState::Failed {
            failure: "boot configuration failed".into(),
        }),
        boot_configuring(ReadyBootConfigState::CheckHostConfig),
    ] {
        let mut txn = env.db_txn().await;
        db::machine::update_state(&mut txn, &host.id, &state).await?;
        txn.commit().await?;
        let error = env
            .api
            .admin_chassis_reset(reset_request(host.id.into()))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::FailedPrecondition);
        assert_eq!(error.message(), "host state does not allow a chassis reset");
        let mut txn = env.db_txn().await;
        let machine = host.db_machine(&mut txn).await;
        assert_eq!(machine.current_state(), &state);
        assert!(machine.machine_maintenance_requested.is_none());
        assert!(machine.health_reports.maintenance_override().is_some());
    }
    Ok(())
}
