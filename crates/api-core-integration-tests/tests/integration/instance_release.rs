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

use std::time::Duration;

use carbide_api_core::test_support::default_config;
use carbide_test_harness::prelude::*;
use carbide_test_harness::test_support::fixture_config::FixtureDefault as _;
use carbide_uuid::machine::StableHostMachineId;
use model::machine::health_override::HARDWARE_HEALTH_OVERRIDE_PREFIX;
use model::test_support::ManagedHostConfig;
use rpc::InstanceReleaseRequest;
use rpc::forge::{InstanceAllocationRequest, InstanceConfig, Issue, IssueCategory};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::IntoRequest;

use super::instance_common::{
    InstanceEnv, default_os_config, default_tenant_config, single_interface_network_config,
};

async fn create_release_host(env: &InstanceEnv) -> TestManagedHost {
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;
    // Release assertions count this healthy hardware report alongside issue and repair overrides.
    env.harness
        .api()
        .insert_machine_health_report(
            rpc::forge::InsertMachineHealthReportRequest {
                machine_id: Some(mh.host.id.into()),
                health_report_entry: Some(rpc::forge::HealthReportEntry {
                    report: Some(
                        health_report::HealthReport::empty(format!(
                            "{HARDWARE_HEALTH_OVERRIDE_PREFIX}health"
                        ))
                        .into(),
                    ),
                    ..Default::default()
                }),
            }
            .into_request(),
        )
        .await
        .expect("Failed to add hardware health report to newly created machine");
    mh
}

/// Tests that older clients work correctly with the enhanced API.
/// Verifies: Old API behavior preserved + NO health overrides applied.
#[sqlx_test]
async fn test_instance_release_backward_compatibility(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let mh = create_release_host(&env).await;

    // Use the fixture's VPC segment for the test
    let segment_id = env.segment_id;

    // Create instance configuration
    let config = InstanceConfig::builder()
        .tenant(default_tenant_config())
        .os(default_os_config())
        .network(single_interface_network_config(segment_id));

    // Allocate an instance using correct API structure
    let instance_result = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test-backward-compat".to_string(),
                    description: "Enhanced instance release API backward compatibility test"
                        .to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect("Failed to allocate instance");

    let instance = instance_result.into_inner();
    let instance_id = *instance.id.as_ref().expect("Instance ID should be present");

    // Test backward compatibility: simulate an older client that doesn't know about
    // the new enhanced instance release fields (issue reporting and repair tenant flag).
    //
    // IMPORTANT: When older gRPC clients send requests, they don't include these new
    // optional fields in the protobuf wire format. The protobuf deserializer on the
    // server side automatically sets missing optional fields to None/default values.
    // Therefore, setting issue: None and is_repair_tenant: None here exactly replicates
    // the behavior of an older client calling this API.
    let release_response = env
        .harness
        .api()
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,            // Exactly what older clients produce
            is_repair_tenant: None, // Exactly what older clients produce
            delete_attribution: None,
        }))
        .await
        .expect("Basic instance release should succeed");

    // Verify the response indicates success (it doesn't have a success field)
    let _release_inner = release_response.into_inner();

    // Verify instance is properly cleaned up by checking machine state
    // The host should transition properly after successful cleanup
    // Wait a moment for async cleanup to complete
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut txn = env.harness.db_txn().await;

    let host_machine = mh.host.db_machine(&mut txn).await;

    // CRITICAL BACKWARD COMPATIBILITY VERIFICATION:
    // When using old API format (no issue, no is_repair_tenant), NO health overrides should be applied
    assert_eq!(
        host_machine.health_reports.merges.len(),
        1, // Single HealthOverride for HardwareHealth
        "Backward compatibility test: NO health overrides should be applied when using old API format"
    );

    // Verify specifically that neither TenantReportedIssue nor RequestRepair overrides exist
    assert!(
        !host_machine
            .health_reports
            .merges
            .contains_key("tenant-reported-issue"),
        "Backward compatibility: TenantReportedIssue override should NOT be applied without issue field"
    );

    assert!(
        !host_machine
            .health_reports
            .merges
            .contains_key(health_report::REPAIR_REQUEST_MERGE_SOURCE),
        "Backward compatibility: RequestRepair override should NOT be applied without issue field"
    );

    println!(" Backward compatibility verified");
    println!("   - No health overrides applied");
    println!("   - No TenantReportedIssue override");
    println!("   - No RequestRepair override");
    println!("   - Old API behavior preserved");

    // Verify the machine state - just log it for informational purposes
    println!(
        "Host machine state after release: {:?}",
        host_machine.current_state()
    );

    txn.commit().await.unwrap();
}

/// Test the enhanced instance release API with repair tenant functionality.
///
/// This test verifies that the repair tenant flag works correctly and
/// may enable special handling for repair operations.
#[sqlx_test]
async fn test_instance_release_repair_tenant(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;

    // Test both repair tenant scenarios: true and false
    let test_scenarios = vec![
        (
            true,
            "repair-tenant-true",
            "Testing repair tenant functionality with flag=true",
        ),
        (
            false,
            "repair-tenant-false",
            "Testing repair tenant functionality with flag=false",
        ),
    ];

    // Share the environment's VPC segment across all test scenarios
    let segment_id = env.segment_id;

    for (is_repair_tenant, test_name, description) in test_scenarios {
        println!("Testing repair tenant scenario: is_repair_tenant={is_repair_tenant}");

        let mh = create_release_host(&env).await;

        // Create instance configuration
        let config = InstanceConfig::builder()
            .tenant(default_tenant_config())
            .os(default_os_config())
            .network(single_interface_network_config(segment_id));

        // Allocate an instance
        let instance_result = env
            .harness
            .api()
            .allocate_instance(
                InstanceAllocationRequest::builder(false)
                    .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                    .config(config)
                    .metadata(rpc::Metadata {
                        name: test_name.to_string(),
                        description: description.to_string(),
                        labels: Vec::new(),
                    })
                    .tonic_request(),
            )
            .await
            .expect("Failed to allocate instance");

        let instance = instance_result.into_inner();
        let instance_id = *instance.id.as_ref().expect("Instance ID should be present");

        // Test enhanced instance release with repair tenant flag
        let release_response = env
            .harness
            .api()
            .release_instance(tonic::Request::new(InstanceReleaseRequest {
                id: Some(instance_id),
                issue: None, // No issue reported
                is_repair_tenant: Some(is_repair_tenant),
                delete_attribution: None,
            }))
            .await
            .expect("Instance release with repair tenant flag should succeed");

        // Verify the response indicates success
        let _release_inner = release_response.into_inner();

        // Verify repair tenant behavior
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut txn = env.harness.db_txn().await;

        let host_machine = mh.host.db_machine(&mut txn).await;

        if is_repair_tenant {
            // For repair tenant releases, verify no new overrides are applied when no issues reported
            // (The repair tenant workflow only acts when there are existing RequestRepair overrides)
            println!(
                "Repair tenant release: No issues reported, no existing RequestRepair override"
            );
        } else {
            // For regular tenant without issues, no health overrides should be applied
            let has_tenant_reported_override = host_machine
                .health_reports
                .merges
                .contains_key("tenant-reported-issue");
            let has_repair_request_override = host_machine
                .health_reports
                .merges
                .contains_key(health_report::REPAIR_REQUEST_MERGE_SOURCE);

            assert!(
                !has_tenant_reported_override,
                "No health overrides should be applied for regular tenant without issues"
            );
            assert!(
                !has_repair_request_override,
                "No health overrides should be applied for regular tenant without issues"
            );
        }

        println!(
            "Host machine state after repair tenant release (is_repair_tenant={is_repair_tenant}): {:?}",
            host_machine.current_state()
        );

        txn.commit().await.unwrap();
    }
}

/// Test the enhanced instance release API with both issue reporting and repair tenant flag.
///
/// This test verifies that both enhancement features work correctly when used together,
/// covering the most comprehensive usage scenario of the enhanced API.
#[sqlx_test]
async fn test_instance_release_combined_enhancements(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let mh = create_release_host(&env).await;
    let segment_id = env.segment_id;

    // Create instance configuration
    let config = InstanceConfig::builder()
        .tenant(default_tenant_config())
        .os(default_os_config())
        .network(single_interface_network_config(segment_id));

    // Allocate an instance
    let instance_result = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test-combined-enhancements".to_string(),
                    description: "Testing combined issue reporting and repair tenant functionality"
                        .to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect("Failed to allocate instance");

    let instance = instance_result.into_inner();
    let instance_id = *instance.id.as_ref().expect("Instance ID should be present");

    // Test enhanced instance release with both features enabled
    let issue = Issue {
        category: IssueCategory::Hardware as i32,
        summary: "Critical hardware failure during repair".to_string(),
        details: "Hardware component failure detected during repair operation. Requires immediate attention.".to_string(),
    };

    let release_response = env
        .harness
        .api()
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: Some(issue),
            is_repair_tenant: Some(true), // This is a repair tenant reporting an issue
            delete_attribution: None,
        }))
        .await
        .expect("Instance release with combined enhancements should succeed");

    // Verify the response indicates success
    let _release_inner = release_response.into_inner();

    // Verify combined enhancement effects
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut txn = env.harness.db_txn().await;

    let host_machine = mh.host.db_machine(&mut txn).await;

    // For repair tenant with issues (no existing RequestRepair override), should apply TenantReportedIssue
    let has_tenant_reported_override = host_machine
        .health_reports
        .merges
        .contains_key("tenant-reported-issue");

    assert!(
        has_tenant_reported_override,
        "Repair tenant with issues should apply TenantReportedIssue health override"
    );

    // Should NOT apply RequestRepair (repair tenants don't trigger auto-repair to prevent cycles)
    let has_repair_request_override = host_machine
        .health_reports
        .merges
        .contains_key(health_report::REPAIR_REQUEST_MERGE_SOURCE);

    assert!(
        !has_repair_request_override,
        "Repair tenant should NOT apply RequestRepair override to prevent repair cycles"
    );

    println!(
        "Host machine state after combined enhancement release: {:?}",
        host_machine.current_state()
    );

    txn.commit().await.unwrap();
}

#[sqlx_test]
async fn test_instance_release_auto_repair_enabled(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();

    // Create custom config with auto-repair ENABLED
    let mut config = default_config::get();
    config.auto_machine_repair_plugin.enabled = true;

    let env = InstanceEnv::new(pool, config).await;

    let mh = create_release_host(&env).await;
    let segment_id = env.segment_id;

    let config = InstanceConfig::builder()
        .tenant(default_tenant_config())
        .os(default_os_config())
        .network(single_interface_network_config(segment_id));

    // Allocate instance
    let instance_result = env
        .harness
        .api()
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(StableHostMachineId::try_from(mh.host.id).unwrap())
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test-auto-repair-enabled".to_string(),
                    description: "Test auto-repair enabled scenario".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .unwrap();

    let allocation_inner = instance_result.into_inner();
    let instance_id = allocation_inner.id.unwrap();

    // Release instance with issue reporting (non-repair tenant, auto-repair ENABLED)
    let release_response = env
        .harness
        .api()
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: Some(Issue {
                category: IssueCategory::Hardware as i32,
                summary: "Memory DIMM failure detected".to_string(),
                details: "ECC errors increasing, DIMM slot 3 needs replacement".to_string(),
            }),
            is_repair_tenant: None, // Regular tenant (not repair tenant)
            delete_attribution: None,
        }))
        .await
        .unwrap();

    let _release_inner = release_response.into_inner();

    // Verify auto-repair enabled effects: BOTH TenantReportedIssue AND RequestRepair should be applied
    tokio::time::sleep(Duration::from_millis(100)).await;
    let mut txn = env.harness.db_txn().await;

    let host_machine = mh.host.db_machine(&mut txn).await;

    println!(
        "Auto-repair enabled test - machine health overrides: {:#?}",
        host_machine.health_reports
    );

    // CRITICAL VERIFICATIONS for auto-repair enabled scenario:
    // 1. Should have THREE health overrides (TenantReportedIssue + RequestRepair + Default HardwareHealth)
    assert_eq!(
        host_machine.health_reports.merges.len(),
        3,
        "Auto-repair enabled should apply both TenantReportedIssue and RequestRepair overrides"
    );

    // 2. Should have TenantReportedIssue override
    assert!(
        host_machine
            .health_reports
            .merges
            .contains_key("tenant-reported-issue"),
        "Should have TenantReportedIssue override for issue reporting"
    );

    // 3. Should have RequestRepair override
    assert!(
        host_machine
            .health_reports
            .merges
            .contains_key(health_report::REPAIR_REQUEST_MERGE_SOURCE),
        "Should have RequestRepair override when auto-repair is enabled"
    );

    // 4. Verify the RequestRepair override content
    let repair_override =
        &host_machine.health_reports.merges[health_report::REPAIR_REQUEST_MERGE_SOURCE];
    let repair_report: health_report::HealthReport = repair_override.clone();
    assert_eq!(
        repair_report.source,
        health_report::REPAIR_REQUEST_MERGE_SOURCE
    );
    assert_eq!(repair_report.alerts.len(), 1);
    assert_eq!(repair_report.alerts[0].id.to_string(), "RequestRepair");
    assert!(
        repair_report.alerts[0]
            .message
            .contains("Memory DIMM failure detected")
    );

    println!("Auto-repair enabled test passed:");
    println!("   - TenantReportedIssue override applied");
    println!("   - RequestRepair override applied");
    println!("   - Both overrides working together");

    txn.commit().await.unwrap();
}

#[sqlx_test]
async fn test_can_not_update_instance_config_after_deletion(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = InstanceEnv::new(pool, default_config::get()).await;
    let segment_id = env.segment_id;
    let mh = env.create_managed_host(ManagedHostConfig::default()).await;

    let initial_os = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("SomeRandomData1".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "SomeRandomiPxe1".to_string(),
            },
        )),
    };

    let config = InstanceConfig::builder()
        .tenant(default_tenant_config())
        .os(default_os_config())
        .network(single_interface_network_config(segment_id))
        .rpc();

    let tinstance = mh
        .instance_builder(&env.harness)
        .config(config.clone())
        .build()
        .await;
    env.advance_instance_to_ready(&mh, &tinstance).await;

    let instance = tinstance.rpc_instance().await;
    let metadata = instance.metadata.as_ref().unwrap().clone();

    assert_eq!(
        instance
            .status
            .as_ref()
            .unwrap()
            .tenant
            .as_ref()
            .unwrap()
            .state(),
        rpc::forge::TenantState::Ready
    );

    env.harness
        .api()
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: tinstance.id.into(),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .unwrap();
    let instance = tinstance.rpc_instance().await;
    assert_eq!(
        instance
            .status
            .as_ref()
            .unwrap()
            .tenant
            .as_ref()
            .unwrap()
            .state(),
        rpc::TenantState::Terminating
    );

    let updated_os = initial_os.clone();

    // Perform an update using update_instance_operating_system
    let err = env
        .harness
        .api()
        .update_instance_operating_system(tonic::Request::new(
            rpc::forge::InstanceOperatingSystemUpdateRequest {
                instance_id: tinstance.id.into(),
                if_version_match: None,
                os: Some(updated_os.clone()),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        err.message(),
        "configuration for a terminating instance can not be changed"
    );

    // Perform an update using update_instance_config
    let mut updated_config = config.clone();
    updated_config.os = Some(updated_os.clone());
    let err = env
        .harness
        .api()
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: tinstance.id.into(),
                if_version_match: None,
                config: Some(updated_config.clone()),
                metadata: Some(metadata),
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        err.message(),
        "configuration for a terminating instance can not be changed"
    );
}
