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

use std::str::FromStr;

use carbide_test_harness::prelude::*;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{MachineId, StableHostMachineId};
use rpc::InstanceReleaseRequest;
use rpc::forge::{InstanceAllocationRequest, InstanceConfig};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::IntoRequest;

use super::instance_common::{
    InstanceFixture, default_os_config, default_tenant_config, single_interface_network_config,
};

async fn send_health_report_entry(
    env: &TestHarness,
    machine_id: &MachineId,
    entry: (
        health_report::HealthReport,
        health_report::HealthReportApplyMode,
    ),
) {
    env.api()
        .insert_machine_health_report(tonic::Request::new(
            rpc::forge::InsertMachineHealthReportRequest {
                machine_id: Some(*machine_id),
                health_report_entry: Some(rpc::forge::HealthReportEntry {
                    report: Some(entry.0.into()),
                    mode: entry.1 as i32,
                }),
            },
        ))
        .await
        .unwrap();
}

async fn remove_health_report_entry(env: &TestHarness, machine_id: &MachineId, source: String) {
    env.api()
        .remove_machine_health_report(tonic::Request::new(
            rpc::forge::RemoveMachineHealthReportRequest {
                machine_id: Some(*machine_id),
                source,
            },
        ))
        .await
        .unwrap();
}

#[sqlx_test]
async fn test_cannot_create_instance_on_unhealthy_dpu(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> eyre::Result<()> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;
    let host_machine_id: StableHostMachineId = mh.host.id.try_into().unwrap();

    // Report an unhealthy DPU
    let status = mh
        .first_dpu()
        .network_status()
        .dpu_health(rpc::health::HealthReport {
            source: "forge-dpu-agent".to_string(),
            triggered_by: None,
            observed_at: None,
            successes: vec![],
            alerts: vec![rpc::health::HealthProbeAlert {
                id: "everything".to_string(),
                target: None,
                in_alert_since: None,
                message: "test_cannot_create_instance_on_unhealthy_dpu".to_string(),
                tenant_message: None,
                classifications: vec![
                    health_report::HealthAlertClassification::prevent_allocations().to_string(),
                    health_report::HealthAlertClassification::prevent_host_state_changes()
                        .to_string(),
                ],
            }],
        })
        .build()
        .await;
    env.api()
        .record_dpu_network_status(status.into_request())
        .await
        .expect("DPU network status should be recorded");

    let result = env
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_machine_id)
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(single_interface_network_config(segment_id)),
                )
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await;
    let Err(err) = result else {
        panic!("Creating an instance should have been refused");
    };
    if err.code() != tonic::Code::FailedPrecondition {
        panic!("Expected grpc code FailedPrecondition, got {}", err.code());
    }
    assert_eq!(
        err.message(),
        "host is not available for allocation due to health probe alert"
    );
    Ok(())
}

#[sqlx_test]
async fn test_create_instance_with_allow_unhealthy_machine_true(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;
    let host_machine_id: StableHostMachineId = mh.host.id.try_into().unwrap();

    // Report an unhealthy DPU
    let status = mh
        .first_dpu()
        .network_status()
        .dpu_health(rpc::health::HealthReport {
            source: "forge-dpu-agent".to_string(),
            triggered_by: None,
            observed_at: None,
            successes: vec![],
            alerts: vec![rpc::health::HealthProbeAlert {
                id: "everything".to_string(),
                target: None,
                in_alert_since: None,
                message: "test_cannot_create_instance_on_unhealthy_dpu".to_string(),
                tenant_message: None,
                classifications: vec![
                    health_report::HealthAlertClassification::prevent_allocations().to_string(),
                    health_report::HealthAlertClassification::prevent_host_state_changes()
                        .to_string(),
                ],
            }],
        })
        .build()
        .await;
    env.api()
        .record_dpu_network_status(status.into_request())
        .await
        .expect("DPU network status should be recorded");

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    let instance = env
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(true)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(
                    InstanceConfig::builder()
                        .tenant(default_tenant_config())
                        .os(default_os_config())
                        .network(single_interface_network_config(segment_id)),
                )
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect("Create instance failed.")
        .into_inner();

    assert_eq!(instance.id, Some(instance_id));

    let instance = env
        .api()
        .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![instance_id],
        }))
        .await
        .unwrap()
        .into_inner()
        .instances;
    assert_eq!(instance.len(), 1);
    assert_eq!(instance[0].id.unwrap(), instance_id);
}

/// Release is rejected when aggregate health includes `PreventInstanceDeletion`; succeeds after the override is removed.
#[sqlx_test]
async fn test_instance_release_rejected_when_aggregate_health_has_prevent_instance_deletion(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let InstanceFixture {
        harness: env,
        mh,
        segment_id,
    } = InstanceFixture::new(pool).await;

    let config = InstanceConfig::builder()
        .tenant(default_tenant_config())
        .os(default_os_config())
        .network(single_interface_network_config(segment_id));

    let instance_result = env
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test-prevent-instance-deletion".to_string(),
                    description: "PreventInstanceDeletion blocks release".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect("allocate instance");

    let instance = instance_result.into_inner();
    let instance_id = *instance.id.as_ref().expect("Instance ID should be present");

    let block_release = health_report::HealthReport {
        source: "test-prevent-instance-deletion-override".to_string(),
        triggered_by: None,
        observed_at: Some(chrono::Utc::now()),
        successes: vec![],
        alerts: vec![health_report::HealthProbeAlert {
            id: health_report::HealthProbeId::from_str("TestPreventInstanceDeletion").unwrap(),
            target: None,
            in_alert_since: None,
            message: "hold instance".to_string(),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::prevent_instance_deletion(),
            ],
        }],
    };

    send_health_report_entry(
        &env,
        &mh.host.id.into(),
        (block_release, health_report::HealthReportApplyMode::Merge),
    )
    .await;

    let err = env
        .api()
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .expect_err(
            "release should fail when PreventInstanceDeletion is present on aggregate health",
        );

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message().contains("PreventInstanceDeletion"),
        "unexpected message: {}",
        err.message()
    );

    remove_health_report_entry(
        &env,
        &mh.host.id.into(),
        "test-prevent-instance-deletion-override".to_string(),
    )
    .await;

    env.api()
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .expect("release should succeed after removing PreventInstanceDeletion source");
}
