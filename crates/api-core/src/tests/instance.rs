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
use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use ::rpc::forge::forge_server::Forge;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use common::api_fixtures::instance::{
    advance_created_instance_into_ready_state, default_os_config, default_tenant_config,
    interface_network_config_with_devices, single_interface_network_config,
    single_interface_network_config_with_vpc_prefix,
};
use common::api_fixtures::tenant::create_fixture_tenant;
use common::api_fixtures::tpm_attestation::{CA_CERT_SERIALIZED, EK_CERT_SERIALIZED};
use common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_test_env, create_test_env_with_overrides, dpu,
    get_config, inject_machine_measurements, network_configured_with_health,
    network_configured_with_health_and_ext_services, persist_machine_validation_result,
    populate_network_security_groups, site_explorer,
};
use config_version::ConfigVersion;
use db::instance_address::UsedOverlayNetworkIpResolver;
use db::ip_allocator::UsedIpResolver;
use db::network_segment::IdColumn;
use db::{self, ObjectColumnFilter};
use futures_util::future::join_all;
use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use itertools::Itertools;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::dpu_machine_update::DpuMachineUpdate;
use model::instance::config::extension_services::InstanceExtensionServicesConfig;
use model::instance::config::infiniband::InstanceInfinibandConfig;
use model::instance::config::network::{
    DeviceLocator, InstanceInterfaceIpFamilyMode, InstanceInterfaceVpcSelection,
    InstanceNetworkConfig, Ipv6InterfaceConfig, NetworkDetails,
};
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::config::spx::InstanceSpxConfig;
use model::machine::{
    AttestationMode, CleanupContext, CleanupState, FailureDetails, InstanceState, Machine,
    MachineState, MachineValidatingState, ManagedHostState, MeasuringState,
    NetworkConfigUpdateState, SpdmMeasuringState, ValidationState,
};
use model::metadata::Metadata;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    NetworkSegmentControllerState, NetworkSegmentSearchConfig, NetworkSegmentType,
    NewNetworkSegment,
};
use model::tenant::TenantOrganizationId;
use model::test_support::ManagedHostConfig;
use model::vpc_prefix::VpcPrefixConfig;
use rpc::forge::{
    AdminForceDeleteMachineRequest, DpuExtensionService, Issue, IssueCategory,
    ManagedHostQuarantineMode, TpmCaCert, TpmCaCertId,
};
use rpc::{InstanceReleaseRequest, Timestamp};
use sqlx::PgPool;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tonic::Request;

use crate::cfg::file::VmaasConfig;
use crate::instance::{allocate_instance, allocate_network};
use crate::network_segment::allocate::PrefixAllocator;
use crate::test_support::fixture_config::FixtureDefault as _;
use crate::test_support::metadata;
use crate::test_support::network_segment::FIXTURE_TENANT_ORG_ID;
use crate::tests::common;
use crate::tests::common::api_fixtures::instance::single_interface_network_config_with_vfs;
use crate::tests::common::api_fixtures::rpc_instance::RpcInstance;
use crate::tests::common::api_fixtures::{
    TestEnv, TestManagedHost, create_managed_host_multi_dpu, create_managed_host_with_ek,
    remove_health_report_entry, send_health_report_entry, update_time_params,
};
use crate::tests::common::attestation::spdm_attestation_run_to_failed_then_to_success;
use crate::tests::common::rpc_builder::{
    InstanceAllocationRequest, InstanceConfig, InstanceConfigExt, VpcCreationRequest,
};

/// Returns tenant config matching the shared VPC fixture so ownership
/// validation does not obscure the scenario under test.
fn fixture_tenant_config() -> rpc::TenantConfig {
    rpc::TenantConfig {
        tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
        ..default_tenant_config()
    }
}

pub(in crate::tests) async fn find_instances_by_label(
    env: &TestEnv,
    label: rpc::forge::Label,
) -> rpc::forge::InstanceList {
    let instance_ids = env
        .api
        .find_instance_ids(tonic::Request::new(rpc::forge::InstanceSearchFilter {
            label: Some(label),
            tenant_org_id: None,
            vpc_id: None,
            instance_type_id: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .instance_ids;

    env.api
        .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids,
        }))
        .await
        .unwrap()
        .into_inner()
}

#[crate::sqlx_test]
async fn test_allocate_and_release_instance_one_dpu(
    pool_options: PgPoolOptions,
    options: PgConnectOptions,
) {
    test_allocate_and_release_instance_impl(pool_options, options, 1, 1).await
}
#[crate::sqlx_test]
async fn test_allocate_and_release_instance_one_of_two_dpus(
    pool_options: PgPoolOptions,
    options: PgConnectOptions,
) {
    test_allocate_and_release_instance_impl(pool_options, options, 2, 1).await
}
#[crate::sqlx_test]
async fn test_allocate_and_release_instance_two_of_two_dpus(
    pool_options: PgPoolOptions,
    options: PgConnectOptions,
) {
    test_allocate_and_release_instance_impl(pool_options, options, 2, 2).await
}
#[crate::sqlx_test]
async fn test_allocate_and_release_instance_two_of_three_dpus(
    pool_options: PgPoolOptions,
    options: PgConnectOptions,
) {
    test_allocate_and_release_instance_impl(pool_options, options, 3, 2).await
}

async fn test_allocate_and_release_instance_impl(
    _: PgPoolOptions,
    options: PgConnectOptions,
    dpu_count: usize,
    instance_interface_count: usize,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_ids = env.create_vpc_and_tenant_segments(dpu_count).await;

    let vpc_ids: Vec<VpcId> = join_all(segment_ids.iter().map(|id| {
        let pool = env.pool.clone();
        async move {
            db::vpc::find_by_segment(&pool, *id)
                .await
                .map(|vpc| vpc.expect("missing VPC for created segment").id)
        }
    }))
    .await
    .into_iter()
    .collect::<Result<_, _>>()
    .unwrap();

    let mh = create_managed_host_multi_dpu(&env, dpu_count).await;

    let (used_dpu_ids, _unused_dpu_ids) = mh.dpu_ids.split_at(instance_interface_count);

    let mut txn = env.db_txn().await;
    for segment_id in &segment_ids {
        assert_eq!(
            db::instance_address::count_by_segment_id(&mut txn, segment_id)
                .await
                .unwrap(),
            0
        );
    }
    let host_machine = mh.host().db_machine(&mut txn).await;

    let mut device_locators = Vec::default();
    for dpu_machine_id in used_dpu_ids {
        device_locators.push(
            host_machine
                .get_device_locator_for_dpu_id(dpu_machine_id)
                .unwrap(),
        );
    }

    assert!(matches!(
        host_machine.current_state(),
        ManagedHostState::Ready
    ));
    txn.commit().await.unwrap();

    let tinstance = mh
        .instance_builer(&env)
        .network(interface_network_config_with_devices(
            &segment_ids,
            &device_locators,
        ))
        .build()
        .await;

    let instance = tinstance.rpc_instance().await;

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    let tenant_config = instance.config().tenant();
    let expected_os = default_os_config();
    let os = instance.config().os();
    assert_eq!(os, &expected_os);

    let expected_tenant_config = default_tenant_config();
    assert_eq!(tenant_config, &expected_tenant_config);

    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;

    let fetched_instance = snapshot.instance.unwrap();
    assert_eq!(&fetched_instance.machine_id, &mh.host().id.into());
    for (segment_index, segment_id) in segment_ids.iter().enumerate() {
        let expected_count = if segment_index < instance_interface_count {
            1
        } else {
            0
        };
        assert_eq!(
            db::instance_address::count_by_segment_id(&mut txn, segment_id)
                .await
                .unwrap(),
            expected_count
        );
    }
    let network_config = fetched_instance.config.network.clone();
    assert_eq!(fetched_instance.network_config_version.version_nr(), 1);
    let mut network_config_no_addresses = network_config.clone();
    for iface in network_config_no_addresses.interfaces.iter_mut() {
        assert_eq!(iface.ip_addrs.len(), 1);
        assert_eq!(iface.interface_prefixes.len(), 1);
        iface.ip_addrs.clear();
        iface.interface_prefixes.clear();
        iface.network_segment_gateways.clear();
        iface.internal_uuid = uuid::Uuid::nil();
    }
    assert_eq!(
        network_config_no_addresses,
        InstanceNetworkConfig::for_segment_ids(&segment_ids, &device_locators, &vpc_ids)
    );

    assert!(!fetched_instance.observations.network.is_empty());
    assert!(fetched_instance.use_custom_pxe_on_boot);

    let _ = db::instance::use_custom_ipxe_on_next_boot(&mh.host().id, false, &mut txn).await;
    let snapshot = mh.snapshot(&mut txn).await;
    let fetched_instance = snapshot.instance.unwrap();
    txn.commit().await.unwrap();

    let mut txn = env.db_txn().await;
    // TODO: The MAC here doesn't matter. It's not used for lookup
    let records = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut txn,
        &fetched_instance.id,
        segment_ids.first().unwrap(),
    )
    .await
    .unwrap();
    let [record] = records.as_slice() else {
        panic!("expected one address on the first segment")
    };

    // This should the first IP. Algo does not look into machine_interface_addresses
    // table for used addresses for instance.
    assert_eq!(record.address.to_string(), "192.0.4.3");
    assert_eq!(
        &record.address,
        network_config.interfaces[0]
            .ip_addrs
            .iter()
            .next()
            .unwrap()
            .1
    );

    assert_eq!(
        format!("{}/32", record.address),
        network_config.interfaces[0]
            .interface_prefixes
            .iter()
            .next()
            .unwrap()
            .1
            .to_string()
    );

    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    ));
    txn.commit().await.unwrap();

    tinstance.delete().await;

    // Address is freed during delete
    let mut txn = env.db_txn().await;
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Ready
    ));
    for segment_id in &segment_ids {
        assert_eq!(
            db::instance_address::count_by_segment_id(&mut txn, segment_id)
                .await
                .unwrap(),
            0
        );
    }
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn test_measurement_assigned_ready_to_waiting_for_measurements_to_ca_failed_to_ready(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();

    let mut config = get_config();
    config.attestation_enabled = true;
    config.spdm.enabled = true;

    // set the NRAS Verifier Mock Verifier to satisfy requests, but we'll later
    // flip it to fail them
    let mut overrides = TestEnvOverrides::with_config(config);
    let nras_should_fail_parsing_flag = Arc::new(AtomicBool::new(false));

    overrides.nras_should_fail_parsing = Some(nras_should_fail_parsing_flag.clone());

    let env = create_test_env_with_overrides(pool, overrides).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let vpc_id = db::vpc::find_by_segment(&env.pool, segment_id)
        .await
        .unwrap()
        .unwrap()
        .id;
    // add CA cert to pass attestation process
    let add_ca_request = tonic::Request::new(TpmCaCert {
        ca_cert: CA_CERT_SERIALIZED.to_vec(),
    });

    let inserted_cert = env
        .api
        .tpm_add_ca_cert(add_ca_request)
        .await
        .expect("Failed to add CA cert")
        .into_inner();

    let mh = create_managed_host_with_ek(&env, &EK_CERT_SERIALIZED).await;

    let mut txn = env.db_txn().await;
    //let dpu_loopback_ip = dpu::loopback_ip(&mut txn, &dpu_machine_id).await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id)
            .await
            .unwrap(),
        0
    );

    let host_machine = mh.host().db_machine(&mut txn).await;
    assert!(matches!(
        host_machine.current_state(),
        ManagedHostState::Ready
    ));
    txn.commit().await.unwrap();

    let device_locator = host_machine
        .get_device_locator_for_dpu_id(&mh.dpu_ids[0])
        .unwrap();

    // send the request to create the instance
    let instance_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(interface_network_config_with_devices(
            &[segment_id],
            std::slice::from_ref(&device_locator),
        )),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };
    let instance_id = env
        .api
        .allocate_instance(tonic::Request::new(rpc::InstanceAllocationRequest {
            instance_id: None,
            machine_id: Some(mh.host().id.into()),
            instance_type_id: None,
            config: Some(instance_config),
            metadata: None,
            allow_unhealthy_machine: false,
        }))
        .await
        .expect("Create instance failed.")
        .into_inner()
        .id
        .expect("Missing instance ID");

    // Do SPDM attestation: first to failed, then to success
    let mut txn = env.db_txn().await;
    nras_should_fail_parsing_flag.store(true, Ordering::Relaxed);

    spdm_attestation_run_to_failed_then_to_success(
        &env,
        nras_should_fail_parsing_flag.clone(),
        &mh,
        &mut txn,
        ManagedHostState::PreAssignedMeasuring {
            spdm_measuring_state: SpdmMeasuringState::PollResult,
        },
    )
    .await;

    advance_created_instance_into_ready_state(&env, &mh).await;

    // fetch the rpc instance from the db
    let get_rpc_instance = async || {
        let mut result = env
            .api
            .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
                instance_ids: vec![instance_id],
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(result.instances.len(), 1);
        RpcInstance::new(result.instances.remove(0))
    };

    let instance = get_rpc_instance().await;

    // ------

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    let tenant_config = instance.config().tenant();
    let expected_os = default_os_config();
    let os = instance.config().os();
    assert_eq!(os, &expected_os);

    let expected_tenant_config = default_tenant_config();
    assert_eq!(tenant_config, &expected_tenant_config);

    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;

    let fetched_instance = snapshot.instance.unwrap();
    assert_eq!(fetched_instance.machine_id, mh.host().id.into());
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id)
            .await
            .unwrap(),
        1
    );

    let network_config = fetched_instance.config.network.clone();
    assert_eq!(fetched_instance.network_config_version.version_nr(), 1);
    let mut network_config_no_addresses = network_config.clone();
    for iface in network_config_no_addresses.interfaces.iter_mut() {
        assert_eq!(iface.ip_addrs.len(), 1);
        assert_eq!(iface.interface_prefixes.len(), 1);
        iface.ip_addrs.clear();
        iface.interface_prefixes.clear();
        iface.network_segment_gateways.clear();
        iface.internal_uuid = uuid::Uuid::nil();
    }
    assert_eq!(
        network_config_no_addresses,
        InstanceNetworkConfig::for_segment_ids(&[segment_id], &[device_locator], &[vpc_id])
    );

    assert!(!fetched_instance.observations.network.is_empty());
    assert!(fetched_instance.use_custom_pxe_on_boot);

    let _ = db::instance::use_custom_ipxe_on_next_boot(&mh.host().id, false, &mut txn).await;
    let snapshot = mh.snapshot(&mut txn).await;

    let fetched_instance = snapshot.instance.unwrap();

    assert!(!fetched_instance.use_custom_pxe_on_boot);
    txn.commit().await.unwrap();

    let mut txn = env.db_txn().await;
    // TODO: The MAC here doesn't matter. It's not used for lookup
    let segment = db::network_segment::find_by_name(&mut txn, "TENANT")
        .await
        .unwrap();
    let records = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut txn,
        &fetched_instance.id,
        &segment.id,
    )
    .await
    .unwrap();
    let [record] = records.as_slice() else {
        panic!("expected one address on the tenant segment")
    };

    // This should the first IP. Algo does not look into machine_interface_addresses
    // table for used addresses for instance.
    assert_eq!(record.address.to_string(), "192.0.4.3");
    assert_eq!(
        &record.address,
        network_config.interfaces[0]
            .ip_addrs
            .iter()
            .next()
            .unwrap()
            .1
    );

    assert_eq!(
        format!("{}/32", record.address),
        network_config.interfaces[0]
            .interface_prefixes
            .iter()
            .next()
            .unwrap()
            .1
            .to_string()
    );

    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    ));
    txn.commit().await.unwrap();

    // from delete_instance()
    env.api
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .expect("Delete instance failed.");

    // The instance should show up immediatly as terminating - even if the state handler didn't yet run
    let instance = get_rpc_instance().await;
    assert_eq!(instance.status().tenant(), rpc::TenantState::Terminating);

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        8,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::HostPlatformConfiguration {
                platform_config_state:
                    model::machine::HostPlatformConfigurationState::CheckHostConfig,
            },
        },
    )
    .await;

    mh.network_configured(&env).await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        2,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::WaitingForDpusToUp,
        },
    )
    .await;

    mh.network_configured(&env).await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::BootingWithDiscoveryImage {
                retry: model::machine::RetryInfo { count: 0 },
            },
        },
    )
    .await;

    // handle_delete_post_bootingwithdiscoveryimage()

    let mut txn = env.db_txn().await;
    let machine = mh.host().db_machine(&mut txn).await;
    db::machine::update_reboot_time(&machine, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    // Run state machine twice.
    // First DeletingManagedResource updates use_admin_network, transitions to WaitingForNetworkReconfig
    // Second to discover we are now in WaitingForNetworkReconfig
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        2,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::WaitingForNetworkReconfig,
        },
    )
    .await;

    // Apply switching back to admin network
    mh.network_configured(&env).await;

    // now we should be in waiting for measurument state
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        2,
        ManagedHostState::PostAssignedMeasuring {
            attestation_mode: AttestationMode::MeasuredBoot {
                measuring_state: MeasuringState::WaitingForMeasurements,
            },
        },
    )
    .await;

    // remove ca cert and inject measurements, now we should go to failed ca
    // validation state
    let delete_ca_certs_request = tonic::Request::new(TpmCaCertId {
        ca_cert_id: inserted_cert.id.unwrap().ca_cert_id,
    });
    env.api
        .tpm_delete_ca_cert(delete_ca_certs_request)
        .await
        .unwrap();

    inject_machine_measurements(&env, mh.host().id).await;

    for _ in 0..5 {
        env.run_machine_state_controller_iteration().await;
    }

    // check that it has failed as intended due to the lack of ca cert
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    assert!(matches!(
        host.current_state(),
        ManagedHostState::Failed {
            details: FailureDetails {
                cause: model::machine::FailureCause::MeasurementsCAValidationFailed { .. },
                ..
            },
            ..
        }
    ));
    txn.commit().await.unwrap();

    // now re-add the ca cert
    let add_ca_request = tonic::Request::new(TpmCaCert {
        ca_cert: CA_CERT_SERIALIZED.to_vec(),
    });

    env.api
        .tpm_add_ca_cert(add_ca_request)
        .await
        .expect("Failed to add CA cert");

    // perform SPDM attestation, set up the NRAS Verifier Mock
    // to fail
    let mut txn = env.db_txn().await;
    nras_should_fail_parsing_flag.store(true, Ordering::Relaxed);

    spdm_attestation_run_to_failed_then_to_success(
        &env,
        nras_should_fail_parsing_flag,
        &mh,
        &mut txn,
        ManagedHostState::PostAssignedMeasuring {
            attestation_mode: AttestationMode::SpdmAttestation {
                spdm_measuring_state: SpdmMeasuringState::PollResult,
            },
        },
    )
    .await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        5,
        ManagedHostState::WaitingForCleanup {
            cleanup_state: CleanupState::HostCleanup {
                boss_controller_id: None,
            },
            cleanup_context: CleanupContext::Deprovision,
        },
    )
    .await;

    let mut txn = env.db_txn().await;
    let machine = mh.host().db_machine(&mut txn).await;
    db::machine::update_reboot_time(&machine, &mut txn)
        .await
        .unwrap();
    db::machine::update_cleanup_time(&machine, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        3,
        ManagedHostState::Validation {
            validation_state: ValidationState::MachineValidation {
                machine_validation: MachineValidatingState::MachineValidating {
                    context: "Cleanup".to_string(),
                    id: MachineValidationId::new(),
                    completed: 1,
                    total: 1,
                    is_enabled: true,
                },
            },
        },
    )
    .await;

    let mut machine_validation_result = rpc::forge::MachineValidationResult {
        validation_id: None,
        name: "instance".to_string(),
        description: "desc".to_string(),
        command: "echo".to_string(),
        args: "test".to_string(),
        std_out: "".to_string(),
        std_err: "".to_string(),
        context: "Cleanup".to_string(),
        exit_code: 0,
        start_time: Some(Timestamp::from(SystemTime::now())),
        end_time: Some(Timestamp::from(SystemTime::now())),
        test_id: Some("test1".to_string()),
    };

    let response = mh.host().forge_agent_control().await;
    let uuid = &response.data.unwrap().pair[1].value;
    let validation_id: MachineValidationId = uuid.parse().unwrap();

    machine_validation_result.validation_id = Some(validation_id);
    persist_machine_validation_result(&env, machine_validation_result.clone()).await;

    let mut txn = env.db_txn().await;
    db::machine::update_machine_validation_time(&mh.host().id, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        3,
        ManagedHostState::HostInit {
            machine_state: MachineState::Discovered {
                skip_reboot_wait: false,
            },
        },
    )
    .await;

    let mut txn = env.db_txn().await;
    let machine = mh.host().db_machine(&mut txn).await;
    db::machine::update_reboot_time(&machine, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        3,
        ManagedHostState::Ready,
    )
    .await;

    // end of handle_delete_post_bootingwithdiscoveryimage()

    assert!(
        env.find_instances(vec![instance_id])
            .await
            .instances
            .is_empty()
    );

    // end of delete_instance()

    // Address is freed during delete
    let mut txn = env.db_txn().await;
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Ready
    ));
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id)
            .await
            .unwrap(),
        0
    );
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn test_allocate_instance_with_labels(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let txn = env
        .pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    txn.commit().await.unwrap();

    let instance_metadata = rpc::forge::Metadata {
        name: "test_instance_with_labels".to_string(),
        description: "this instance must have labels.".to_string(),
        labels: vec![
            rpc::forge::Label {
                key: "key1".to_string(),
                value: Some("value1".to_string()),
            },
            rpc::forge::Label {
                key: "key2".to_string(),
                value: None,
            },
        ],
    };

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .metadata(instance_metadata.clone())
        .build()
        .await;

    // Test searching based on instance id.
    let mut instance_matched_by_id = tinstance.rpc_instance().await.into_inner();

    instance_matched_by_id.metadata = instance_matched_by_id.metadata.take().map(|mut metadata| {
        metadata.labels.sort_by(|l1, l2| l1.key.cmp(&l2.key));
        metadata
    });

    assert_eq!(
        instance_matched_by_id.metadata,
        Some(instance_metadata.clone())
    );

    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;

    let fetched_instance = snapshot.instance.unwrap();
    assert_eq!(fetched_instance.machine_id, mh.host().id.into());

    assert_eq!(fetched_instance.metadata.name, "test_instance_with_labels");
    assert_eq!(
        fetched_instance.metadata.description,
        "this instance must have labels."
    );
    assert!(fetched_instance.metadata.labels.len() == 2);
    assert_eq!(
        fetched_instance.metadata.labels.get("key1").unwrap(),
        "value1"
    );
    assert_eq!(fetched_instance.metadata.labels.get("key2").unwrap(), "");

    let mut instance_matched_by_label = find_instances_by_label(
        &env,
        rpc::forge::Label {
            key: "key1".to_string(),
            value: None,
        },
    )
    .await
    .instances
    .remove(0);

    instance_matched_by_label.metadata =
        instance_matched_by_label
            .metadata
            .take()
            .map(|mut metadata| {
                metadata.labels.sort_by(|l1, l2| l1.key.cmp(&l2.key));
                metadata
            });

    assert_eq!(
        instance_matched_by_label.machine_id.unwrap(),
        mh.host().id.into()
    );

    assert_eq!(instance_matched_by_label.metadata, Some(instance_metadata));
}

#[crate::sqlx_test]
async fn test_allocate_instance_with_invalid_metadata(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    for (invalid_metadata, expected_err) in metadata::invalid_metadata_testcases(true) {
        let tenant_config = default_tenant_config();
        let config = InstanceConfig::builder()
            .tenant(tenant_config)
            .os(default_os_config())
            .network(single_interface_network_config(segment_id))
            .rpc();

        let result = env
            .api
            .allocate_instance(
                InstanceAllocationRequest::builder(false)
                    .machine_id(host_machine_id)
                    .config(config)
                    .metadata(invalid_metadata.clone())
                    .tonic_request(),
            )
            .await;

        let err = result.expect_err(&format!(
            "Invalid metadata of type should not be accepted: {invalid_metadata:?}"
        ));

        assert_eq!(err.code(), tonic::Code::InvalidArgument);
        assert!(
            err.message().contains(&expected_err),
            "Testcase: {:?}\nMessage is \"{}\".\nMessage should contain: \"{}\"",
            invalid_metadata,
            err.message(),
            expected_err
        );
    }
}

#[crate::sqlx_test]
async fn test_instance_hostname_creation(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let txn = env
        .pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    txn.commit().await.unwrap();

    let instance_hostname = "test-hostname";

    mh.instance_builer(&env)
        .single_interface_network_config(segment_id)
        .hostname(instance_hostname)
        .tenant_org("org-nebulon")
        .build()
        .await;

    let mut txn = env.db_txn().await;

    let snapshot = mh.snapshot(&mut txn).await;

    let fetched_instance = snapshot.instance.unwrap();

    let returned_hostname = fetched_instance.config.tenant.hostname;

    assert_eq!(returned_hostname.unwrap(), instance_hostname);

    //Check for duplicate hostnames
    let txn = env
        .pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    txn.commit().await.unwrap();

    create_managed_host(&env)
        .await
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .hostname(instance_hostname)
        .tenant_org("org-nvidia") // different org, should fail on the same one
        .build()
        .await;
}

#[crate::sqlx_test]
async fn test_instance_dns_resolution(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let (segment_id_1, segment_id_2) = env.create_vpc_and_dual_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let network = rpc::InstanceNetworkConfig {
        interfaces: vec![
            rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical as i32,
                network_segment_id: Some(segment_id_1),
                network_details: None,
                device: None,
                device_instance: 0u32,
                virtual_function_id: None,
                ip_address: None,
                ipv6_interface_config: None,
                routing_profile: None,
            },
            rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Virtual as i32,
                network_segment_id: Some(segment_id_2),
                network_details: None,
                device: None,
                device_instance: 0u32,
                virtual_function_id: None,
                ip_address: None,
                ipv6_interface_config: None,
                routing_profile: None,
            },
        ],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };

    // Create instance with hostname
    mh.instance_builer(&env)
        .network(network)
        .hostname("test-hostname")
        .tenant_org("nvidia-org")
        .build()
        .await;

    let response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(mh.dpu().id.into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    //DNS record domain always uses IP Address (for now)
    let dns_record = env
        .api
        .lookup_record(tonic::Request::new(
            rpc::protos::dns::DnsResourceRecordLookupRequest {
                qname: "192-0-2-3.dwrt1.com.".to_string(),
                zone_id: uuid::Uuid::new_v4().to_string(),
                local: None,
                remote: None,
                qtype: "A".to_string(),
                real_remote: None,
            },
        ))
        .await
        .unwrap()
        .into_inner();

    tracing::info!(
        dns_record = ?dns_record,
        "Fetched DNS record",
    );
    assert_eq!(dns_record.records.first().unwrap().content, "192.0.2.3");

    //DHCP response uses hostname set during allocation
    assert_eq!(
        "test-hostname.dwrt1.com",
        response.tenant_interfaces[0].fqdn
    );
}

#[crate::sqlx_test]
async fn test_instance_null_hostname(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    //Create instance with no hostname set
    let mut tenant_config = default_tenant_config();
    tenant_config.hostname = None;
    let instance_config = InstanceConfig::builder()
        .tenant(tenant_config)
        .os(default_os_config())
        .network(single_interface_network_config(segment_id))
        .rpc();

    mh.instance_builer(&env)
        .config(instance_config)
        .build()
        .await;

    let _response = env
        .api
        .get_managed_host_network_config(tonic::Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(mh.dpu().id.into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    //DNS record domain always uses dashed IP (for now)
    let dns_record = env
        .api
        .lookup_record(tonic::Request::new(
            rpc::protos::dns::DnsResourceRecordLookupRequest {
                qname: "192-0-2-3.dwrt1.com.".to_string(),
                zone_id: uuid::Uuid::new_v4().to_string(),
                local: None,
                remote: None,
                qtype: "A".to_string(),
                real_remote: None,
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(dns_record.records.first().unwrap().content, "192.0.2.3");

    //DHCP response uses dashed IP
    assert_eq!(
        dns_record.records.first().unwrap().qname,
        "192-0-2-3.dwrt1.com."
    );
}

#[crate::sqlx_test]
async fn test_instance_search_based_on_labels(pool: sqlx::PgPool) {
    let env = create_test_env(pool.clone()).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    for i in 0..=9 {
        let mh = create_managed_host(&env).await;

        mh.instance_builer(&env)
            .single_interface_network_config(segment_id)
            .metadata(rpc::forge::Metadata {
                name: format!("instance_{i}{i}{i}").to_string(),
                description: format!("instance_{i}{i}{i} have labels").to_string(),
                labels: vec![
                    rpc::forge::Label {
                        key: format!("key_A_{i}{i}{i}").to_string(),
                        value: Some(format!("value_A_{i}{i}{i}").to_string()),
                    },
                    rpc::forge::Label {
                        key: format!("key_B_{i}{i}{i}").to_string(),
                        value: None,
                    },
                ],
            })
            .build()
            .await;
    }

    // Test searching based on value.
    let instance_matched_by_label = find_instances_by_label(
        &env,
        rpc::forge::Label {
            key: "".to_string(),
            value: Some("value_A_444".to_string()),
        },
    )
    .await
    .instances
    .remove(0);

    assert_eq!(
        instance_matched_by_label.metadata.unwrap().name,
        "instance_444"
    );

    // Test searching based on key.
    let instance_matched_by_label = find_instances_by_label(
        &env,
        rpc::forge::Label {
            key: "key_A_111".to_string(),
            value: None,
        },
    )
    .await
    .instances
    .remove(0);

    assert_eq!(
        instance_matched_by_label.metadata.unwrap().name,
        "instance_111"
    );

    // Test searching based on key and value.
    let instance_matched_by_label = find_instances_by_label(
        &env,
        rpc::forge::Label {
            key: "key_A_888".to_string(),
            value: Some("value_A_888".to_string()),
        },
    )
    .await
    .instances
    .remove(0);

    assert_eq!(
        instance_matched_by_label.metadata.unwrap().name,
        "instance_888"
    );
}

#[crate::sqlx_test]
async fn test_create_instance_with_provided_id(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id));

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    let instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(config)
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

    let instance = env.one_instance(instance_id).await;
    assert_eq!(instance.inner().id, Some(instance_id));
}

#[crate::sqlx_test]
async fn test_instance_deletion_before_provisioning_finishes(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Create an instance in non-ready state
    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id));

    let instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.host().id)
                .config(config)
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
    assert_eq!(
        instance
            .status
            .as_ref()
            .unwrap()
            .tenant
            .as_ref()
            .unwrap()
            .state(),
        rpc::TenantState::Provisioning
    );

    let instance_id = instance.id.expect("Missing instance ID");

    // Stop provisioning at the point that normally requires a DPU network-status
    // observation. Releasing here must not wait for that observation.
    env.run_network_segment_controller_iteration().await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForNetworkConfig,
        },
    )
    .await;

    env.api
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .expect("Delete instance failed.");

    let instance = env.one_instance(instance_id).await;
    assert_eq!(instance.status().tenant(), rpc::TenantState::Terminating);

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForRebootToReady,
        },
    )
    .await;
    env.run_machine_state_controller_iteration().await;
    mh.host().reboot_completed().await;

    // Now go through regular deletion
    mh.delete_instance(&env, instance_id).await;
}

#[crate::sqlx_test]
async fn test_instance_waits_for_primary_dpu_bgp_before_pxe_reboot(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    const PRIMARY_DPU_BGP_WAIT_REASON: &str =
        "Waiting for the primary DPU p0 BGP session to be established";
    const HOST_HEALTH_WAIT_REASON: &str =
        "Waiting for lifecycle-blocking host health alerts to clear before PXE reboot";

    fn dpu_health_alert(
        id: &str,
        target: Option<&str>,
        classifications: Vec<health_report::HealthAlertClassification>,
    ) -> rpc::health::HealthReport {
        rpc::health::HealthReport {
            source: "forge-dpu-agent".to_string(),
            triggered_by: None,
            observed_at: None,
            successes: vec![],
            alerts: vec![rpc::health::HealthProbeAlert {
                id: id.to_string(),
                target: target.map(str::to_string),
                in_alert_since: None,
                message: "test lifecycle gate".to_string(),
                tenant_message: None,
                classifications: classifications
                    .into_iter()
                    .map(|classification| classification.to_string())
                    .collect(),
            }],
        }
    }

    fn post_config_wait_health() -> rpc::health::HealthReport {
        dpu_health_alert(
            health_report::HealthProbeId::post_config_check_wait().as_str(),
            None,
            vec![
                health_report::HealthAlertClassification::prevent_allocations(),
                health_report::HealthAlertClassification::prevent_host_state_changes(),
            ],
        )
    }

    fn bgp_tor_health(target: &str, prevent_allocations: bool) -> rpc::health::HealthReport {
        let classifications = prevent_allocations
            .then(health_report::HealthAlertClassification::prevent_allocations)
            .into_iter()
            .collect();
        dpu_health_alert(
            health_report::HealthProbeId::bgp_peering_tor().as_str(),
            Some(target),
            classifications,
        )
    }

    fn pxe_blocking_bgp_merge(source: &str) -> health_report::HealthReport {
        health_report::HealthReport {
            source: source.to_string(),
            triggered_by: None,
            observed_at: None,
            successes: vec![],
            alerts: vec![health_report::HealthProbeAlert {
                id: health_report::HealthProbeId::bgp_peering_tor(),
                target: Some("p0_if".to_string()),
                in_alert_since: None,
                message: "test additive PXE gate".to_string(),
                tenant_message: None,
                classifications: vec![
                    health_report::HealthAlertClassification::prevent_allocations(),
                ],
            }],
        }
    }

    async fn assert_instance_state(env: &TestEnv, mh: &TestManagedHost, expected: InstanceState) {
        let mut txn = env.db_txn().await;
        assert_eq!(
            mh.host().db_machine(&mut txn).await.current_state(),
            &ManagedHostState::Assigned {
                instance_state: expected,
            }
        );
        txn.commit().await.unwrap();
    }

    async fn assert_pxe_reboot_wait_outcome(
        env: &TestEnv,
        mh: &TestManagedHost,
        expected_reason: &str,
    ) {
        let mut txn = env.db_txn().await;
        let host_machine = mh.host().db_machine(&mut txn).await;
        let Some(PersistentStateHandlerOutcome::Wait {
            reason,
            source_ref: Some(source_ref),
        }) = &host_machine.controller_state_outcome
        else {
            panic!("The PXE reboot gate should persist a Wait outcome with a source reference");
        };
        assert_eq!(reason, expected_reason);
        assert!(source_ref.file.ends_with("/handler.rs"));
        txn.commit().await.unwrap();
    }

    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;
    let dpu_id = mh.dpu_ids[0];

    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id));
    env.api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.host().id)
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect("Create instance failed.");

    env.run_network_segment_controller_iteration().await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForNetworkConfig,
        },
    )
    .await;

    // Both the configuration settle period and a failed p0 session hold the
    // initial network configuration gate.
    network_configured_with_health(&env, &dpu_id, Some(post_config_wait_health())).await;
    env.run_machine_state_controller_iteration().await;
    assert_instance_state(&env, &mh, InstanceState::WaitingForNetworkConfig).await;

    network_configured_with_health(&env, &dpu_id, Some(bgp_tor_health("p0_if", true))).await;
    env.run_machine_state_controller_iteration().await;
    assert_instance_state(&env, &mh, InstanceState::WaitingForNetworkConfig).await;

    // An empty DPU Replace report masks the agent Merge report and allows
    // provisioning to advance to the reboot gate.
    send_health_report_entry(
        &env,
        &dpu_id,
        (
            health_report::HealthReport::empty("test-pxe-bgp-override".to_string()),
            health_report::HealthReportApplyMode::Replace,
        ),
    )
    .await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForRebootToReady,
        },
    )
    .await;

    // Removing the Replace report exposes the agent p0 alert again, so the
    // reboot gate must recheck it before restarting the host.
    remove_health_report_entry(&env, &dpu_id, "test-pxe-bgp-override".to_string()).await;
    env.run_machine_state_controller_iteration().await;
    assert_instance_state(&env, &mh, InstanceState::WaitingForRebootToReady).await;
    assert_pxe_reboot_wait_outcome(&env, &mh, PRIMARY_DPU_BGP_WAIT_REASON).await;

    // The reboot gate also rechecks health alerts that block host state
    // changes and persists a diagnosable wait reason.
    network_configured_with_health(&env, &dpu_id, Some(post_config_wait_health())).await;
    env.run_machine_state_controller_iteration().await;
    assert_instance_state(&env, &mh, InstanceState::WaitingForRebootToReady).await;
    assert_pxe_reboot_wait_outcome(&env, &mh, HOST_HEALTH_WAIT_REASON).await;

    // A visible p1 alert permits reboot, but an additional primary DPU Merge
    // report that carries the p0 classification still blocks it.
    network_configured_with_health(&env, &dpu_id, Some(bgp_tor_health("p1_if", false))).await;
    send_health_report_entry(
        &env,
        &dpu_id,
        (
            pxe_blocking_bgp_merge("test-pxe-bgp-merge"),
            health_report::HealthReportApplyMode::Merge,
        ),
    )
    .await;
    env.run_machine_state_controller_iteration().await;
    assert_instance_state(&env, &mh, InstanceState::WaitingForRebootToReady).await;

    // Clearing the last PXE blocking Merge report lets provisioning reach Ready.
    remove_health_report_entry(&env, &dpu_id, "test-pxe-bgp-merge".to_string()).await;
    env.run_machine_state_controller_iteration().await;
    mh.host().reboot_completed().await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await;
}

#[crate::sqlx_test]
async fn test_instance_release_waits_for_primary_dpu_network_health_before_discovery_reboot(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    const DPU_NETWORK_READY_WAIT_REASON: &str =
        "Waiting for the primary DPU network to become ready before PXE reboot";

    fn agent_health(alert_id: Option<health_report::HealthProbeId>) -> rpc::health::HealthReport {
        rpc::health::HealthReport {
            source: "forge-dpu-agent".to_string(),
            triggered_by: None,
            observed_at: None,
            successes: vec![],
            alerts: alert_id
                .map(|id| rpc::health::HealthProbeAlert {
                    id: id.to_string(),
                    target: None,
                    in_alert_since: None,
                    message: "test primary DPU network health".to_string(),
                    tenant_message: None,
                    classifications: vec![
                        health_report::HealthAlertClassification::prevent_allocations().to_string(),
                        health_report::HealthAlertClassification::prevent_host_state_changes()
                            .to_string(),
                    ],
                })
                .into_iter()
                .collect(),
        }
    }

    async fn assert_release_is_waiting(env: &TestEnv, mh: &TestManagedHost, scenario: &str) {
        let mut txn = env.db_txn().await;
        let host = mh.host().db_machine(&mut txn).await;
        assert_eq!(
            host.current_state(),
            &ManagedHostState::Assigned {
                instance_state: InstanceState::WaitingForDpusToUp,
            },
            "{scenario}",
        );
        let Some(PersistentStateHandlerOutcome::Wait { reason, .. }) =
            &host.controller_state_outcome
        else {
            panic!("{scenario}: release gate should persist a Wait outcome");
        };
        assert_eq!(reason, DPU_NETWORK_READY_WAIT_REASON, "{scenario}");
        txn.commit().await.unwrap();
    }

    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;
    let instance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    env.api
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(instance.id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .expect("Delete instance failed.");

    let mut txn = env.db_txn().await;
    db::machine::update_state(
        &mut txn,
        &mh.host().id,
        &ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForDpusToUp,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    let redfish_checkpoint = env.redfish_sim.timepoint();
    network_configured_with_health(
        &env,
        &mh.dpu_ids[0],
        Some(agent_health(Some(
            health_report::HealthProbeId::nvue_api_running(),
        ))),
    )
    .await;
    env.run_machine_state_controller_iteration().await;
    assert_release_is_waiting(&env, &mh, "NVUE API is unavailable").await;

    let power_actions = env
        .redfish_sim
        .actions_since(&redfish_checkpoint)
        .all_hosts()
        .into_iter()
        .filter(|action| matches!(action, RedfishSimAction::Power(_)))
        .collect::<Vec<_>>();
    assert!(
        power_actions.is_empty(),
        "release must not reboot the host before primary DPU network health is ready: {power_actions:?}",
    );

    network_configured_with_health(&env, &mh.dpu_ids[0], Some(agent_health(None))).await;
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.db_txn().await;
    assert_eq!(
        mh.host().db_machine(&mut txn).await.current_state(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage {
                retry: model::machine::RetryInfo { count: 0 },
            },
        }
    );
    txn.commit().await.unwrap();

    let power_actions = env
        .redfish_sim
        .actions_since(&redfish_checkpoint)
        .all_hosts()
        .into_iter()
        .filter(|action| matches!(action, RedfishSimAction::Power(_)))
        .collect::<Vec<_>>();
    assert_eq!(
        power_actions,
        vec![RedfishSimAction::Power(
            libredfish::SystemPowerControl::ForceRestart,
        )],
    );
}

#[crate::sqlx_test]
async fn test_instance_deletion_is_idempotent(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    // We can call `release_instance` multiple times
    for i in 0..2 {
        env.api
            .release_instance(tonic::Request::new(InstanceReleaseRequest {
                id: Some(tinstance.id),
                issue: None,
                is_repair_tenant: None,
                delete_attribution: None,
            }))
            .await
            .unwrap_or_else(|_| panic!("Delete instance failed failed on attempt {i}."));
        let instance = tinstance.rpc_instance().await;
        assert_eq!(instance.status().tenant(), rpc::TenantState::Terminating);
    }

    // And finally delete the instance
    tinstance.delete().await;

    // Release instance on non-existing instance should lead to a Not Found error
    let err = env
        .api
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(tinstance.id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .expect_err("Expect deletion to fail");
    assert_eq!(err.code(), tonic::Code::NotFound);
    let err_msg = err.message();
    assert_eq!(
        err.message(),
        format!("instance not found: {}", tinstance.id),
        "Error message is: {}",
        err_msg
    );
}

#[crate::sqlx_test]
async fn test_can_not_create_2_instances_with_same_id(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();
    let (host_machine_id_2, _dpu_machine_id_2) = create_managed_host(&env).await.into();

    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id))
        .rpc();

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    let instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(config.clone())
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

    let result = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id_2)
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await;

    // TODO: Do not leak the full database error to users
    let err = result.expect_err("Expect instance creation to fail");
    assert!(err.message().contains("Database Error: error returned from database: duplicate key value violates unique constraint \"instances_pkey\""));
}

#[crate::sqlx_test]
async fn test_instance_cloud_init_metadata(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> eyre::Result<()> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.db_txn().await;
    let machine = mh.host().db_machine(&mut txn).await;

    let request = tonic::Request::new(rpc::forge::CloudInitInstructionsRequest {
        ip: machine.status.interfaces[0].addresses[0].to_string(),
    });

    let response = env.api.get_cloud_init_instructions(request).await?;

    let Some(metadata) = response.into_inner().metadata else {
        panic!("The value for metadata should not have been None");
    };

    assert_eq!(metadata.instance_id, mh.host().id.to_string());

    let (tinstance, instance) = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build_and_return()
        .await;

    let request = tonic::Request::new(rpc::forge::CloudInitInstructionsRequest {
        ip: instance.status().network().interfaces[0].addresses[0].to_string(),
    });

    let response = env.api.get_cloud_init_instructions(request).await?;

    let Some(metadata) = response.into_inner().metadata else {
        panic!("The value for metadata should not have been None");
    };

    assert_eq!(metadata.instance_id, tinstance.id.to_string());

    txn.commit().await.unwrap();
    tinstance.delete().await;

    Ok(())
}

#[crate::sqlx_test]
async fn test_can_not_create_instance_for_dpu(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let vpc_id = db::vpc::find_by_segment(&env.pool, segment_id)
        .await
        .unwrap()
        .unwrap()
        .id;
    let host_config = env.managed_host_config();
    let dpu_machine_id = dpu::create_dpu_machine(&env, &host_config).await;
    let request = crate::instance::InstanceAllocationRequest {
        instance_id: InstanceId::new(),
        machine_id: dpu_machine_id.into(),
        instance_type_id: None,
        config: model::instance::config::InstanceConfig {
            os: default_os_config().try_into().unwrap(),
            tenant: default_tenant_config().try_into().unwrap(),
            network: InstanceNetworkConfig::for_segment_ids(&[segment_id], &[], &[vpc_id]),
            infiniband: InstanceInfinibandConfig::default(),
            nvlink: InstanceNvLinkConfig::default(),
            spxconfig: InstanceSpxConfig::default(),
            network_security_group_id: None,
            extension_services: InstanceExtensionServicesConfig::default(),
            power_profile: None,
        },
        metadata: Metadata {
            name: "test_instance".to_string(),
            description: "tests/instance".to_string(),
            labels: HashMap::new(),
        },
        allow_unhealthy_machine: false,
    };

    // Note: This also requests a background task in the DB for creating managed
    // resources. That's however ok - we will just ignore it and not execute
    // that task. Later we might also verify that the creation of those resources
    // is requested
    let result = allocate_instance(&env.api, request, env.config.host_health).await;
    let error = result.expect_err("expected allocation to fail").to_string();
    assert!(
        error.contains("is of type DPU and can not be converted into an instance"),
        "Error message should contain 'is of type Dpu and can not be converted into an instance', but is {error}"
    );
}

#[crate::sqlx_test]
// This test verifies parity between `addresses` and the compatibility fields.
#[allow(deprecated)]
async fn test_instance_address_creation(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let (segment_id_1, segment_id_2) = env.create_vpc_and_dual_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id_1)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id_2)
            .await
            .unwrap(),
        0
    );
    txn.commit().await.unwrap();

    let network = rpc::InstanceNetworkConfig {
        interfaces: vec![
            rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical as i32,
                network_segment_id: Some(segment_id_1),
                network_details: None,
                device: None,
                device_instance: 0u32,
                virtual_function_id: None,
                ip_address: None,
                ipv6_interface_config: None,
                routing_profile: None,
            },
            rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Virtual as i32,
                network_segment_id: Some(segment_id_2),
                network_details: None,
                device: None,
                device_instance: 0u32,
                virtual_function_id: None,
                ip_address: None,
                ipv6_interface_config: None,
                routing_profile: None,
            },
        ],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };

    mh.instance_builer(&env).network(network).build().await;

    let mut txn = env.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id_1)
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id_2)
            .await
            .unwrap(),
        1
    );

    // TODO(chet): This will be where I also drop prefix allocation testing!

    // Check the allocated IP for the PF/primary interface.
    let allocated_ip_resolver = UsedOverlayNetworkIpResolver {
        segment_id: segment_id_1,
        busy_ips: vec![],
    };
    let used_ips = allocated_ip_resolver.used_ips(txn.as_mut()).await.unwrap();
    let used_prefixes = allocated_ip_resolver
        .used_prefixes(txn.as_mut())
        .await
        .unwrap();
    assert_eq!(1, used_ips.len());
    assert_eq!(1, used_prefixes.len());
    assert_eq!("192.0.4.3", used_ips[0].to_string());
    assert_eq!("192.0.4.3/32", used_prefixes[0].to_string());

    // Check the allocated VF.
    let allocated_ip_resolver = UsedOverlayNetworkIpResolver {
        segment_id: segment_id_2,
        busy_ips: vec![],
    };
    let used_ips = allocated_ip_resolver.used_ips(txn.as_mut()).await.unwrap();
    let used_prefixes = allocated_ip_resolver
        .used_prefixes(txn.as_mut())
        .await
        .unwrap();
    assert_eq!(1, used_ips.len());
    assert_eq!(1, used_prefixes.len());
    assert_eq!("192.1.4.3", used_ips[0].to_string());
    assert_eq!("192.1.4.3/32", used_prefixes[0].to_string());

    txn.commit().await.unwrap();

    // The addresses should show up in the internal config - which is sent to the DPU
    let network_config = env
        .api
        .get_managed_host_network_config(tonic::Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(mh.dpu().id.into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert!(!network_config.use_admin_network);
    assert_eq!(network_config.tenant_interfaces.len(), 2);
    assert_eq!(
        network_config.tenant_interfaces[0].ip.as_deref(),
        Some("192.0.4.3")
    );
    assert_eq!(
        network_config.tenant_interfaces[1].ip.as_deref(),
        Some("192.1.4.3")
    );
    for interface in &network_config.tenant_interfaces {
        assert_eq!(
            interface.addresses,
            vec![rpc::forge::InterfaceAddressConfig {
                address_family: rpc::forge::AddressFamily::V4.into(),
                ip: interface.ip.clone().unwrap(),
                interface_prefix: interface.interface_prefix.clone().unwrap(),
                prefix: interface.prefix.clone().unwrap(),
                gateway: interface.gateway.clone(),
                svi_ip: interface.svi_ip.clone(),
                tenant_vrf_loopback_ip: None,
            }]
        );
    }
    assert_eq!(network_config.dpu_network_pinger_type, None);
    // Ensure the VPC prefixes (which in this case are the two network segment
    // IDs referenced above) are both associated with both interfaces.
    let expected_vpc_prefixes = vec!["192.0.4.0/24".to_string(), "192.1.4.0/24".to_string()];
    assert_eq!(
        network_config.tenant_interfaces[0].vpc_prefixes,
        expected_vpc_prefixes
    );
    assert_eq!(
        network_config.tenant_interfaces[1].vpc_prefixes,
        expected_vpc_prefixes
    );
}

#[crate::sqlx_test]
async fn test_cannot_create_instance_on_unhealthy_dpu(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> eyre::Result<()> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    // Report an unhealthy DPU
    network_configured_with_health(
        &env,
        &dpu_machine_id,
        Some(rpc::health::HealthReport {
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
        }),
    )
    .await;

    let result = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_machine_id)
                .config(
                    InstanceConfig::default_tenant_and_os()
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

#[crate::sqlx_test]
async fn test_create_instance_with_allow_unhealthy_machine_true(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (host_machine_id, dpu_machine_id) = create_managed_host(&env).await.into();

    // Report an unhealthy DPU
    network_configured_with_health(
        &env,
        &dpu_machine_id,
        Some(rpc::health::HealthReport {
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
        }),
    )
    .await;

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    let instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(true)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(
                    InstanceConfig::default_tenant_and_os()
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

    let instance = env.one_instance(instance_id).await;
    assert_eq!(instance.id(), instance_id);
}

#[crate::sqlx_test]
async fn test_instance_phone_home(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let mut os = default_os_config();
    os.phone_home_enabled = true;
    let instance_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(os),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        nvlink: None,
        spxconfig: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        power_profile: None,
    };

    let tinstance = mh
        .instance_builer(&env)
        .config(instance_config)
        .build()
        .await;

    let instance = tinstance.rpc_instance().await;

    // Should be in a provisioning state
    assert_eq!(instance.status().tenant(), rpc::TenantState::Provisioning);

    // Phone home to transition to the ready state
    let mut phone_home_req = tonic::Request::new(rpc::forge::InstancePhoneHomeLastContactRequest {
        instance_id: Some(tinstance.id),
    });
    let mut auth_context = crate::auth::AuthContext::default();
    auth_context
        .principals
        .push(carbide_authn::middleware::Principal::SpiffeMachineIdentifier(mh.id.to_string()));
    phone_home_req.extensions_mut().insert(auth_context);
    env.api
        .update_instance_phone_home_last_contact(phone_home_req)
        .await
        .unwrap();

    let instance = tinstance.rpc_instance().await;

    assert_eq!(instance.status().tenant(), rpc::TenantState::Ready);
}

#[crate::sqlx_test]
async fn test_bootingwithdiscoveryimage_delay(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    env.api
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(tinstance.id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .expect("Delete instance failed.");

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        8,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::HostPlatformConfiguration {
                platform_config_state:
                    model::machine::HostPlatformConfigurationState::CheckHostConfig,
            },
        },
    )
    .await;

    mh.network_configured(&env).await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        2,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::WaitingForDpusToUp,
        },
    )
    .await;

    mh.network_configured(&env).await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::BootingWithDiscoveryImage {
                retry: model::machine::RetryInfo { count: 0 },
            },
        },
    )
    .await;

    assert!(
        env.test_meter
            .formatted_metric("carbide_reboot_attempts_in_booting_with_discovery_image_count")
            .is_none(),
        "State is not changed. The reboot counter should only increased once state changed"
    );
    tokio::time::sleep(Duration::from_secs(2)).await;

    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    txn.commit().await.unwrap();

    update_time_params(&env.pool, &host, 1, None).await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        1,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::BootingWithDiscoveryImage {
                retry: model::machine::RetryInfo { count: 1 },
            },
        },
    )
    .await;

    assert!(
        env.test_meter
            .formatted_metric("carbide_reboot_attempts_in_booting_with_discovery_image_count")
            .is_none(),
        "State is not changed. The reboot counter should only increased once state changed"
    );

    common::api_fixtures::instance::handle_delete_post_bootingwithdiscoveryimage(&env, &mh).await;

    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_reboot_attempts_in_booting_with_discovery_image_sum")
            .unwrap(),
        "2"
    );
    assert_eq!(
        env.test_meter
            .formatted_metric("carbide_reboot_attempts_in_booting_with_discovery_image_count")
            .unwrap(),
        "1"
    );
}

#[crate::sqlx_test]
async fn test_create_instance_duplicate_keyset_ids(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let config = rpc::InstanceConfig {
        os: Some(default_os_config()),
        tenant: Some(rpc::TenantConfig {
            tenant_organization_id: "Tenant1".to_string(),
            tenant_keyset_ids: vec![
                "a".to_string(),
                "bad_id".to_string(),
                "c".to_string(),
                "bad_id".to_string(),
            ],
            hostname: Some("test-instance".to_string()),
        }),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        nvlink: None,
        spxconfig: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        power_profile: None,
    };

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    let err = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect_err("Duplicate TenantKeyset IDs should not be accepted");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(err.message(), "duplicate tenant KeySet ID found: bad_id");
}

#[crate::sqlx_test]
async fn test_create_instance_keyset_ids_max(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let config = rpc::InstanceConfig {
        os: Some(default_os_config()),
        tenant: Some(rpc::TenantConfig {
            tenant_organization_id: "Tenant1".to_string(),
            tenant_keyset_ids: vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "d".to_string(),
                "e".to_string(),
                "f".to_string(),
                "g".to_string(),
                "h".to_string(),
                "i".to_string(),
                "j".to_string(),
                "k".to_string(),
            ],
            hostname: Some("test-hostname".to_string()),
        }),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        nvlink: None,
        spxconfig: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        power_profile: None,
    };

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    let err = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect_err("More than 10 TenantKeyset IDs should not be accepted");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        err.message(),
        "more than 10 tenant KeySet IDs are not allowed"
    );
}

#[crate::sqlx_test]
async fn test_allocate_instance_with_old_network_segemnt(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let vpc_id = db::vpc::find_by_segment(&env.pool, segment_id)
        .await
        .unwrap()
        .unwrap()
        .id;
    let mh = create_managed_host(&env).await;

    let txn = env
        .pool
        .begin()
        .await
        .expect("Unable to create transaction on database pool");
    txn.commit().await.unwrap();

    let instance_metadata = rpc::forge::Metadata {
        name: "test_instance_with_labels".to_string(),
        description: "this instance does not have labels.".to_string(),
        labels: vec![],
    };

    let device_locator = DeviceLocator {
        device: "DPU1".to_string(),
        device_instance: 0,
    };
    let mut nw_config =
        interface_network_config_with_devices(&[segment_id], std::slice::from_ref(&device_locator));
    for interface in &mut nw_config.interfaces {
        interface.network_details = None;
    }

    let tinstance = mh
        .instance_builer(&env)
        .network(nw_config)
        .metadata(instance_metadata.clone())
        .build()
        .await;

    // Test searching based on instance id.
    let mut instance_matched_by_id = tinstance.rpc_instance().await.into_inner();

    instance_matched_by_id.metadata = instance_matched_by_id.metadata.take().map(|mut metadata| {
        metadata.labels.sort_by(|l1, l2| l1.key.cmp(&l2.key));
        metadata
    });

    assert_eq!(
        instance_matched_by_id.metadata,
        Some(instance_metadata.clone())
    );

    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;

    let fetched_instance = snapshot.instance.unwrap();
    assert_eq!(fetched_instance.machine_id, mh.id.into());

    let network_config = fetched_instance.config.network;
    assert_eq!(fetched_instance.network_config_version.version_nr(), 1);
    let mut network_config_no_addresses = network_config;
    for iface in network_config_no_addresses.interfaces.iter_mut() {
        assert_eq!(iface.ip_addrs.len(), 1);
        assert_eq!(iface.interface_prefixes.len(), 1);
        iface.ip_addrs.clear();
        iface.interface_prefixes.clear();
        iface.network_segment_gateways.clear();
        iface.internal_uuid = uuid::Uuid::nil();
    }

    assert_eq!(
        network_config_no_addresses,
        InstanceNetworkConfig::for_segment_ids(&[segment_id], &[device_locator], &[vpc_id])
    );
}

#[crate::sqlx_test]
async fn test_allocate_network_vpc_prefix_id(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    env.create_vpc_and_tenant_segment().await;
    let vpc = db::vpc::find_by_name(&env.pool, "test vpc 1")
        .await
        .unwrap()
        .into_iter()
        .next()
        .unwrap();

    let vpc_prefix_id = create_tenant_overlay_prefix(&env, vpc.id).await;

    let x = rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: 0,
            network_segment_id: None,
            network_details: Some(
                rpc::forge::instance_interface_config::NetworkDetails::VpcPrefixId(vpc_prefix_id),
            ),
            device: None,
            device_instance: 0u32,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };

    let config = rpc::InstanceConfig {
        tenant: Some(rpc::TenantConfig {
            tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
            hostname: Some("xyz".to_string()),
            tenant_keyset_ids: vec![],
        }),
        os: Some(default_os_config()),
        network: Some(x),
        infiniband: None,
        nvlink: None,
        spxconfig: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        power_profile: None,
    };

    let mut config: model::instance::config::InstanceConfig = config.try_into().unwrap();

    assert!(config.network.interfaces[0].network_segment_id.is_none());

    let mut txn = env.db_txn().await;
    let tenant_organization_id = config.tenant.tenant_organization_id.clone();
    allocate_network(&mut config.network, &tenant_organization_id, &mut txn)
        .await
        .unwrap();

    txn.commit().await.unwrap();
    assert!(config.network.interfaces[0].network_segment_id.is_some());

    let mut txn = env.db_txn().await;
    let network_segment = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(
            IdColumn,
            &config.network.interfaces[0].network_segment_id.unwrap(),
        ),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();

    let np = network_segment[0].prefixes[0].prefix;
    match np {
        IpNetwork::V4(ipv4_network) => assert_eq!(
            Ipv4Addr::from_str("10.217.5.224").unwrap(),
            ipv4_network.network()
        ),
        IpNetwork::V6(_) => panic!("Can not be ipv6."),
    }
}

#[crate::sqlx_test]
async fn test_allocate_and_release_instance_vpc_prefix_id(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let expected_tenant_config = default_tenant_config();

    // Create the fixture tenant so the FNN VPC inherits a DPU-renderable routing profile.
    create_fixture_tenant(&env, expected_tenant_config.tenant_organization_id.clone())
        .await
        .unwrap();

    // Create the VPC as FNN up front so the routing profile is persisted with it.
    let segment_id = env
        .create_vpc_and_tenant_segment_with_vpc_details(
            VpcCreationRequest::builder(expected_tenant_config.tenant_organization_id.clone())
                .metadata(Metadata {
                    name: "test vpc 1".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .rpc(),
        )
        .await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id)
            .await
            .unwrap(),
        0
    );
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Ready
    ));
    txn.commit().await.unwrap();

    let mut vpc = db::vpc::find_by_name(&env.pool, "test vpc 1")
        .await
        .unwrap();
    let vpc = vpc.remove(0);

    let vpc_prefix_id = create_tenant_overlay_prefix(&env, vpc.id).await;
    let vpc_prefix = env
        .api
        .get_vpc_prefixes(tonic::Request::new(rpc::forge::VpcPrefixGetRequest {
            vpc_prefix_ids: vec![vpc_prefix_id],
            deleted: rpc::forge::DeletedFilter::Exclude as i32,
        }))
        .await
        .unwrap()
        .into_inner()
        .vpc_prefixes[0]
        .clone();

    assert_eq!(vpc_prefix.total_31_segments, 16);
    assert_eq!(vpc_prefix.available_31_segments, 16);

    let tinstance = mh
        .instance_builer(&env)
        .network(single_interface_network_config_with_vpc_prefix(
            vpc_prefix_id,
        ))
        .build()
        .await;

    let vpc_prefix = env
        .api
        .get_vpc_prefixes(tonic::Request::new(rpc::forge::VpcPrefixGetRequest {
            vpc_prefix_ids: vec![vpc_prefix_id],
            deleted: rpc::forge::DeletedFilter::Exclude as i32,
        }))
        .await
        .unwrap()
        .into_inner()
        .vpc_prefixes[0]
        .clone();

    assert_eq!(vpc_prefix.total_31_segments, 16);
    assert_eq!(vpc_prefix.available_31_segments, 15);

    let instance = tinstance.rpc_instance().await;

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    let tenant_config = instance.config().tenant();
    let expected_os = default_os_config();
    let os = instance.config().os();
    assert_eq!(os, &expected_os);

    assert_eq!(tenant_config, &expected_tenant_config);

    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;

    let fetched_instance = snapshot.instance.unwrap();
    assert_eq!(fetched_instance.machine_id, mh.id.into());
    assert_eq!(
        db::instance_address::count_by_segment_id(
            &mut txn,
            &fetched_instance.config.network.interfaces[0]
                .network_segment_id
                .unwrap()
        )
        .await
        .unwrap(),
        1
    );

    let ns_id = fetched_instance.config.network.interfaces[0]
        .network_segment_id
        .unwrap();

    let ns = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db::network_segment::IdColumn, &ns_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    let ns = ns.first().unwrap();

    assert!(ns.status.vlan_id.is_none());
    assert!(ns.status.vni.is_none());

    let network_config = fetched_instance.config.network.clone();
    assert_eq!(fetched_instance.network_config_version.version_nr(), 1);
    let mut network_config_no_addresses = network_config.clone();
    for iface in network_config_no_addresses.interfaces.iter_mut() {
        assert_eq!(iface.ip_addrs.len(), 1);
        assert_eq!(iface.interface_prefixes.len(), 1);
        iface.ip_addrs.clear();
        iface.interface_prefixes.clear();
        iface.network_segment_gateways.clear();
        iface.network_segment_id = None;
        iface.internal_uuid = uuid::Uuid::nil();
    }
    assert_eq!(
        network_config_no_addresses,
        InstanceNetworkConfig::for_vpc_prefix_id(vpc_prefix_id, Some(vpc.id))
    );

    assert!(!fetched_instance.observations.network.is_empty());
    assert!(fetched_instance.use_custom_pxe_on_boot);

    let _ = db::instance::use_custom_ipxe_on_next_boot(&mh.id, false, &mut txn).await;
    let snapshot = mh.snapshot(&mut txn).await;

    let fetched_instance = snapshot.instance.unwrap();

    assert!(!fetched_instance.use_custom_pxe_on_boot);
    txn.commit().await.unwrap();

    let mut txn = env.db_txn().await;
    let mut ns = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(
            IdColumn,
            &fetched_instance.config.network.interfaces[0]
                .network_segment_id
                .unwrap(),
        ),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();

    let ns = ns.remove(0);

    let records = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut txn,
        &fetched_instance.id,
        &ns.id,
    )
    .await
    .unwrap();
    let [record] = records.as_slice() else {
        panic!("expected one address on the allocated segment")
    };

    // This should the first IP. Algo does not look into machine_interface_addresses
    // table for used addresses for instance.
    assert_eq!(record.address.to_string(), "10.217.5.225");
    assert_eq!(
        &record.address,
        network_config.interfaces[0]
            .ip_addrs
            .iter()
            .next()
            .unwrap()
            .1
    );

    assert_eq!(
        format!("{}/32", record.address),
        network_config.interfaces[0]
            .interface_prefixes
            .iter()
            .next()
            .unwrap()
            .1
            .to_string()
    );

    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    ));
    txn.commit().await.unwrap();

    tinstance.delete().await;

    let segment_ids = fetched_instance
        .config
        .network
        .interfaces
        .iter()
        .filter_map(|x| match x.network_details {
            Some(NetworkDetails::VpcPrefixId(_)) => x.network_segment_id,
            _ => None,
        })
        .collect_vec();

    // Address is freed during delete
    let mut txn = env.db_txn().await;
    let network_segments = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::List(IdColumn, &segment_ids),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();

    assert!(network_segments.is_empty());

    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Ready
    ));
    assert_eq!(
        db::instance_address::count_by_segment_id(
            &mut txn,
            &fetched_instance.config.network.interfaces[0]
                .network_segment_id
                .unwrap()
        )
        .await
        .unwrap(),
        0
    );
    let vpc_prefix = env
        .api
        .get_vpc_prefixes(tonic::Request::new(rpc::forge::VpcPrefixGetRequest {
            vpc_prefix_ids: vec![vpc_prefix_id],
            deleted: rpc::forge::DeletedFilter::Exclude as i32,
        }))
        .await
        .unwrap()
        .into_inner()
        .vpc_prefixes[0]
        .clone();

    assert_eq!(vpc_prefix.total_31_segments, 16);
    assert_eq!(vpc_prefix.available_31_segments, 16);
    txn.commit().await.unwrap();
}

/// Selects or validates a linknet before creating its generated segment,
/// mirroring production allocation order in focused allocator tests.
async fn allocate_test_network_segment(
    allocator: &PrefixAllocator,
    txn: &mut sqlx::PgConnection,
    vpc_id: VpcId,
    requested_prefix: Option<IpNetwork>,
) -> crate::CarbideResult<(NetworkSegmentId, IpNetwork)> {
    // Validate an explicit request against the parent or select its next free linknet.
    let prefix = if let Some(requested_prefix) = requested_prefix {
        allocator
            .validate_desired_prefix(&mut *txn, requested_prefix)
            .await?;
        requested_prefix
    } else {
        allocator.next_free_prefix(&mut *txn).await?
    };

    // Create the generated segment only after selection or validation succeeds.
    allocator
        .allocate_network_segment_for_prefix(&mut *txn, vpc_id, prefix)
        .await
}

/// Verifies automatic selection remains deterministic and tenant-scoped across
/// every supported address-family mode.
#[crate::sqlx_test]
async fn test_auto_vpc_prefix_selection_uses_static_first_fit(pool: PgPool) {
    let fixture = create_auto_vpc_selection_fixture(pool).await;

    // Consume the original IPv4 capacity while checking first-fit, ownership,
    // persisted RPC projection, and final exhaustion.
    assert_static_ipv4_first_fit(&fixture).await;
    assert_explicit_prefix_tenant_ownership(&fixture).await;
    allocate_and_assert_auto_vpc_instance(&fixture).await;
    assert_ipv4_candidates_exhausted(&fixture).await;

    // Add fresh family capacity and exercise IPv6-only and dual-stack through
    // the public allocation boundary.
    let dual_stack_prefixes = add_dual_stack_prefix_capacity(&fixture).await;
    assert_ipv6_only_resolution(&fixture, dual_stack_prefixes.ipv6_prefix_id).await;
    assert_dual_stack_resolution(&fixture, &dual_stack_prefixes).await;
}

/// SLAAC retains the selected IPv6 linknet without materializing a host address.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_slaac_vpc_allocation_preserves_prefix_without_ipv6_address(pool: PgPool) {
    let fixture = create_auto_vpc_selection_fixture_with_slaac(pool, true).await;
    let ineligible_ipv6_prefix =
        IpNetwork::V6(Ipv6Network::new("fd42:2403:0:2::".parse().unwrap(), 126).unwrap());
    let ineligible_ipv6_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &fixture.env,
        fixture.vpc_id,
        "ineligible SLAAC IPv6 candidate",
        ineligible_ipv6_prefix,
    )
    .await;
    let ipv6_parent_prefix =
        IpNetwork::V6(Ipv6Network::new("fd42:2403::".parse().unwrap(), 63).unwrap());
    let ipv6_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &fixture.env,
        fixture.vpc_id,
        "SLAAC IPv6 candidate",
        ipv6_parent_prefix,
    )
    .await;

    let historical_prefix = "fd42:2403:0:2::/127".parse::<IpNetwork>().unwrap();
    let historical_allocator =
        PrefixAllocator::new(ineligible_ipv6_prefix_id, ineligible_ipv6_prefix, None, 127).unwrap();
    let mut txn = fixture.env.db_txn().await;
    let (historical_segment_id, allocated_prefix) = allocate_test_network_segment(
        &historical_allocator,
        txn.as_mut(),
        fixture.vpc_id,
        Some(historical_prefix),
    )
    .await
    .unwrap();
    assert_eq!(allocated_prefix, historical_prefix);
    let mut persisted_prefixes = db::network_prefix::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db::network_prefix::SegmentIdColumn, &historical_segment_id),
    )
    .await
    .unwrap();
    assert_eq!(persisted_prefixes.len(), 1);
    let persisted_prefix = persisted_prefixes.pop().unwrap();
    txn.commit().await.unwrap();

    // Keep an existing interface prefix as is. Check whether the VPC prefix can
    // contain a /64 only when allocating a new interface prefix.
    let mut historical_config =
        InstanceNetworkConfig::for_vpc_prefix_id(ineligible_ipv6_prefix_id, Some(fixture.vpc_id));
    historical_config.interfaces[0].network_segment_id = Some(historical_segment_id);
    historical_config.interfaces[0]
        .interface_prefixes
        .insert(persisted_prefix.id, historical_prefix);
    let expected_historical_config = historical_config.clone();
    let mut txn = fixture.env.db_txn().await;
    allocate_network(
        &mut historical_config,
        &fixture.tenant_organization_id,
        txn.as_mut(),
    )
    .await
    .unwrap();
    assert_eq!(historical_config, expected_historical_config);
    let replayed_prefixes = db::network_prefix::find_allocation_occupancy(
        txn.as_mut(),
        ineligible_ipv6_prefix_id,
        ineligible_ipv6_prefix,
    )
    .await
    .unwrap();
    assert_eq!(replayed_prefixes.len(), 1);
    let replayed_prefix = &replayed_prefixes[0];
    assert_eq!(replayed_prefix.id, persisted_prefix.id);
    assert_eq!(replayed_prefix.segment_id, historical_segment_id);
    assert_eq!(replayed_prefix.prefix, historical_prefix);
    assert_eq!(
        replayed_prefix.vpc_prefix_id,
        Some(ineligible_ipv6_prefix_id)
    );
    assert_eq!(replayed_prefix.vpc_prefix, Some(ineligible_ipv6_prefix));
    txn.rollback().await.unwrap();

    // Explicit address checks must distinguish the SLAAC policy from a prefix
    // family mismatch. Keep these cases in one table so primary and dual-stack
    // sidecar validation cannot drift.
    let mut ipv6_only =
        InstanceNetworkConfig::for_vpc_prefix_id(ipv6_prefix_id, Some(fixture.vpc_id));
    ipv6_only.interfaces[0].requested_ip_addr = Some("fd42:2403::1".parse().unwrap());
    let mut dual_stack = InstanceNetworkConfig::for_vpc_prefix_id(
        fixture.lower_ipv4_prefix_id,
        Some(fixture.vpc_id),
    );
    dual_stack.interfaces[0].ipv6_interface_config = Some(Ipv6InterfaceConfig {
        vpc_prefix_id: ipv6_prefix_id,
        requested_ip_addr: Some("fd42:2403::3".parse().unwrap()),
    });
    let mut mismatched_primary =
        InstanceNetworkConfig::for_vpc_prefix_id(ipv6_prefix_id, Some(fixture.vpc_id));
    mismatched_primary.interfaces[0].requested_ip_addr = Some("192.0.2.1".parse().unwrap());
    let ineligible_parent =
        InstanceNetworkConfig::for_vpc_prefix_id(ineligible_ipv6_prefix_id, Some(fixture.vpc_id));
    for (scenario, mut config, expected_error_fragments) in [
        (
            "IPv6-only primary request",
            ipv6_only,
            ["requested IPv6 address", "has SLAAC enabled"],
        ),
        (
            "dual-stack IPv6 sidecar request",
            dual_stack,
            ["requested IPv6 address", "has SLAAC enabled"],
        ),
        (
            "IPv4 address with an IPv6 primary prefix",
            mismatched_primary,
            ["requested IP address", "does not match VPC prefix"],
        ),
        (
            "SLAAC parent narrower than one interface prefix",
            ineligible_parent,
            [
                "fd42:2403:0:2::/126",
                "cannot contain a /64 interface prefix",
            ],
        ),
    ] {
        let mut txn = fixture.env.db_txn().await;
        let error = allocate_network(&mut config, &fixture.tenant_organization_id, &mut txn)
            .await
            .expect_err(scenario);
        assert!(
            matches!(
                &error,
                crate::CarbideError::InvalidArgument(message)
                    if expected_error_fragments
                        .iter()
                        .all(|fragment| message.contains(fragment))
            ),
            "unexpected {scenario} error: {error}",
        );
        txn.rollback().await.unwrap();
    }

    // An explicit VPC-prefix selection uses the same SLAAC carve policy as
    // automatic selection. Roll it back so the lifecycle below can still
    // exercise deterministic first-fit allocation from the same parent.
    let mut explicit_config =
        InstanceNetworkConfig::for_vpc_prefix_id(ipv6_prefix_id, Some(fixture.vpc_id));
    let mut txn = fixture.env.db_txn().await;
    allocate_network(
        &mut explicit_config,
        &fixture.tenant_organization_id,
        txn.as_mut(),
    )
    .await
    .unwrap();
    let explicit_interface = &explicit_config.interfaces[0];
    assert!(explicit_interface.ip_addrs.is_empty());
    assert!(explicit_interface.interface_prefixes.is_empty());
    let explicit_segment_id = explicit_interface.network_segment_id.unwrap();
    let explicit_prefixes = db::network_prefix::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db::network_prefix::SegmentIdColumn, &explicit_segment_id),
    )
    .await
    .unwrap();
    assert_eq!(explicit_prefixes.len(), 1);
    assert_eq!(
        explicit_prefixes[0].prefix,
        "fd42:2403::/64".parse::<IpNetwork>().unwrap(),
    );
    assert_eq!(explicit_prefixes[0].vpc_prefix_id, Some(ipv6_prefix_id));
    txn.rollback().await.unwrap();

    let (managed_host, instance) = allocate_auto_vpc_instance(
        &fixture,
        rpc::forge::InstanceInterfaceIpFamilyMode::Ipv6Only,
        "automatic-SLAAC-ipv6-selection",
    )
    .await;
    let segment_id = assert_auto_rpc_resolution(
        &instance,
        fixture.vpc_id,
        rpc::forge::InstanceInterfaceIpFamilyMode::Ipv6Only,
        None,
        Some(ipv6_prefix_id),
    );
    let status_interface = &instance
        .status
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces[0];
    assert!(status_interface.addresses.is_empty());
    assert_eq!(status_interface.prefixes, ["fd42:2403::/64"]);

    // Re-read through the public API: the selected parent prefix remains
    // visible even though SLAAC deliberately has no concrete host address.
    let instance_id = instance.id.unwrap();
    let persisted_rpc = fixture.env.one_instance(instance_id).await;
    assert_auto_rpc_resolution(
        persisted_rpc.inner(),
        fixture.vpc_id,
        rpc::forge::InstanceInterfaceIpFamilyMode::Ipv6Only,
        None,
        Some(ipv6_prefix_id),
    );
    let persisted_status_interface = &persisted_rpc.status().network().interfaces[0];
    assert!(persisted_status_interface.addresses.is_empty());
    assert_eq!(persisted_status_interface.prefixes, ["fd42:2403::/64"]);

    let vpc_instance_ids = fixture
        .env
        .api
        .find_instance_ids(Request::new(rpc::forge::InstanceSearchFilter {
            label: None,
            tenant_org_id: None,
            vpc_id: Some(fixture.vpc_id.to_string()),
            instance_type_id: None,
        }))
        .await
        .unwrap()
        .into_inner()
        .instance_ids;
    assert!(
        vpc_instance_ids.contains(&instance_id),
        "searching by VPC must retain an addressless SLAAC instance",
    );

    let mut txn = fixture.env.db_txn().await;
    let snapshot = db::instance::find_by_id(txn.as_mut(), instance_id)
        .await
        .unwrap()
        .expect("persisted SLAAC instance");
    let interface = &snapshot.config.network.interfaces[0];
    assert!(interface.ip_addrs.is_empty());

    let segments = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(IdColumn, &segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    let [segment] = segments.as_slice() else {
        panic!("expected one generated SLAAC segment");
    };
    let [network_prefix] = segment.prefixes.as_slice() else {
        panic!("expected one generated SLAAC IPv6 prefix");
    };
    assert_eq!(
        network_prefix.prefix,
        "fd42:2403::/64".parse::<IpNetwork>().unwrap(),
    );
    assert_eq!(
        interface.interface_prefixes.get(&network_prefix.id),
        Some(&network_prefix.prefix),
    );
    assert!(
        db::instance_address::find_by_segment_id(txn.as_mut(), &segment_id)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        db::instance::count_network_segment_references(txn.as_mut(), &segment_id)
            .await
            .unwrap(),
        1,
    );
    assert_eq!(
        db::instance::count_vpc_references(txn.as_mut(), &fixture.vpc_id)
            .await
            .unwrap(),
        1,
    );
    assert!(
        db::instance_address::segment_has_allocations(txn.as_mut(), &segment_id)
            .await
            .unwrap()
    );
    let segment_delete_error = db::network_segment::mark_as_deleted(segment, txn.as_mut())
        .await
        .expect_err("SLAAC config must keep its segment with no host address in use");
    assert!(matches!(
        segment_delete_error,
        db::DatabaseError::NetworkSegmentDelete(message)
            if message.contains("referencing instance count: 1")
    ));
    txn.commit().await.unwrap();

    // New agents prefer the address list shared by both address families,
    // while older agents read the nested IPv6 config. Both receive the prefix
    // and an intentionally empty host IP for SLAAC.
    let managed_config = fixture
        .env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu().id.into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let [tenant_interface] = managed_config.tenant_interfaces.as_slice() else {
        panic!("expected one SLAAC tenant interface");
    };
    let expected_fqdn_label = instance_id.to_string();
    assert_eq!(
        tenant_interface
            .fqdn
            .split_once('.')
            .map(|(label, _)| label),
        Some(expected_fqdn_label.as_str()),
    );
    let ipv6 = tenant_interface
        .ipv6_interface_config
        .as_ref()
        .expect("SLAAC IPv6 interface config");
    assert!(ipv6.ip.is_empty());
    assert_eq!(ipv6.interface_prefix, network_prefix.prefix.to_string());
    let [address] = tenant_interface.addresses.as_slice() else {
        panic!("expected one SLAAC IPv6 family entry");
    };
    assert_eq!(address.address_family(), rpc::forge::AddressFamily::V6);
    assert!(address.ip.is_empty());
    assert_eq!(address.interface_prefix, network_prefix.prefix.to_string());

    // A retry sees the retained interface prefix as the durable allocation
    // result and does not consume another linknet.
    let mut retried_network = snapshot.config.network.clone();
    let expected_network = retried_network.clone();
    let mut txn = fixture.env.db_txn().await;
    allocate_network(
        &mut retried_network,
        &fixture.tenant_organization_id,
        txn.as_mut(),
    )
    .await
    .unwrap();
    assert_eq!(retried_network, expected_network);
    txn.rollback().await.unwrap();

    // During a network update, both the old and new configurations can still
    // own resources. Allocations that retain only a prefix in either half must
    // retain their segment and VPC even though they have no normalized row in
    // `instance_addresses`.
    let mut pending_old = snapshot.config.network.clone();
    let pending_old_segment_id = NetworkSegmentId::new();
    let pending_old_vpc_id = VpcId::new();
    pending_old.interfaces[0].network_segment_id = Some(pending_old_segment_id);
    pending_old.interfaces[0].vpc_id = Some(pending_old_vpc_id);
    let mut pending_new = snapshot.config.network.clone();
    let pending_new_segment_id = NetworkSegmentId::new();
    let pending_new_vpc_id = VpcId::new();
    pending_new.interfaces[0].network_segment_id = Some(pending_new_segment_id);
    pending_new.interfaces[0].vpc_id = Some(pending_new_vpc_id);

    let mut txn = fixture.env.db_txn().await;
    db::instance::trigger_update_network_config_request(
        &instance_id,
        &pending_old,
        &pending_new,
        &mut txn,
    )
    .await
    .unwrap();
    for (scenario, pending_segment_id, pending_vpc_id) in [
        (
            "pending old config",
            pending_old_segment_id,
            pending_old_vpc_id,
        ),
        (
            "pending new config",
            pending_new_segment_id,
            pending_new_vpc_id,
        ),
    ] {
        assert_eq!(
            db::instance::count_network_segment_references(txn.as_mut(), &pending_segment_id,)
                .await
                .unwrap(),
            1,
            "{scenario} segment reference",
        );
        assert!(
            db::instance_address::segment_has_allocations(txn.as_mut(), &pending_segment_id)
                .await
                .unwrap(),
            "{scenario} segment must remain in use",
        );
        assert_eq!(
            db::instance::count_vpc_references(txn.as_mut(), &pending_vpc_id)
                .await
                .unwrap(),
            1,
            "{scenario} VPC reference",
        );
    }

    // This transaction will be rolled back. Remove the prefix rows so that
    // only the reference from the instance config guards VPC deletion in this
    // test.
    db::network_prefix::delete_for_segment(segment_id, txn.as_mut())
        .await
        .unwrap();
    db::network_prefix::delete_for_segment(historical_segment_id, txn.as_mut())
        .await
        .unwrap();
    for vpc_prefix_id in [
        fixture.lower_ipv4_prefix_id,
        fixture.higher_ipv4_prefix_id,
        ineligible_ipv6_prefix_id,
        ipv6_prefix_id,
    ] {
        db::vpc_prefix::final_delete(vpc_prefix_id, txn.as_mut())
            .await
            .unwrap();
    }
    let locked_vpcs = db::vpc::find_by_with_lock(
        txn.as_mut(),
        ObjectColumnFilter::One(db::vpc::IdColumn, &fixture.vpc_id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await
    .unwrap();
    assert_eq!(locked_vpcs.len(), 1);
    let vpc_delete_error = db::vpc::try_delete(txn.as_mut(), fixture.vpc_id)
        .await
        .expect_err("SLAAC config must keep its VPC in use");
    assert!(matches!(
        vpc_delete_error,
        db::DatabaseError::FailedPrecondition(message)
            if message.contains("instance network configurations still reference it")
    ));
    txn.rollback().await.unwrap();

    // The remaining IPv6 linknet can still back a dual-stack interface. Only
    // IPv4 is materialized; IPv6 retains its selected prefix for SLAAC.
    let (dual_stack_host, dual_stack_instance) = allocate_auto_vpc_instance(
        &fixture,
        rpc::forge::InstanceInterfaceIpFamilyMode::DualStack,
        "automatic-SLAAC-dual-stack-selection",
    )
    .await;
    let dual_stack_segment_id = assert_auto_rpc_resolution(
        &dual_stack_instance,
        fixture.vpc_id,
        rpc::forge::InstanceInterfaceIpFamilyMode::DualStack,
        Some(fixture.lower_ipv4_prefix_id),
        Some(ipv6_prefix_id),
    );
    let dual_status_interface = &dual_stack_instance
        .status
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces[0];
    assert_eq!(dual_status_interface.addresses.len(), 1);
    assert!(
        dual_status_interface.addresses[0]
            .parse::<IpAddr>()
            .unwrap()
            .is_ipv4()
    );
    assert_eq!(dual_status_interface.prefixes.len(), 2);
    assert_eq!(
        dual_status_interface.prefixes[0],
        format!("{}/32", dual_status_interface.addresses[0])
    );
    assert_eq!(dual_status_interface.prefixes[1], "fd42:2403:0:1::/64");

    let dual_stack_instance_id = dual_stack_instance.id.unwrap();
    let mut txn = fixture.env.db_txn().await;
    let dual_snapshot = db::instance::find_by_id(txn.as_mut(), dual_stack_instance_id)
        .await
        .unwrap()
        .expect("persisted dual-stack SLAAC instance");
    let dual_interface = &dual_snapshot.config.network.interfaces[0];
    assert_eq!(dual_interface.ip_addrs.len(), 1);
    assert!(
        dual_interface
            .ip_addrs
            .values()
            .all(|address| address.is_ipv4())
    );
    assert_eq!(dual_interface.interface_prefixes.len(), 2);
    let expected_dual_ipv6_prefix = "fd42:2403:0:1::/64".parse::<IpNetwork>().unwrap();
    assert_eq!(
        dual_interface
            .interface_prefixes
            .values()
            .find(|prefix| prefix.is_ipv6()),
        Some(&expected_dual_ipv6_prefix),
    );
    let dual_addresses =
        db::instance_address::find_by_segment_id(txn.as_mut(), &dual_stack_segment_id)
            .await
            .unwrap();
    assert_eq!(dual_addresses.len(), 1);
    assert!(dual_addresses[0].address.is_ipv4());
    txn.commit().await.unwrap();

    let dual_managed_config = fixture
        .env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(dual_stack_host.dpu().id.into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let [dual_tenant_interface] = dual_managed_config.tenant_interfaces.as_slice() else {
        panic!("expected one dual-stack SLAAC tenant interface");
    };
    let dual_ipv6 = dual_tenant_interface
        .addresses
        .iter()
        .find(|address| address.address_family() == rpc::forge::AddressFamily::V6)
        .expect("dual-stack SLAAC IPv6 family entry");
    assert!(dual_ipv6.ip.is_empty());
    assert!(!dual_ipv6.interface_prefix.is_empty());
    let dual_ipv4 = dual_tenant_interface
        .addresses
        .iter()
        .find(|address| address.address_family() == rpc::forge::AddressFamily::V4)
        .expect("dual-stack IPv4 family entry");
    assert!(!dual_ipv4.ip.is_empty());

    // The /63 provides exactly two /64 allocations. The ineligible /126 is
    // ignored rather than surfacing an allocator validation error.
    let mut exhausted_config =
        automatic_network_config(fixture.vpc_id, InstanceInterfaceIpFamilyMode::Ipv6Only);
    let mut txn = fixture.env.db_txn().await;
    let error = allocate_network(
        &mut exhausted_config,
        &fixture.tenant_organization_id,
        txn.as_mut(),
    )
    .await
    .unwrap_err();
    assert!(matches!(
        error,
        crate::CarbideError::ResourceExhausted(message)
            if message
                == format!(
                    "no eligible IPv6 VPC prefix in VPC `{}` could allocate an interface linknet",
                    fixture.vpc_id,
                )
    ));
    txn.rollback().await.unwrap();
}

struct ManualSlaacSegmentFixture {
    env: TestEnv,
    vpc_id: VpcId,
    segment_id: NetworkSegmentId,
}

/// Creates a tenant-owned FNN VPC with SLAAC enabled and one manually managed
/// segment. Keeping the segment independent of a `VpcPrefix` row lets the
/// lifecycle tests isolate VPC and segment deletion from the VPC prefix
/// controller.
async fn create_manual_slaac_segment_fixture(
    pool: PgPool,
    segment_name: &str,
    prefix: IpNetwork,
) -> ManualSlaacSegmentFixture {
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID)
        .await
        .unwrap();

    let vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: format!("{segment_name}-vpc"),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .slaac_enabled(true)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();

    let segment_id = NetworkSegmentId::new();
    let mut txn = env.db_txn().await;
    db::network_segment::persist(
        NewNetworkSegment {
            id: segment_id,
            name: segment_name.to_string(),
            subdomain_id: None,
            vpc_id: Some(vpc_id),
            mtu: 9000,
            prefixes: vec![NewNetworkPrefix {
                prefix,
                gateway: None,
                dhcpv6_link_address: None,
                num_reserved: 0,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Tenant,
            can_stretch: Some(false),
            allocation_strategy: Default::default(),
            infer_slaac_eui64_addresses: false,
        },
        txn.as_mut(),
        NetworkSegmentControllerState::Ready,
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    ManualSlaacSegmentFixture {
        env,
        vpc_id,
        segment_id,
    }
}

/// Seeds the parent row needed by address allocation and returns its machine
/// snapshot. The empty network config makes later lifecycle ownership writes
/// explicit in each test.
async fn seed_empty_instance_for_network_allocation(
    env: &TestEnv,
) -> (InstanceId, Machine, ConfigVersion) {
    let managed_host = create_managed_host(env).await;
    let instance_id = InstanceId::new();
    let network_config_version = ConfigVersion::initial();
    let mut txn = env.db_txn().await;
    let machine = managed_host.host().db_machine(&mut txn).await;
    sqlx::query(
        "INSERT INTO instances (id, machine_id, network_config, network_config_version, \
                                nvlink_config) \
         VALUES ($1, $2, '{\"interfaces\": []}'::jsonb, $3, \
                 '{\"gpu_configs\": []}'::jsonb)",
    )
    .bind(instance_id)
    .bind(managed_host.id)
    .bind(network_config_version)
    .execute(txn.as_mut())
    .await
    .unwrap();
    txn.commit().await.unwrap();
    (instance_id, machine, network_config_version)
}

/// A segment cannot keep a soft-deleted VPC usable or silently lose its SLAAC policy.
#[crate::sqlx_test]
async fn test_deleted_slaac_vpc_rejects_manual_segment_address_allocation(pool: PgPool) {
    let fixture = create_manual_slaac_segment_fixture(
        pool,
        "deletion-first-SLAAC-segment",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 240, 3, 0), 30).unwrap()),
    )
    .await;
    let (instance_id, machine, _) = seed_empty_instance_for_network_allocation(&fixture.env).await;

    // Let deletion acquire its read lock on instance_addresses and retain it
    // until the competing allocator is proven to be waiting on that transaction.
    let mut deletion_txn = fixture.env.db_txn().await;
    let deletion_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(deletion_txn.as_mut())
        .await
        .unwrap();
    let locked_vpcs = db::vpc::find_by_with_lock(
        deletion_txn.as_mut(),
        ObjectColumnFilter::One(db::vpc::IdColumn, &fixture.vpc_id),
        db::vpc::VpcRowLock::Mutation,
    )
    .await
    .unwrap();
    assert_eq!(locked_vpcs.len(), 1);
    assert!(
        db::vpc::try_delete(deletion_txn.as_mut(), fixture.vpc_id)
            .await
            .unwrap()
            .is_some()
    );

    let allocation_pool = fixture.env.pool.clone();
    let network_config =
        InstanceNetworkConfig::for_segment_ids(&[fixture.segment_id], &[], &[fixture.vpc_id]);
    let allocation_task = tokio::spawn(async move {
        let mut txn = allocation_pool.begin().await.unwrap();
        let result =
            db::instance_address::allocate(txn.as_mut(), instance_id, network_config, &machine)
                .await;
        txn.rollback().await.unwrap();
        result
    });
    wait_until_query_blocked_by(
        &fixture.env.pool,
        deletion_pid,
        "LOCK TABLE instance_addresses IN ACCESS EXCLUSIVE MODE",
    )
    .await;
    deletion_txn.commit().await.unwrap();

    let allocation_error = allocation_task
        .await
        .unwrap()
        .expect_err("allocator must observe the deletion that won serialization");
    assert!(matches!(
        allocation_error,
        db::DatabaseError::FailedPrecondition(message)
            if message.contains("only 0 active VPCs were found")
    ));

    let mut txn = fixture.env.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(txn.as_mut(), &fixture.segment_id)
            .await
            .unwrap(),
        0,
        "the losing allocation must not materialize an address",
    );
    txn.rollback().await.unwrap();
}

/// If allocation wins serialization, final deletion must wait for its commit
/// and then retain a segment referenced only by SLAAC prefix configuration.
#[crate::sqlx_test]
async fn test_slaac_allocation_blocks_and_defeats_segment_final_delete(pool: PgPool) {
    let fixture = create_manual_slaac_segment_fixture(
        pool,
        "allocation-first-SLAAC-segment",
        IpNetwork::V6(Ipv6Network::new("fd42:2403:feed::".parse().unwrap(), 64).unwrap()),
    )
    .await;
    let (instance_id, machine, network_config_version) =
        seed_empty_instance_for_network_allocation(&fixture.env).await;

    let mut allocation_txn = fixture.env.db_txn().await;
    let allocation_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(allocation_txn.as_mut())
        .await
        .unwrap();
    let network_config =
        InstanceNetworkConfig::for_segment_ids(&[fixture.segment_id], &[], &[fixture.vpc_id]);
    let allocated = db::instance_address::allocate(
        allocation_txn.as_mut(),
        instance_id,
        network_config,
        &machine,
    )
    .await
    .unwrap();
    assert!(allocated.interfaces[0].ip_addrs.is_empty());
    assert_eq!(allocated.interfaces[0].interface_prefixes.len(), 1);
    db::instance::update_network_config(
        allocation_txn.as_mut(),
        instance_id,
        network_config_version,
        &allocated,
        false,
    )
    .await
    .unwrap();

    let deletion_pool = fixture.env.pool.clone();
    let segment_id = fixture.segment_id;
    let deletion_task = tokio::spawn(async move {
        let mut txn = deletion_pool.begin().await.unwrap();
        let result = db::network_segment::final_delete(segment_id, &mut txn).await;
        txn.rollback().await.unwrap();
        result
    });
    wait_until_query_blocked_by(
        &fixture.env.pool,
        allocation_pid,
        "FROM machine_interfaces WHERE segment_id = $1",
    )
    .await;
    allocation_txn.commit().await.unwrap();

    let deletion_error = deletion_task
        .await
        .unwrap()
        .expect_err("an instance config that retains only a prefix must retain the segment");
    assert!(matches!(
        deletion_error,
        db::DatabaseError::NetworkSegmentDelete(message)
            if message.contains("allocations still reference it")
    ));

    let mut txn = fixture.env.db_txn().await;
    assert!(
        !db::network_segment::find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(IdColumn, &fixture.segment_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await
        .unwrap()
        .is_empty(),
        "failed final deletion must retain the segment",
    );
    assert!(
        db::instance_address::find_by_segment_id(txn.as_mut(), &fixture.segment_id)
            .await
            .unwrap()
            .is_empty(),
        "SLAAC allocation must retain only its prefix",
    );
    assert_eq!(
        db::instance::count_network_segment_references(txn.as_mut(), &fixture.segment_id)
            .await
            .unwrap(),
        1,
    );
    txn.rollback().await.unwrap();
}

/// Verifies a rare overlap from outside parent-prefix serialization retries
/// instead of surfacing exclusion-constraint contention as allocation failure.
#[crate::sqlx_test]
async fn test_auto_vpc_prefix_selection_retries_concurrent_network_prefix_insert(pool: PgPool) {
    let fixture = create_auto_vpc_selection_fixture(pool).await;
    let conflicting_prefix = IpNetwork::new(fixture.lower_ipv4_prefix.network(), 31).unwrap();

    // Keep a conflicting child prefix uncommitted so allocation cannot observe
    // it before choosing the same linknet and waiting on the exclusion constraint.
    let mut blocker = fixture.env.pool.begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await
        .unwrap();
    db::network_segment::persist(
        NewNetworkSegment {
            id: NetworkSegmentId::new(),
            name: "overlap-race-blocker".to_string(),
            subdomain_id: None,
            vpc_id: Some(fixture.vpc_id),
            mtu: 9000,
            prefixes: vec![NewNetworkPrefix {
                prefix: conflicting_prefix,
                gateway: Some(conflicting_prefix.network()),
                dhcpv6_link_address: None,
                num_reserved: 0,
            }],
            vlan_id: None,
            vni: None,
            segment_type: NetworkSegmentType::Tenant,
            can_stretch: Some(false),
            allocation_strategy: Default::default(),
            infer_slaac_eui64_addresses: false,
        },
        blocker.as_mut(),
        NetworkSegmentControllerState::Ready,
    )
    .await
    .unwrap();

    // Start allocation on another connection and prove its prefix insert is
    // blocked by this exact transaction before allowing the conflict to resolve.
    let allocation_task = tokio::spawn(allocate_automatic_ipv4_network(
        fixture.env.pool.clone(),
        fixture.vpc_id,
        fixture.tenant_organization_id.clone(),
    ));
    wait_until_query_blocked_by(
        &fixture.env.pool,
        blocker_pid,
        "INSERT INTO network_prefixes",
    )
    .await;
    blocker.commit().await.unwrap();

    // The single retry must stay on the same parent but choose the other free /31.
    let allocated = allocation_task.await.unwrap().unwrap();
    let interface = &allocated.interfaces[0];
    assert_eq!(
        interface.network_details,
        Some(NetworkDetails::VpcPrefixId(fixture.lower_ipv4_prefix_id)),
    );
    let mut txn = fixture.env.db_txn().await;
    let generated_segment = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(IdColumn, &interface.network_segment_id.unwrap()),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(generated_segment.len(), 1);
    assert_ne!(generated_segment[0].prefixes[0].prefix, conflicting_prefix);
    assert!(
        fixture
            .lower_ipv4_prefix
            .contains(generated_segment[0].prefixes[0].prefix.network())
    );
    txn.commit().await.unwrap();
}

/// Verifies a deleted candidate is rejected by its locking re-read so allocation
/// cannot proceed from stale discovery state.
#[crate::sqlx_test]
async fn test_auto_vpc_prefix_selection_rechecks_concurrent_candidate_deletion(pool: PgPool) {
    let fixture = create_auto_vpc_selection_fixture(pool).await;

    // Hold the lower candidate's soft-delete uncommitted. Candidate discovery
    // still sees the prior active row, while its selected-row lock must wait.
    let mut blocker = fixture.env.pool.begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await
        .unwrap();
    let _: VpcPrefixId = sqlx::query_scalar(
        r#"
            UPDATE network_vpc_prefixes
            SET deleted = NOW()
            WHERE id = $1
              AND deleted IS NULL
            -- Hold the candidate row until allocation reaches its locking re-read.
            RETURNING id
        "#,
    )
    .bind(fixture.lower_ipv4_prefix_id)
    .fetch_one(blocker.as_mut())
    .await
    .unwrap();

    // Commit only after the allocator is waiting on the deleted candidate.
    let allocation_task = tokio::spawn(allocate_automatic_ipv4_network(
        fixture.env.pool.clone(),
        fixture.vpc_id,
        fixture.tenant_organization_id.clone(),
    ));
    wait_until_query_blocked_by(&fixture.env.pool, blocker_pid, "FOR NO KEY UPDATE").await;
    blocker.commit().await.unwrap();

    // The locking re-read must reject the deleted row and fall through.
    let allocated = allocation_task.await.unwrap().unwrap();
    assert_eq!(
        allocated.interfaces[0].network_details,
        Some(NetworkDetails::VpcPrefixId(fixture.higher_ipv4_prefix_id)),
    );
}

/// Verifies a request retains its discovery-time candidate set so concurrent
/// capacity changes cannot disturb canonical allocation ordering.
#[crate::sqlx_test]
async fn test_auto_vpc_prefix_selection_freezes_concurrent_candidate_insert(pool: PgPool) {
    let fixture = create_auto_vpc_selection_fixture(pool).await;

    // Exhaust both existing candidates before arranging the discovery boundary.
    assert_static_ipv4_first_fit(&fixture).await;
    let final_original_allocation = allocate_automatic_ipv4_network(
        fixture.env.pool.clone(),
        fixture.vpc_id,
        fixture.tenant_organization_id.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        final_original_allocation.interfaces[0].network_details,
        Some(NetworkDetails::VpcPrefixId(fixture.higher_ipv4_prefix_id)),
    );

    // Hold the first candidate so a blocked selected-row lock proves the
    // allocation transaction already froze its candidate query result.
    let mut blocker = fixture.env.pool.begin().await.unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await
        .unwrap();
    let _: VpcPrefixId = sqlx::query_scalar(
        r#"
            SELECT id
            FROM network_vpc_prefixes
            WHERE id = $1
            -- Keep discovery unlocked but block the selected candidate re-read.
            FOR UPDATE
        "#,
    )
    .bind(fixture.lower_ipv4_prefix_id)
    .fetch_one(blocker.as_mut())
    .await
    .unwrap();
    let allocation_task = tokio::spawn(allocate_automatic_ipv4_network(
        fixture.env.pool.clone(),
        fixture.vpc_id,
        fixture.tenant_organization_id.clone(),
    ));
    wait_until_query_blocked_by(&fixture.env.pool, blocker_pid, "FOR NO KEY UPDATE").await;

    // Add usable capacity only after discovery, then let the frozen request continue.
    let inserted_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &fixture.env,
        fixture.vpc_id,
        "concurrently-inserted-candidate",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 218, 0, 8), 30).unwrap()),
    )
    .await;
    blocker.rollback().await.unwrap();

    // The in-flight request proves only its original candidates exhausted; a
    // fresh request discovers and allocates from the newly committed candidate.
    let error = allocation_task.await.unwrap().unwrap_err();
    assert!(matches!(error, crate::CarbideError::ResourceExhausted(_)));
    let retried = allocate_automatic_ipv4_network(
        fixture.env.pool.clone(),
        fixture.vpc_id,
        fixture.tenant_organization_id.clone(),
    )
    .await
    .unwrap();
    assert_eq!(
        retried.interfaces[0].network_details,
        Some(NetworkDetails::VpcPrefixId(inserted_prefix_id)),
    );
}

/// Verifies a VPC-only selector cannot bypass tenant ownership checks at the
/// public RPC boundary.
#[crate::sqlx_test]
async fn test_auto_vpc_prefix_selection_rejects_cross_tenant_rpc_request(pool: PgPool) {
    const OTHER_TENANT: &str = "auto-prefix-selection-other-tenant";

    // Create a valid second tenant and an allocatable host so only VPC ownership fails.
    let fixture = create_auto_vpc_selection_fixture(pool).await;
    create_fixture_tenant(&fixture.env, OTHER_TENANT)
        .await
        .unwrap();
    let managed_host = create_managed_host(&fixture.env).await;

    // Request the owning tenant's VPC from a different valid tenant through AllocateInstance.
    let error = fixture
        .env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(managed_host.id)
                .config(
                    InstanceConfig::default_tenant_and_os()
                        .tenant(rpc::TenantConfig {
                            tenant_organization_id: OTHER_TENANT.to_string(),
                            ..default_tenant_config()
                        })
                        .network(automatic_ipv4_rpc_network_config(fixture.vpc_id)),
                )
                .tonic_request(),
        )
        .await
        .unwrap_err();

    // Ownership failure must retain its precise public status and diagnostic.
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        format!(
            "VPC `{}` is not owned by tenant `{OTHER_TENANT}`",
            fixture.vpc_id
        ),
    );
}

/// Verifies force deletion recognizes automatic-selector resources so generated
/// segments and addresses cannot leak.
#[crate::sqlx_test]
async fn test_auto_vpc_prefix_selection_force_delete_marks_generated_segment_deleted(pool: PgPool) {
    let fixture = create_auto_vpc_selection_fixture(pool).await;
    let managed_host = create_managed_host(&fixture.env).await;

    // Allocate through the public selector and synchronize networking so the
    // address being released is known to have become active.
    let tinstance = managed_host
        .instance_builer(&fixture.env)
        .tenant_org(FIXTURE_TENANT_ORG_ID)
        .network(automatic_ipv4_rpc_network_config(fixture.vpc_id))
        .build()
        .await;
    let instance_id = tinstance.id;
    let persisted = tinstance.rpc_instance().await;
    assert_ipv4_auto_rpc_resolution(
        persisted.inner(),
        fixture.vpc_id,
        fixture.lower_ipv4_prefix_id,
    );
    assert!(
        !persisted.status().network().interfaces[0]
            .addresses
            .is_empty()
    );
    let generated_segment_id = persisted.config().network().interfaces[0]
        .network_segment_id
        .unwrap();

    // Force delete must route automatic intent through generated-resource cleanup.
    let response = fixture
        .env
        .api
        .admin_force_delete_machine(Request::new(AdminForceDeleteMachineRequest {
            host_query: managed_host.id.to_string(),
            delete_interfaces: false,
            delete_bmc_interfaces: false,
            delete_bmc_credentials: false,
            allow_delete_with_orphaned_dpf_crds: false,
            delete_bmc_suppressions: false,
            delete_retained_boot_interfaces: false,
            allow_delete_with_instance_type: false,
            allow_delete_with_instance: true,
        }))
        .await
        .unwrap()
        .into_inner();
    assert!(response.all_done);
    assert_eq!(response.instance_id, instance_id.to_string());
    assert!(
        fixture
            .env
            .find_instances(vec![instance_id])
            .await
            .instances
            .is_empty()
    );

    // The generated segment is queued for lifecycle deletion and its instance address is freed.
    let mut txn = fixture.env.db_txn().await;
    let generated_segments = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(IdColumn, &generated_segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    let [generated_segment] = generated_segments.as_slice() else {
        panic!("expected one force-deleted generated segment");
    };
    assert!(generated_segment.is_marked_as_deleted());
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &generated_segment_id)
            .await
            .unwrap(),
        0,
    );
    txn.commit().await.unwrap();
}

/// Holds one tenant-owned FNN VPC and ordered candidates so every scenario
/// shares the same deterministic first-fit baseline.
struct AutoVpcSelectionFixture {
    env: TestEnv,
    vpc_id: VpcId,
    // Candidate labels follow VPC prefix ID order, which drives static first-fit.
    lower_ipv4_prefix_id: VpcPrefixId,
    lower_ipv4_prefix: IpNetwork,
    higher_ipv4_prefix_id: VpcPrefixId,
    tenant_organization_id: TenantOrganizationId,
}

/// Holds fresh per-family candidates added after IPv4 exhaustion so future
/// family-mode checks cannot perturb the initial first-fit coverage.
struct AutoVpcDualStackPrefixes {
    ipv4_prefix_id: VpcPrefixId,
    ipv6_prefix_id: VpcPrefixId,
}

/// Creates an FNN VPC and derives candidate roles from prefix ID order so
/// static first-fit assertions remain deterministic.
async fn create_auto_vpc_selection_fixture(pool: PgPool) -> AutoVpcSelectionFixture {
    create_auto_vpc_selection_fixture_with_slaac(pool, false).await
}

/// Creates the fixture for automatic selection with an explicit VPC SLAAC policy.
async fn create_auto_vpc_selection_fixture_with_slaac(
    pool: PgPool,
    slaac_enabled: bool,
) -> AutoVpcSelectionFixture {
    // Build the shared environment without site-level FNN configuration and
    // register the tenant used by every scenario.
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID)
        .await
        .unwrap();

    // Automatic prefix selection is restricted to FNN VPCs.
    let vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "auto-prefix-selection-vpc".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .slaac_enabled(slaac_enabled)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();

    // Two non-overlapping /30 candidates provide two /31 linknets each. Their
    // CIDRs also avoid the default fixture networks used by the real instance.
    let first_prefix = IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 218, 0, 0), 30).unwrap());
    let first_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        vpc_id,
        "auto-prefix-candidate-one",
        first_prefix,
    )
    .await;
    let second_prefix = IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 218, 0, 4), 30).unwrap());
    let second_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        vpc_id,
        "auto-prefix-candidate-two",
        second_prefix,
    )
    .await;

    // First-fit order is based on prefix ID rather than creation order, so
    // derive stable lower/higher roles from the generated IDs.
    let (lower_prefix_id, lower_prefix, higher_prefix_id) = if first_prefix_id < second_prefix_id {
        (first_prefix_id, first_prefix, second_prefix_id)
    } else {
        (second_prefix_id, second_prefix, first_prefix_id)
    };

    AutoVpcSelectionFixture {
        env,
        vpc_id,
        lower_ipv4_prefix_id: lower_prefix_id,
        lower_ipv4_prefix: lower_prefix,
        higher_ipv4_prefix_id: higher_prefix_id,
        tenant_organization_id: FIXTURE_TENANT_ORG_ID.parse().unwrap(),
    }
}

/// Verifies prefix ID order determines first fit and each candidate is
/// exhausted before allocation falls through to the next one.
async fn assert_static_ipv4_first_fit(fixture: &AutoVpcSelectionFixture) {
    // Two /31 allocations fill the lower-ID /30; the third must fall through.
    for expected_prefix_id in [
        fixture.lower_ipv4_prefix_id,
        fixture.lower_ipv4_prefix_id,
        fixture.higher_ipv4_prefix_id,
    ] {
        let mut network_config =
            automatic_network_config(fixture.vpc_id, InstanceInterfaceIpFamilyMode::Ipv4Only);
        let mut txn = fixture.env.db_txn().await;
        allocate_network(
            &mut network_config,
            &fixture.tenant_organization_id,
            &mut txn,
        )
        .await
        .unwrap();

        // Resolution uses the rolling-compatible explicit fields internally.
        let interface = &network_config.interfaces[0];
        assert_eq!(
            interface.network_details,
            Some(NetworkDetails::VpcPrefixId(expected_prefix_id)),
        );
        assert!(interface.network_segment_id.is_some());
        txn.commit().await.unwrap();
    }
}

/// Verifies shared allocation logic does not weaken tenant ownership when a
/// request selects a prefix explicitly.
async fn assert_explicit_prefix_tenant_ownership(fixture: &AutoVpcSelectionFixture) {
    // Request an existing explicit prefix under a different tenant identity.
    let mut explicit_config = InstanceNetworkConfig::for_vpc_prefix_id(
        fixture.higher_ipv4_prefix_id,
        Some(fixture.vpc_id),
    );
    let wrong_tenant_organization_id = "another-tenant".parse().unwrap();
    let mut txn = fixture.env.db_txn().await;
    let error = allocate_network(
        &mut explicit_config,
        &wrong_tenant_organization_id,
        &mut txn,
    )
    .await
    .unwrap_err();

    // The failure must identify ownership rather than allocation capacity.
    assert!(matches!(
        error,
        crate::CarbideError::FailedPrecondition(message)
            if message.contains("is not owned by tenant")
    ));
    txn.rollback().await.unwrap();
}

/// Allocates one instance through the public automatic VPC selector.
async fn allocate_auto_vpc_instance(
    fixture: &AutoVpcSelectionFixture,
    family_mode: rpc::forge::InstanceInterfaceIpFamilyMode,
    name: &str,
) -> (TestManagedHost, rpc::Instance) {
    let managed_host = create_managed_host(&fixture.env).await;
    let (_, instance) = managed_host
        .instance_builer(&fixture.env)
        .tenant_org(FIXTURE_TENANT_ORG_ID)
        .network(automatic_rpc_network_config(fixture.vpc_id, family_mode))
        .metadata(rpc::Metadata {
            name: name.to_string(),
            description: "tests/instance".to_string(),
            labels: Vec::new(),
        })
        .build_and_return()
        .await;

    (managed_host, instance.into_inner())
}

/// Allocates and re-reads through the public boundary so intent and resolution
/// are proven to persist beyond the allocation response.
async fn allocate_and_assert_auto_vpc_instance(fixture: &AutoVpcSelectionFixture) {
    // Allocate the final original IPv4 linknet through the public selector and
    // verify its immediate projection.
    let (_, instance) = allocate_auto_vpc_instance(
        fixture,
        rpc::forge::InstanceInterfaceIpFamilyMode::Ipv4Only,
        "automatic-vpc-prefix-selection",
    )
    .await;
    assert_ipv4_auto_rpc_resolution(&instance, fixture.vpc_id, fixture.higher_ipv4_prefix_id);

    // Re-read through FindInstancesByIds to verify persisted intent and resolution.
    let instance_id = instance.id.unwrap();
    let persisted = fixture.env.one_instance(instance_id).await;
    assert_ipv4_auto_rpc_resolution(
        persisted.inner(),
        fixture.vpc_id,
        fixture.higher_ipv4_prefix_id,
    );
}

/// Confirms original IPv4 capacity is exhausted so later family-mode checks
/// can resolve only from freshly added prefixes.
async fn assert_ipv4_candidates_exhausted(fixture: &AutoVpcSelectionFixture) {
    // Four earlier allocations consumed both /31 linknets in both original /30 candidates.
    let mut exhausted_config =
        automatic_network_config(fixture.vpc_id, InstanceInterfaceIpFamilyMode::Ipv4Only);
    let mut txn = fixture.env.db_txn().await;
    let error = allocate_network(
        &mut exhausted_config,
        &fixture.tenant_organization_id,
        &mut txn,
    )
    .await
    .unwrap_err();
    assert!(matches!(error, crate::CarbideError::ResourceExhausted(_)));
    txn.rollback().await.unwrap();
}

/// Adds per-family capacity after first-fit checks so it cannot affect their
/// candidate ordering or exhaustion assertions.
async fn add_dual_stack_prefix_capacity(
    fixture: &AutoVpcSelectionFixture,
) -> AutoVpcDualStackPrefixes {
    // Add capacity only after exhaustion so it cannot affect earlier first-fit checks.
    // The IPv6 /126 supplies one /127 to IPv6-only and another to dual stack.
    let dual_ipv4_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &fixture.env,
        fixture.vpc_id,
        "dual-stack-ipv4-candidate",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 218, 0, 8), 30).unwrap()),
    )
    .await;
    let ipv6_prefix =
        IpNetwork::V6(Ipv6Network::new("fd42:218::".parse::<Ipv6Addr>().unwrap(), 126).unwrap());
    let ipv6_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &fixture.env,
        fixture.vpc_id,
        "automatic-ipv6-candidate",
        ipv6_prefix,
    )
    .await;

    AutoVpcDualStackPrefixes {
        ipv4_prefix_id: dual_ipv4_prefix_id,
        ipv6_prefix_id,
    }
}

/// Exercises IPv6-only resolution, persistence, and DPU config rendering
/// through the public allocation boundary.
#[allow(deprecated)]
async fn assert_ipv6_only_resolution(
    fixture: &AutoVpcSelectionFixture,
    ipv6_prefix_id: VpcPrefixId,
) {
    let (managed_host, instance) = allocate_auto_vpc_instance(
        fixture,
        rpc::forge::InstanceInterfaceIpFamilyMode::Ipv6Only,
        "automatic-ipv6-only-selection",
    )
    .await;
    let ipv6_only_segment_id = assert_auto_rpc_resolution(
        &instance,
        fixture.vpc_id,
        rpc::forge::InstanceInterfaceIpFamilyMode::Ipv6Only,
        None,
        Some(ipv6_prefix_id),
    );

    // Re-read through the public API to prove the family intent and selected
    // prefix persisted beyond the allocation response.
    let instance_id = instance.id.unwrap();
    let persisted = fixture.env.one_instance(instance_id).await;
    assert_auto_rpc_resolution(
        persisted.inner(),
        fixture.vpc_id,
        rpc::forge::InstanceInterfaceIpFamilyMode::Ipv6Only,
        None,
        Some(ipv6_prefix_id),
    );
    let status = persisted.status();
    let status_interface = &status.network().interfaces[0];
    assert_eq!(status_interface.addresses.len(), 1);
    assert!(
        status_interface.addresses[0]
            .parse::<IpAddr>()
            .unwrap()
            .is_ipv6()
    );
    assert_eq!(status_interface.prefixes.len(), 1);
    assert!(
        status_interface.prefixes[0]
            .parse::<IpNetwork>()
            .unwrap()
            .is_ipv6()
    );

    let mut txn = fixture.env.db_txn().await;
    let ipv6_only_segment = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(IdColumn, &ipv6_only_segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(ipv6_only_segment[0].prefixes.len(), 1);
    assert_eq!(
        ipv6_only_segment[0].prefixes[0].prefix,
        "fd42:218::/127".parse::<IpNetwork>().unwrap(),
    );

    let ipv6_only_addresses =
        db::instance_address::find_by_segment_id(txn.as_mut(), &ipv6_only_segment_id)
            .await
            .unwrap();

    // Persistence contains one odd IPv6 host address from the selected /127.
    assert_eq!(ipv6_only_addresses.len(), 1);
    assert!(matches!(
        ipv6_only_addresses[0].address,
        IpAddr::V6(address) if address.to_bits() & 1 == 1
    ));
    txn.commit().await.unwrap();

    // Core publishes only the real V6 family and leaves deprecated V4 fields
    // empty rather than fabricating an unusable V4 address configuration.
    let managed_config = fixture
        .env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu().id.into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let [tenant_interface] = managed_config.tenant_interfaces.as_slice() else {
        panic!("expected one IPv6-only tenant interface");
    };
    assert!(tenant_interface.gateway.is_none());
    assert!(tenant_interface.ip.is_none());
    assert!(tenant_interface.interface_prefix.is_none());
    assert!(tenant_interface.prefix.is_none());
    assert!(tenant_interface.svi_ip.is_none());
    let [address] = tenant_interface.addresses.as_slice() else {
        panic!("expected one IPv6 tenant address configuration");
    };
    assert_eq!(
        rpc::forge::AddressFamily::try_from(address.address_family).unwrap(),
        rpc::forge::AddressFamily::V6,
    );
    assert!(address.ip.parse::<IpAddr>().unwrap().is_ipv6());
    assert_eq!(
        tenant_interface
            .ipv6_interface_config
            .as_ref()
            .map(|config| config.ip.as_str()),
        Some(address.ip.as_str()),
    );
}

/// Exercises dual-stack resolution and per-family address assignment through
/// the public allocation boundary.
// This test deliberately verifies the deprecated V4 compatibility projection.
#[allow(deprecated)]
async fn assert_dual_stack_resolution(
    fixture: &AutoVpcSelectionFixture,
    prefixes: &AutoVpcDualStackPrefixes,
) {
    // IPv6-only consumed the first /127. Dual stack uses IPv4 as primary and
    // attaches the remaining IPv6 /127 to the same generated segment.
    let (managed_host, instance) = allocate_auto_vpc_instance(
        fixture,
        rpc::forge::InstanceInterfaceIpFamilyMode::DualStack,
        "automatic-dual-stack-selection",
    )
    .await;
    let dual_stack_segment_id = assert_auto_rpc_resolution(
        &instance,
        fixture.vpc_id,
        rpc::forge::InstanceInterfaceIpFamilyMode::DualStack,
        Some(prefixes.ipv4_prefix_id),
        Some(prefixes.ipv6_prefix_id),
    );

    let instance_id = instance.id.unwrap();
    let persisted = fixture.env.one_instance(instance_id).await;
    assert_auto_rpc_resolution(
        persisted.inner(),
        fixture.vpc_id,
        rpc::forge::InstanceInterfaceIpFamilyMode::DualStack,
        Some(prefixes.ipv4_prefix_id),
        Some(prefixes.ipv6_prefix_id),
    );
    let status = persisted.status();
    let status_interface = &status.network().interfaces[0];
    assert_eq!(status_interface.addresses.len(), 2);
    assert_eq!(
        status_interface
            .addresses
            .iter()
            .filter(|address| address.parse::<IpAddr>().unwrap().is_ipv4())
            .count(),
        1,
    );
    assert_eq!(status_interface.prefixes.len(), 2);
    assert_eq!(
        status_interface
            .prefixes
            .iter()
            .filter(|prefix| prefix.parse::<IpNetwork>().unwrap().is_ipv4())
            .count(),
        1,
    );

    // Both selected family linknets must share one generated segment.
    let mut txn = fixture.env.db_txn().await;
    let dual_stack_segment = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(IdColumn, &dual_stack_segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(dual_stack_segment[0].prefixes.len(), 2);
    assert_eq!(
        dual_stack_segment[0]
            .prefixes
            .iter()
            .filter(|prefix| prefix.prefix.is_ipv4())
            .count(),
        1,
    );
    assert_eq!(
        dual_stack_segment[0]
            .prefixes
            .iter()
            .filter(|prefix| prefix.prefix.is_ipv6())
            .count(),
        1,
    );

    let dual_stack_addresses =
        db::instance_address::find_by_segment_id(txn.as_mut(), &dual_stack_segment_id)
            .await
            .unwrap();

    // Persistence contains one address per family.
    assert_eq!(dual_stack_addresses.len(), 2);
    assert_eq!(
        dual_stack_addresses
            .iter()
            .filter(|address| address.address.is_ipv4())
            .count(),
        1,
    );
    assert!(dual_stack_addresses.iter().any(|address| matches!(
        address.address,
        IpAddr::V6(address) if address.to_bits() & 1 == 1
    )));
    let ipv6_segment_prefix = dual_stack_segment[0]
        .prefixes
        .iter()
        .find(|prefix| prefix.prefix.is_ipv6())
        .expect("dual-stack segment has an IPv6 prefix")
        .prefix
        .to_string();
    txn.commit().await.unwrap();

    let managed_config = fixture
        .env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu().id.into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let [tenant_interface] = managed_config.tenant_interfaces.as_slice() else {
        panic!("expected one dual-stack tenant interface");
    };
    let [ipv4_address, ipv6_address] = tenant_interface.addresses.as_slice() else {
        panic!("expected IPv4 and IPv6 tenant address configurations");
    };
    assert_eq!(
        [ipv4_address.address_family(), ipv6_address.address_family()],
        [rpc::forge::AddressFamily::V4, rpc::forge::AddressFamily::V6],
    );
    assert!(tenant_interface.gateway.is_some());
    assert!(tenant_interface.ip.is_some());
    assert!(tenant_interface.interface_prefix.is_some());
    assert!(tenant_interface.prefix.is_some());
    assert_eq!(tenant_interface.gateway, ipv4_address.gateway);
    assert_eq!(
        tenant_interface.ip.as_deref(),
        Some(ipv4_address.ip.as_str())
    );
    assert_eq!(
        tenant_interface.interface_prefix.as_deref(),
        Some(ipv4_address.interface_prefix.as_str()),
    );
    assert_eq!(
        tenant_interface.prefix.as_deref(),
        Some(ipv4_address.prefix.as_str())
    );
    assert_eq!(tenant_interface.svi_ip, ipv4_address.svi_ip);
    let legacy_ipv6 = tenant_interface
        .ipv6_interface_config
        .as_ref()
        .expect("dual-stack config has an IPv6 sidecar");
    assert_eq!(ipv6_address.ip, legacy_ipv6.ip);
    assert_eq!(ipv6_address.interface_prefix, legacy_ipv6.interface_prefix);
    assert_eq!(ipv6_address.prefix, ipv6_segment_prefix);
    assert_eq!(ipv6_address.svi_ip, legacy_ipv6.svi_ip);
    assert!(ipv6_address.gateway.is_none());
    assert!(ipv6_address.tenant_vrf_loopback_ip.is_none());
}

/// Builds unresolved selector intent so allocator internals can be exercised
/// without RPC orchestration.
fn automatic_network_config(
    vpc_id: VpcId,
    family_mode: InstanceInterfaceIpFamilyMode,
) -> InstanceNetworkConfig {
    // Seed the interface shape with the constructor for an explicitly selected
    // prefix, then replace its dummy prefix with unresolved automatic intent.
    let mut config = InstanceNetworkConfig::for_vpc_prefix_id(VpcPrefixId::new(), Some(vpc_id));
    let interface = &mut config.interfaces[0];
    interface.network_details = None;
    interface.vpc_selection = Some(InstanceInterfaceVpcSelection {
        vpc_id,
        family_mode,
    });
    config
}

/// Allocates one automatic IPv4 config in its own transaction for concurrency tests.
async fn allocate_automatic_ipv4_network(
    pool: PgPool,
    vpc_id: VpcId,
    tenant_organization_id: TenantOrganizationId,
) -> Result<InstanceNetworkConfig, crate::CarbideError> {
    let mut config = automatic_network_config(vpc_id, InstanceInterfaceIpFamilyMode::Ipv4Only);
    let mut txn = pool.begin().await.unwrap();
    let result = allocate_network(&mut config, &tenant_organization_id, txn.as_mut()).await;

    // Commit successful generated resources; failed attempts must leave no outer effects.
    match result {
        Ok(()) => {
            txn.commit().await.unwrap();
            Ok(config)
        }
        Err(error) => {
            txn.rollback().await.unwrap();
            Err(error)
        }
    }
}

/// Waits for a query to reach the expected lock boundary so concurrency tests
/// release blockers deterministically rather than by timing.
async fn wait_until_query_blocked_by(pool: &PgPool, blocker_pid: i32, query_fragment: &str) {
    // Poll for up to 30 seconds to let the spawned query reach the held lock.
    for _ in 0..300 {
        let blocked: bool = sqlx::query_scalar(
            r#"
                SELECT EXISTS (
                    SELECT 1
                    FROM pg_stat_activity AS activity
                    WHERE activity.datname = current_database()
                      AND activity.wait_event_type = 'Lock'
                      -- Match only work blocked by this test's transaction.
                      AND $1 = ANY(pg_blocking_pids(activity.pid))
                      AND activity.query ILIKE '%' || $2 || '%'
                )
            "#,
        )
        .bind(blocker_pid)
        .bind(query_fragment)
        .fetch_one(pool)
        .await
        .unwrap();
        if blocked {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    panic!("query never blocked on {query_fragment}");
}

/// Builds unresolved external selector intent so public-boundary tests do not
/// pre-resolve a prefix themselves.
fn automatic_ipv4_rpc_network_config(vpc_id: VpcId) -> rpc::InstanceNetworkConfig {
    automatic_rpc_network_config(vpc_id, rpc::forge::InstanceInterfaceIpFamilyMode::Ipv4Only)
}

fn automatic_rpc_network_config(
    vpc_id: VpcId,
    family_mode: rpc::forge::InstanceInterfaceIpFamilyMode,
) -> rpc::InstanceNetworkConfig {
    rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: None,
            network_details: Some(rpc::forge::instance_interface_config::NetworkDetails::Vpc(
                rpc::forge::InstanceInterfaceVpcSelection {
                    vpc_id: Some(vpc_id),
                    family_mode: family_mode as i32,
                },
            )),
            device: None,
            device_instance: 0,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    }
}

/// Verifies config retains caller VPC intent while status exposes its resolved
/// prefix, protecting the distinction between intent and active allocation.
fn assert_auto_rpc_resolution(
    instance: &rpc::Instance,
    vpc_id: VpcId,
    family_mode: rpc::forge::InstanceInterfaceIpFamilyMode,
    ipv4_vpc_prefix_id: Option<VpcPrefixId>,
    ipv6_vpc_prefix_id: Option<VpcPrefixId>,
) -> NetworkSegmentId {
    // Config retains VPC-level caller intent while carrying the generated segment.
    let interface = &instance
        .config
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces[0];
    let selection = match interface.network_details.as_ref() {
        Some(rpc::forge::instance_interface_config::NetworkDetails::Vpc(selection)) => selection,
        other => panic!("expected automatic VPC selection, got {other:?}"),
    };
    assert_eq!(selection.vpc_id, Some(vpc_id));
    assert_eq!(selection.family_mode, family_mode as i32);
    let network_segment_id = interface.network_segment_id.unwrap();

    // Status publishes the active family-keyed prefix separately.
    let status_interface = &instance
        .status
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces[0];
    assert_eq!(status_interface.vpc_id, Some(vpc_id));
    let resolved = status_interface.resolved_vpc_prefixes.as_ref().unwrap();
    assert_eq!(resolved.ipv4_vpc_prefix_id, ipv4_vpc_prefix_id);
    assert_eq!(resolved.ipv6_vpc_prefix_id, ipv6_vpc_prefix_id);

    network_segment_id
}

fn assert_ipv4_auto_rpc_resolution(
    instance: &rpc::Instance,
    vpc_id: VpcId,
    vpc_prefix_id: VpcPrefixId,
) {
    assert_auto_rpc_resolution(
        instance,
        vpc_id,
        rpc::forge::InstanceInterfaceIpFamilyMode::Ipv4Only,
        Some(vpc_prefix_id),
        None,
    );
}

async fn create_tenant_overlay_prefix(env: &TestEnv, vpc_id: VpcId) -> VpcPrefixId {
    create_tenant_overlay_prefix_with_prefix(
        env,
        vpc_id,
        "vpc prefix 1",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 217, 5, 224), 27).unwrap()),
    )
    .await
}

/// Creates a tenant overlay VPC prefix with an explicit CIDR for allocation tests.
async fn create_tenant_overlay_prefix_with_prefix(
    env: &TestEnv,
    vpc_id: VpcId,
    name: &str,
    prefix: IpNetwork,
) -> VpcPrefixId {
    let mut txn = env.db_txn().await;

    // Look up the current VPC version so the prefix insert can increment it.
    let vpcs = db::vpc::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(db::vpc::IdColumn, &vpc_id),
    )
    .await
    .unwrap();
    let expected_vpc_version = vpcs[0].version;

    // Persist the prefix directly so tests can focus on allocation behavior.
    let vpc_prefix_id = db::vpc_prefix::persist(
        model::vpc_prefix::NewVpcPrefix {
            id: uuid::Uuid::new_v4().into(),
            site_prefix_id: None,
            vpc_id,
            config: VpcPrefixConfig { prefix },
            metadata: Metadata {
                name: name.to_string(),
                description: "desc".to_string(),
                labels: HashMap::new(),
            },
        },
        expected_vpc_version,
        &mut txn,
    )
    .await
    .unwrap()
    .id;
    txn.commit().await.unwrap();
    vpc_prefix_id
}

/// Builds a two-interface physical network config backed by VPC prefixes.
fn dual_physical_network_config_with_vpc_prefixes(
    first_prefix_id: VpcPrefixId,
    second_prefix_id: VpcPrefixId,
) -> rpc::InstanceNetworkConfig {
    // Put each PF on a distinct BlueField device instance so validation treats
    // them as separate physical interfaces.
    let interfaces = [first_prefix_id, second_prefix_id]
        .into_iter()
        .enumerate()
        .map(
            |(device_instance, vpc_prefix_id)| rpc::InstanceInterfaceConfig {
                function_type: rpc::InterfaceFunctionType::Physical as i32,
                network_segment_id: None,
                network_details: Some(
                    rpc::forge::instance_interface_config::NetworkDetails::VpcPrefixId(
                        vpc_prefix_id,
                    ),
                ),
                device: Some("BlueField SoC".to_string()),
                device_instance: device_instance as u32,
                virtual_function_id: None,
                ip_address: None,
                ipv6_interface_config: None,
                routing_profile: None,
            },
        )
        .collect();

    rpc::InstanceNetworkConfig {
        interfaces,
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    }
}

#[crate::sqlx_test]
async fn test_allocate_and_update_with_network_security_group(
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;

    populate_network_security_groups(env.api.clone()).await;

    // NSG ID of and NSG for the default tenant provided by fixtures.
    let good_network_security_group_id = "fd3ab096-d811-11ef-8fe9-7be4b2483448";

    // NSG ID of not-the-default-tenant provided by fixtures.
    let bad_network_security_group_id = "ddfcabc4-92dc-41e2-874e-2c7eeb9fa156";

    // Create a new managed host in the DB and get the snapshot.
    let mh = site_explorer::new_host(&env, ManagedHostConfig::default())
        .await
        .unwrap();

    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Try to create an instance, but send in a valid and
    // existing NSG ID that doesn't match the tenant of
    // instance being created.
    // This should fail.
    let _ = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.host_snapshot.id)
                .config(
                    InstanceConfig::default_tenant_and_os()
                        .network(single_interface_network_config(segment_id))
                        .network_security_group_id(bad_network_security_group_id)
                        .rpc(),
                )
                .metadata(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                })
                .tonic_request(),
        )
        .await
        .unwrap_err();

    // Try that once more, but with an NSG ID
    // that has the same tenant as the instance.
    let i = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.host_snapshot.id)
                .config(
                    InstanceConfig::default_tenant_and_os()
                        .network(single_interface_network_config(segment_id))
                        .network_security_group_id(good_network_security_group_id)
                        .rpc(),
                )
                .metadata(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                })
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    // Check that the instance actually has the ID we expect
    assert_eq!(
        i.config.unwrap().network_security_group_id.as_deref(),
        Some(good_network_security_group_id)
    );

    let instance_id = i.id.unwrap();

    // Now update to remove the NSG attachment.
    let i = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(
                    InstanceConfig::default_tenant_and_os()
                        .network(single_interface_network_config(segment_id))
                        .into(),
                ),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    // Check that the instance no longer has an NSG ID
    assert!(i.config.unwrap().network_security_group_id.is_none());

    // Now try to update it again and try to add the NSG with the mismatched tenant org
    // Now update to remove the NSG attachment.
    let _ = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(
                    InstanceConfig::default_tenant_and_os()
                        .network(single_interface_network_config(segment_id))
                        .network_security_group_id(bad_network_security_group_id)
                        .rpc(),
                ),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap_err();

    // Now try to update it again and but with a good NSG
    let i = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(
                    InstanceConfig::default_tenant_and_os()
                        .network(single_interface_network_config(segment_id))
                        .network_security_group_id(good_network_security_group_id)
                        .into(),
                ),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    // Check that the instance actually has the ID we expect
    assert_eq!(
        i.config.unwrap().network_security_group_id.as_deref(),
        Some(good_network_security_group_id)
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_instance_cannot_allocate_requested_ip_with_network_segment(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let (segment_id, segment_id2) = env.create_vpc_and_dual_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id)
            .await
            .unwrap(),
        0
    );
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Ready
    ));
    txn.commit().await.unwrap();

    // Attempt to create an instance with a network segment and
    // an explicit IP request.
    let err = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
                .config(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(rpc::forge::InstanceOperatingSystemConfig {
                        phone_home_enabled: false,
                        run_provisioning_instructions_on_every_boot: false,
                        user_data: Some("SomeRandomData1".to_string()),
                        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
                            rpc::forge::InlineIpxe {
                                ipxe_script: "SomeRandomiPxe1".to_string(),
                            },
                        )),
                    }),
                    network: Some(rpc::InstanceNetworkConfig {
                        interfaces: vec![rpc::InstanceInterfaceConfig {
                            ip_address: Some("192.168.0.1".to_string()),
                            ipv6_interface_config: None,
                            routing_profile: None,

                            function_type: rpc::InterfaceFunctionType::Physical as i32,
                            network_segment_id: None,
                            network_details: Some(
                                rpc::forge::instance_interface_config::NetworkDetails::SegmentId(
                                    segment_id2,
                                ),
                            ),
                            device: None,
                            device_instance: 0,
                            virtual_function_id: None,
                        }],
                        #[allow(deprecated)]
                        auto: false,
                        auto_config: None,
                    }),
                    infiniband: None,
                    network_security_group_id: None,
                    dpu_extension_services: None,
                    nvlink: None,
                    spxconfig: None,
                    power_profile: None,
                })
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect_err("IP request with network segment should not be allowed");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .contains("explicit IP requests are only supported for VPC prefixes")
    );
}

#[crate::sqlx_test]
async fn test_allocate_and_update_network_config_instance(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let (segment_id, segment_id2) = env.create_vpc_and_dual_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id)
            .await
            .unwrap(),
        0
    );
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Ready
    ));
    txn.commit().await.unwrap();

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let instance = tinstance.rpc_instance().await;

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    assert_eq!(
        instance.status().network().configs_synced(),
        rpc::SyncState::Synced
    );

    let new_network_config = rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: None,
            network_details: Some(
                rpc::forge::instance_interface_config::NetworkDetails::SegmentId(segment_id2),
            ),
            device: None,
            device_instance: 0,
            virtual_function_id: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };

    // Now update to change network config.
    let _ = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(new_network_config),
                    infiniband: None,
                    nvlink: None,
                    network_security_group_id: None,
                    dpu_extension_services: None,
                    spxconfig: None,
                    power_profile: None,
                }),
                instance_id: instance.rpc_id(),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap();

    let instance = tinstance.rpc_instance().await;

    assert_eq!(
        instance.status().network().configs_synced(),
        rpc::SyncState::Pending
    );

    let mut txn = env.db_txn().await;
    let instance = tinstance.db_instance(&mut txn).await;
    txn.rollback().await.unwrap();

    assert!(instance.update_network_config_request.is_some());
    let update_req = instance.update_network_config_request.unwrap();
    let expected = NetworkDetails::NetworkSegment(segment_id2);

    assert_eq!(
        expected,
        update_req.new_config.interfaces[0]
            .network_details
            .clone()
            .unwrap(),
    );
}

#[crate::sqlx_test]
async fn test_allocate_and_update_network_config_instance_state_machine(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let (segment_id, segment_id2) = env.create_vpc_and_dual_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.db_txn().await;
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &segment_id)
            .await
            .unwrap(),
        0
    );
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Ready
    ));
    txn.commit().await.unwrap();

    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let instance = tinstance.rpc_instance().await;

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    assert_eq!(
        instance.status().network().configs_synced(),
        rpc::SyncState::Synced
    );

    let new_network_config = rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: None,
            network_details: Some(
                rpc::forge::instance_interface_config::NetworkDetails::SegmentId(segment_id2),
            ),
            device: None,
            device_instance: 0,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };

    // Now update to change network config.
    let _ = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(new_network_config),
                    infiniband: None,
                    nvlink: None,
                    spxconfig: None,
                    network_security_group_id: None,
                    dpu_extension_services: None,
                    power_profile: None,
                }),
                instance_id: instance.rpc_id(),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap();

    // Instance should move to NetworkConfigUpdateState::WaitingForNetworkSegmentToBeReady
    env.run_machine_state_controller_iteration().await;
    // Instance should move to NetworkConfigUpdateState::WaitingForConfigSynced
    env.run_machine_state_controller_iteration().await;
    // and stay there only.
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.db_txn().await;
    let current_state = mh.host().db_machine(&mut txn).await;
    let current_state = current_state.current_state();
    println!("Current State: {current_state}");
    assert!(matches!(
        current_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::NetworkConfigUpdate {
                network_config_update_state: NetworkConfigUpdateState::WaitingForConfigSynced
            }
        }
    ));
    txn.rollback().await.unwrap();

    // - forge-dpu-agent gets an instance network to configure, reports it configured
    mh.network_configured(&env).await;
    // Move to ReleaseOldResources state.
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::NetworkConfigUpdate {
                network_config_update_state: NetworkConfigUpdateState::ReleaseOldResources
            }
        }
    ));
    txn.rollback().await.unwrap();
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    ));
    txn.rollback().await.unwrap();
}

#[crate::sqlx_test]
async fn test_allocate_instance_with_multiple_fnn_vpc_prefixes(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;

    // Create two FNN VPCs and prefixes to exercise cross-VPC allocation.
    let first_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "fnn vpc 1".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let second_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "fnn vpc 2".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let first_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        first_vpc,
        "fnn vpc prefix 1",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 217, 5, 224), 27).unwrap()),
    )
    .await;
    let second_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        second_vpc,
        "fnn vpc prefix 2",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 217, 6, 224), 27).unwrap()),
    )
    .await;

    // Allocate an instance whose interfaces draw from both VPC prefixes.
    let instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
                .config(
                    InstanceConfig::default_tenant_and_os()
                        .tenant(fixture_tenant_config())
                        .network(dual_physical_network_config_with_vpc_prefixes(
                            first_prefix_id,
                            second_prefix_id,
                        )),
                )
                .metadata(rpc::Metadata {
                    name: "multi-fnn-vpc".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await
        .expect("FNN instance allocation across multiple VPCs should succeed")
        .into_inner();

    // Verify the response resolved both interfaces that selected prefixes.
    let interfaces = &instance
        .config
        .as_ref()
        .unwrap()
        .network
        .as_ref()
        .unwrap()
        .interfaces;
    assert_eq!(interfaces.len(), 2);
    assert!(
        interfaces
            .iter()
            .all(|iface| iface.network_segment_id.is_some())
    );

    // Fetch through the API to verify the persisted config, not just the create response.
    let persisted = env.one_instance(instance.id.unwrap()).await;
    let persisted_interfaces = &persisted.config().network().interfaces;
    let segment_ids = persisted_interfaces
        .iter()
        .map(|iface| iface.network_segment_id.unwrap())
        .collect_vec();
    assert_eq!(segment_ids.iter().copied().collect::<HashSet<_>>().len(), 2);

    // Verify the allocated segments are attached to both requested FNN VPCs.
    let mut txn = env.db_txn().await;
    let segments = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::List(IdColumn, &segment_ids),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        segments
            .iter()
            .map(|segment| segment.config.vpc_id.unwrap())
            .collect::<HashSet<_>>(),
        HashSet::from([first_vpc, second_vpc])
    );
}

#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_fnn_vrf_loopbacks_are_per_vpc_for_pf_and_vf_on_one_dpu(pool: sqlx::PgPool) {
    let mut overrides = TestEnvOverrides::default().with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().use_vpc_vrf_loopback = true;
    let env = create_test_env_with_overrides(pool, overrides).await;

    // Create FNN tenants matching the existing peer-VPC fixture organizations.
    for (organization_id, name) in [
        (FIXTURE_TENANT_ORG_ID, "fnn vf loopback tenant 1"),
        (
            "e65a9d69-39d2-4872-a53e-e5cb87c84e75",
            "fnn vf loopback tenant 2",
        ),
    ] {
        env.api
            .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
                organization_id: organization_id.to_string(),
                routing_profile_type: Some("INTERNAL".to_string()),
                metadata: Some(rpc::forge::Metadata {
                    name: name.to_string(),
                    ..Default::default()
                }),
            }))
            .await
            .unwrap();
    }

    // Create two FNN VPCs and allocate PF/VF interfaces on one DPU.
    let (first_vpc, _, first_segment_id, second_vpc, _, second_segment_id) = env
        .create_vpc_and_peer_vpc_with_tenant_segments(
            rpc::forge::VpcVirtualizationType::Fnn,
            rpc::forge::VpcVirtualizationType::Fnn,
        )
        .await;
    let first_vpc = first_vpc.expect("first VPC should be present");
    let second_vpc = second_vpc.expect("second VPC should be present");
    let mh = create_managed_host(&env).await;
    let dpu_id = mh.dpu_ids[0];
    let instance = mh
        .instance_builer(&env)
        .network(single_interface_network_config_with_vfs(vec![
            first_segment_id,
            second_segment_id,
        ]))
        .build()
        .await;

    // Fetch the single DPU config and verify both PF and VF receive loopbacks.
    let network_config = env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(dpu_id.into()),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(network_config.tenant_interfaces.len(), 2);
    assert_eq!(
        network_config.tenant_interfaces[0].function_type,
        rpc::InterfaceFunctionType::Physical as i32
    );
    assert_eq!(
        network_config.tenant_interfaces[1].function_type,
        rpc::InterfaceFunctionType::Virtual as i32
    );
    let first_loopback = network_config.tenant_interfaces[0]
        .tenant_vrf_loopback_ip
        .clone()
        .expect("PF VPC loopback should be present");
    let second_loopback = network_config.tenant_interfaces[1]
        .tenant_vrf_loopback_ip
        .clone()
        .expect("VF VPC loopback should be present");
    assert_ne!(first_loopback, second_loopback);

    // Update the instance to keep only the PF-backed VPC interface.
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(first_segment_id)),
                infiniband: None,
                nvlink: None,
                spxconfig: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                power_profile: None,
            }),
            instance_id: Some(instance.id),
            metadata: Some(rpc::Metadata {
                name: "single-fnn-vpc".to_string(),
                description: "tests/instance".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    // Move the instance to ready state after the network config update.
    env.run_machine_state_controller_iteration_network_config_return_to_ready(&mh, false)
        .await;

    // Verify the retained PF VPC loopback remains and the removed VF VPC loopback is gone.
    let mut txn = env.db_txn().await;
    let retained_loopback = db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &first_vpc)
        .await
        .unwrap()
        .expect("retained PF VPC loopback should remain");
    assert_eq!(retained_loopback.loopback_ip.to_string(), first_loopback);
    assert!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &second_vpc)
            .await
            .unwrap()
            .is_none()
    );
}

/// Verifies that non-FNN interfaces cannot span direct segments from multiple VPCs.
#[crate::sqlx_test]
async fn test_allocate_instance_rejects_multiple_non_fnn_network_segments(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;

    // Create two non-FNN tenant segments across two VPCs.
    let (_, _, first_segment_id, _, _, second_segment_id) = env
        .create_vpc_and_peer_vpc_with_tenant_segments(
            rpc::forge::VpcVirtualizationType::EthernetVirtualizer,
            rpc::forge::VpcVirtualizationType::EthernetVirtualizer,
        )
        .await;

    // Non-FNN segments must still reject cross-VPC interface configs.
    let err = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
                .config(InstanceConfig::default_tenant_and_os().network(
                    interface_network_config_with_devices(
                        &[first_segment_id, second_segment_id],
                        &[
                            DeviceLocator {
                                device: "BlueField SoC".to_string(),
                                device_instance: 0,
                            },
                            DeviceLocator {
                                device: "BlueField SoC".to_string(),
                                device_instance: 1,
                            },
                        ],
                    ),
                ))
                .tonic_request(),
        )
        .await
        .expect_err("non-FNN cross-VPC segment allocation should fail");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .contains("found segments attached to multiple VPCs")
    );
}

/// Verifies that dual-stack prefixes on one interface cannot cross VPC boundaries.
#[crate::sqlx_test]
async fn test_allocate_instance_rejects_dual_stack_prefixes_from_different_vpcs(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;

    // Create two FNN VPCs so the global multi-FNN check passes.
    let first_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "dual-stack fnn vpc 1".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let second_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "dual-stack fnn vpc 2".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();

    // Put the IPv4 and IPv6 prefixes in different VPCs on the same interface.
    let primary_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        first_vpc,
        "dual-stack primary prefix",
        IpNetwork::V4(Ipv4Network::new(Ipv4Addr::new(10, 217, 9, 224), 27).unwrap()),
    )
    .await;
    let ipv6_prefix_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        second_vpc,
        "dual-stack ipv6 prefix",
        IpNetwork::V6(Ipv6Network::new(Ipv6Addr::from_str("fd00:217:9::").unwrap(), 120).unwrap()),
    )
    .await;

    // Reject creating a single dual-stack segment that crosses VPC boundaries.
    let err = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
                .config(
                    InstanceConfig::default_tenant_and_os()
                        .tenant(fixture_tenant_config())
                        .network(rpc::InstanceNetworkConfig {
                            interfaces: vec![rpc::InstanceInterfaceConfig {
                            function_type: rpc::InterfaceFunctionType::Physical as i32,
                            network_segment_id: None,
                            network_details: Some(
                                rpc::forge::instance_interface_config::NetworkDetails::VpcPrefixId(
                                    primary_prefix_id,
                                ),
                            ),
                            device: Some("BlueField SoC".to_string()),
                            device_instance: 0,
                            virtual_function_id: None,
                            ip_address: None,
                            ipv6_interface_config: Some(rpc::forge::InstanceInterfaceIpv6Config {
                                vpc_prefix_id: Some(ipv6_prefix_id),
                                ip_address: None,
                            }),
                            routing_profile: None,
                        }],
                            #[allow(deprecated)]
                            auto: false,
                            auto_config: None,
                        }),
                )
                .tonic_request(),
        )
        .await
        .expect_err("dual-stack prefixes from different VPCs should fail");

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(
        err.message()
            .contains("dual-stack VPC prefixes must belong to the same VPC")
    );
}

// ================================================================================================
// Enhanced InstanceReleaseRequest API Tests (Issue Reporting & Repair Tenant Support)
// ================================================================================================
//
// Test Organization:
// 1. test_instance_release_backward_compatibility - API compatibility + no health overrides
// 2. test_instance_release_new_features - Issue reporting + repair tenant flags individually
// 3. test_instance_release_auto_repair_scenarios - Auto-repair integration scenarios
// 4. test_instance_release_repair_lifecycle - Complete repair lifecycle scenarios
// ================================================================================================

/// Tests that older clients work correctly with the enhanced API.
/// Verifies: Old API behavior preserved + NO health overrides applied.
#[crate::sqlx_test]
async fn test_instance_release_backward_compatibility(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;

    // Create a VPC segment for the test
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Create instance configuration
    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id));

    // Allocate an instance using correct API structure
    let instance_result = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
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
        .api
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
    let mut txn = env.db_txn().await;
    // Wait a moment for async cleanup to complete
    tokio::time::sleep(Duration::from_millis(100)).await;

    let host_machine = mh.host().db_machine(&mut txn).await;

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
#[crate::sqlx_test]
async fn test_instance_release_repair_tenant(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;

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

    // Create a single VPC segment to be shared across all test scenarios
    let segment_id = env.create_vpc_and_tenant_segment().await;

    for (is_repair_tenant, test_name, description) in test_scenarios {
        println!("Testing repair tenant scenario: is_repair_tenant={is_repair_tenant}");

        let mh = create_managed_host(&env).await;

        // Create instance configuration
        let config = InstanceConfig::default_tenant_and_os()
            .network(single_interface_network_config(segment_id));

        // Allocate an instance
        let instance_result = env
            .api
            .allocate_instance(
                InstanceAllocationRequest::builder(false)
                    .machine_id(mh.id)
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
            .api
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
        let mut txn = env.db_txn().await;
        tokio::time::sleep(Duration::from_millis(100)).await;

        let host_machine = mh.host().db_machine(&mut txn).await;

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
#[crate::sqlx_test]
async fn test_instance_release_combined_enhancements(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Create instance configuration
    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id));

    // Allocate an instance
    let instance_result = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
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
        .api
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
    let mut txn = env.db_txn().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let host_machine = mh.host().db_machine(&mut txn).await;

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

/// Release is rejected when aggregate health includes `PreventInstanceDeletion`; succeeds after the override is removed.
#[crate::sqlx_test]
async fn test_instance_release_rejected_when_aggregate_health_has_prevent_instance_deletion(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let mh = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id));

    let instance_result = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
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
        &mh.host().id,
        (block_release, health_report::HealthReportApplyMode::Merge),
    )
    .await;

    let err = env
        .api
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
        &mh.host().id,
        "test-prevent-instance-deletion-override".to_string(),
    )
    .await;

    env.api
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(instance_id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .expect("release should succeed after removing PreventInstanceDeletion source");
}

#[crate::sqlx_test]
async fn test_instance_release_auto_repair_enabled(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();

    // Create custom config with auto-repair ENABLED
    let mut config = get_config();
    config.auto_machine_repair_plugin.enabled = true;

    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    let mh = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id));

    // Allocate instance
    let instance_result = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
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
        .api
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
    let mut txn = env.db_txn().await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let host_machine = mh.host().db_machine(&mut txn).await;

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

// Test that if due to race condition instance creation and reprovision is started together,
// carbide must continue with instance creation, not reprovision.
#[crate::sqlx_test]
async fn test_instance_creation_when_reprovision_is_triggered_parallel(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;

    let mh = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id));

    // Step 1: Send a instance allocation request.
    let allocation_response = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(mh.id)
                .config(config)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner();

    // Step 2: Trigger DPU reprovision.
    let mut txn = env.db_txn().await;
    let machine_update = DpuMachineUpdate {
        host_machine_id: mh.id,
        dpu_machine_id: mh.dpu_ids[0],
        firmware_version: "test".to_string(),
        dpf_managed: false,
    };

    db::dpu_machine_update::trigger_reprovisioning_for_managed_host(&mut txn, &[machine_update])
        .await
        .unwrap();
    txn.commit().await.unwrap();

    advance_created_instance_into_ready_state(&env, &mh).await;

    // Step 3: Check instance state. Should be ready.
    let instance = env
        .api
        .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![allocation_response.id.unwrap()],
        }))
        .await
        .unwrap()
        .into_inner();

    assert_eq!(
        instance.instances[0]
            .clone()
            .status
            .unwrap()
            .tenant
            .unwrap()
            .state,
        rpc::forge::TenantState::Ready as i32
    );

    let reprov_machines = env
        .api
        .list_dpu_waiting_for_reprovisioning(tonic::Request::new(
            rpc::forge::DpuReprovisioningListRequest {},
        ))
        .await
        .unwrap()
        .into_inner();

    assert!(reprov_machines.dpus.is_empty());
}

#[crate::sqlx_test]
async fn test_can_not_update_instance_config_after_deletion(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

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

    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id))
        .rpc();

    let tinstance = mh
        .instance_builer(&env)
        .config(config.clone())
        .build()
        .await;

    let instance = tinstance.rpc_instance().await;
    let metadata = instance.metadata().clone();

    assert_eq!(instance.status().tenant(), rpc::forge::TenantState::Ready);

    env.api
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: tinstance.id.into(),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .unwrap();
    let instance = tinstance.rpc_instance().await;
    assert_eq!(instance.status().tenant(), rpc::TenantState::Terminating);

    let updated_os = initial_os.clone();

    // Perform an update using update_instance_operating_system
    let err = env
        .api
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
        .api
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

#[crate::sqlx_test]
async fn test_default_config_vf_enabled(_: PgPoolOptions, _options: PgConnectOptions) {
    let config = get_config();
    assert!(
        config
            .vmaas_config
            .as_ref()
            .map(|vc| vc.allow_instance_vf)
            .unwrap_or(true)
    );
}

#[crate::sqlx_test]
async fn test_instance_with_vf_when_vf_disabled(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = get_config();
    config.vmaas_config = Some(VmaasConfig {
        allow_instance_vf: false,
        hbn_reps: None,
        bridging: None,
    });

    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let managed_host = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segments(2).await;

    // Create instance configuration
    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config_with_vfs(segment_id));

    // Allocate an instance
    let instance_result = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(managed_host.id)
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test-disabled-vf".to_string(),
                    description: "Testing instance creation when vf disabled".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await;

    assert!(instance_result.is_err());
}

#[crate::sqlx_test]
async fn test_instance_without_vf_when_vf_disabled(_: PgPoolOptions, options: PgConnectOptions) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = get_config();
    config.vmaas_config = Some(VmaasConfig {
        allow_instance_vf: false,
        hbn_reps: None,
        bridging: None,
    });

    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let managed_host = create_managed_host(&env).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Create instance configuration
    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id));

    // Allocate an instance
    let instance_result = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(managed_host.id)
                .config(config)
                .metadata(rpc::Metadata {
                    name: "test-disabled-vf".to_string(),
                    description: "Testing instance creation when vf disabled".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await;

    assert!(instance_result.is_ok());
}

/// Verifies sparse DPF topology membership gates both allocation and network replacement.
#[crate::sqlx_test]
async fn test_dpf_topology_rejects_unselected_vf_on_create_and_update(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let config = crate::test_support::default_config::with_dpf_intercept_topology(&[7]);
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let managed_host = create_managed_host(&env).await;
    let segment_ids = env.create_vpc_and_tenant_segments(2).await;

    // VF0 is structurally valid and globally enabled, but the replacement topology selects VF7.
    let mut network_with_unselected_vf =
        single_interface_network_config_with_vfs(segment_ids.clone());
    network_with_unselected_vf.interfaces[1].virtual_function_id = Some(0);
    let create_error = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(managed_host.id)
                .config(
                    InstanceConfig::default_tenant_and_os()
                        .network(network_with_unselected_vf.clone()),
                )
                .tonic_request(),
        )
        .await
        .expect_err("an unselected topology VF must not be allocated");
    assert!(create_error.message().contains(
        "virtual function VF0 is not selected by the configured DPF intercept-bridging topology"
    ));

    // A PF-only instance remains valid; once ready, adding the same unselected VF must fail at
    // the network-update gate and leave no staged replacement request.
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_ids[0])
        .build()
        .await;
    let update_error = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                instance_id: instance.rpc_instance().await.rpc_id(),
                if_version_match: None,
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(network_with_unselected_vf),
                    infiniband: None,
                    network_security_group_id: None,
                    dpu_extension_services: None,
                    nvlink: None,
                    spxconfig: None,
                    power_profile: None,
                }),
                metadata: Some(rpc::forge::Metadata {
                    name: "topology-update".to_string(),
                    description: String::new(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("an unselected topology VF must not be staged");
    assert!(update_error.message().contains(
        "virtual function VF0 is not selected by the configured DPF intercept-bridging topology"
    ));

    let mut txn = env.db_txn().await;
    assert!(
        instance
            .db_instance(&mut txn)
            .await
            .update_network_config_request
            .is_none()
    );
    txn.rollback().await.unwrap();
}

/// Verifies raw protobuf VF identities cannot alias selected topology VFs during conversion.
#[crate::sqlx_test]
async fn test_public_instance_endpoints_reject_out_of_range_wire_vfs(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    // Include the truncation targets for 256 (VF0) and 263 (VF7). Without wire-range validation,
    // both malformed values would pass exact topology membership after a lossy u32-to-u8 cast.
    let config = crate::test_support::default_config::with_dpf_intercept_topology(&[0, 7, 15]);
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let create_host = create_managed_host(&env).await;
    let update_host = create_managed_host(&env).await;
    let segment_ids = env.create_vpc_and_tenant_segments(2).await;
    let update_instance = update_host
        .instance_builer(&env)
        .single_interface_network_config(segment_ids[0])
        .build()
        .await;
    let update_instance_id = update_instance.rpc_instance().await.rpc_id();

    // Build the protobuf directly so values wider than the model's u8 cannot be normalized by
    // test helpers before they reach the public handlers.
    let config_with_wire_vf = |wire_vf_id| {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        network.interfaces[1].virtual_function_id = Some(wire_vf_id);
        rpc::InstanceConfig {
            tenant: Some(default_tenant_config()),
            os: Some(default_os_config()),
            network: Some(network),
            infiniband: None,
            network_security_group_id: None,
            dpu_extension_services: None,
            nvlink: None,
            spxconfig: None,
            power_profile: None,
        }
    };
    let metadata = |operation: &str, wire_vf_id| rpc::forge::Metadata {
        name: format!("{operation}-vf{wire_vf_id}"),
        description: String::new(),
        labels: vec![],
    };

    for wire_vf_id in [16, 256, 263] {
        let create_error = env
            .api
            .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
                machine_id: Some(create_host.id.into()),
                config: Some(config_with_wire_vf(wire_vf_id)),
                instance_id: None,
                instance_type_id: None,
                metadata: Some(metadata("create", wire_vf_id)),
                allow_unhealthy_machine: false,
            }))
            .await
            .expect_err("out-of-range wire VF must be rejected during instance creation");
        assert_eq!(create_error.code(), tonic::Code::InvalidArgument);
        assert!(
            create_error
                .message()
                .contains(&format!("virtual function ID of value {wire_vf_id}"))
        );

        let update_error = env
            .api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: update_instance_id,
                if_version_match: None,
                config: Some(config_with_wire_vf(wire_vf_id)),
                metadata: Some(metadata("update", wire_vf_id)),
            }))
            .await
            .expect_err("out-of-range wire VF must be rejected during instance update");
        assert_eq!(update_error.code(), tonic::Code::InvalidArgument);
        assert!(
            update_error
                .message()
                .contains(&format!("virtual function ID of value {wire_vf_id}"))
        );
    }

    // VF15 is the inclusive boundary and must pass the same public create and update paths.
    env.api
        .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(create_host.id.into()),
            config: Some(config_with_wire_vf(15)),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(metadata("create", 15)),
            allow_unhealthy_machine: false,
        }))
        .await
        .expect("VF15 must remain valid during instance creation");
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: update_instance_id,
            if_version_match: None,
            config: Some(config_with_wire_vf(15)),
            metadata: Some(metadata("update", 15)),
        }))
        .await
        .expect("VF15 must remain valid during instance update");
}

fn create_dpu_extension_service_data(name: &str) -> String {
    format!(
        "apiVersion: v1\nkind: Pod\nmetadata:\n  name: {}\nspec:\n  containers:\n    - name: app\n      image: nginx:1.27",
        name
    )
}

#[crate::sqlx_test]
async fn test_allocate_instance_with_extension_services(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let _ = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "best_org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::Metadata {
                name: "best_org".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    // Create an extension service
    let service = env
        .api
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                service_id: None,
                service_name: "test-service".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service"),
                credential: None,
                observability: None,
            },
        ))
        .await?
        .into_inner();

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service.service_id.clone(),
                version: service
                    .latest_version_info
                    .as_ref()
                    .unwrap()
                    .version
                    .clone(),
            }],
        }),
        power_profile: None,
    };

    let _tinstance = mh
        .instance_builer(&env)
        .config(config.clone())
        .build()
        .await;

    // Verify the extension service config is correctly stored in database
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    let instance_snapshot = snapshot.instance.unwrap();

    assert_eq!(
        instance_snapshot
            .config
            .extension_services
            .service_configs
            .len(),
        1
    );
    assert_eq!(
        instance_snapshot.config.extension_services.service_configs[0].service_id,
        service.service_id.parse().unwrap()
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_allocate_instance_with_extension_services_rejected_on_dpf_host(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;
    let (service, _, _) = create_dpu_extension_services(&env).await?;

    let mut txn = env.pool.begin().await?;
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &mh.id).await?;
    txn.commit().await?;

    let result = env
        .api
        .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(mh.id.into()),
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                network_security_group_id: None,
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                        service_id: service.service_id,
                        version: service.latest_version_info.unwrap().version,
                    }],
                }),
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::Metadata {
                name: "dpf-extension-service".to_string(),
                description: String::new(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await
        .expect_err("extension services must be rejected on a DPF-managed host");

    assert_eq!(result.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        result.message(),
        format!(
            "DPU extension services are not supported on DPF-managed host {}",
            mh.id
        )
    );

    Ok(())
}

async fn create_dpu_extension_services(
    env: &TestEnv,
) -> Result<
    (
        DpuExtensionService,
        DpuExtensionService,
        DpuExtensionService,
    ),
    Box<dyn std::error::Error>,
> {
    let _ = env
        .api
        .create_tenant(tonic::Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "best_org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::Metadata {
                name: "best_org".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();

    let service1 = env
        .api
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                service_id: None,
                service_name: "test-service1".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service1-v1"),
                credential: None,
                observability: None,
            },
        ))
        .await?
        .into_inner();

    // Update the extension service with a new version
    let service1 = env
        .api
        .update_dpu_extension_service(tonic::Request::new(
            rpc::forge::UpdateDpuExtensionServiceRequest {
                service_id: service1.service_id.clone(),
                service_name: None,
                description: Some("Test service for instance".to_string()),
                data: create_dpu_extension_service_data("test-service1-v2"),
                credential: None,
                if_version_ctr_match: None,
                observability: None,
            },
        ))
        .await?
        .into_inner();

    let service2 = env
        .api
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                service_id: None,
                service_name: "test-service2".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service2-v1"),
                credential: None,
                observability: None,
            },
        ))
        .await?
        .into_inner();

    let service3 = env
        .api
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                service_id: None,
                service_name: "test-service3".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service3-v1"),
                credential: None,
                observability: None,
            },
        ))
        .await?
        .into_inner();

    Ok((service1, service2, service3))
}

#[crate::sqlx_test]
async fn test_allocate_instance_with_duplicate_extension_services(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Create extension services
    let (service1, _, _) = create_dpu_extension_services(&env).await.unwrap();

    let instance = env
        .api
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(mh.id.into()),
            config: Some(rpc::InstanceConfig {
                network_security_group_id: None,
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![
                        rpc::forge::InstanceDpuExtensionServiceConfig {
                            service_id: service1.service_id.clone(),
                            version: service1
                                .latest_version_info
                                .as_ref()
                                .unwrap()
                                .version
                                .clone(),
                        },
                        rpc::forge::InstanceDpuExtensionServiceConfig {
                            service_id: service1.service_id.clone(),
                            version: service1
                                .latest_version_info
                                .as_ref()
                                .unwrap()
                                .version
                                .clone(),
                        },
                    ],
                }),
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "newinstance".to_string(),
                description: "desc".to_string(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await;
    println!("instance: {:?}", instance);
    assert!(instance.is_err());
    let err = instance.unwrap_err();
    assert!(err.message().starts_with(
        "duplicate extension services in configuration. only one version of each service is allowed"
    ));

    Ok(())
}

#[crate::sqlx_test]
async fn test_update_instance_with_extension_services(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Create extension services
    let (service1, service2, service3) = create_dpu_extension_services(&env).await.unwrap();

    let service1_version2 = service1.active_versions[0].clone();
    let service1_version1 = service1.active_versions[1].clone();
    let service2_version = service2
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();
    let service3_version = service3
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service1.service_id.clone(),
                version: service1_version1.clone(),
            }],
        }),
        power_profile: None,
    };

    let tinstance = mh
        .instance_builer(&env)
        .config(config.clone())
        .build()
        .await;

    let instance = tinstance.rpc_instance().await.into_inner();
    assert!(
        instance
            .status
            .as_ref()
            .unwrap()
            .tenant
            .as_ref()
            .unwrap()
            .state
            == rpc::forge::TenantState::Ready as i32
    );

    let instance_id = tinstance.id;

    // Update the extension service config
    let updated_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service1.service_id.clone(),
                    version: service1_version2.clone(),
                },
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service2.service_id.clone(),
                    version: service2_version.clone(),
                },
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service3.service_id.clone(),
                    version: service3_version.clone(),
                },
            ],
        }),
        power_profile: None,
    };
    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(updated_config),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    assert!(
        instance
            .status
            .as_ref()
            .unwrap()
            .tenant
            .as_ref()
            .unwrap()
            .state
            == rpc::forge::TenantState::Configuring as i32
    );

    // The extension services config in the instance rpc response should be empty because
    // we only return active services to users.
    let extension_services_config = instance
        .config
        .unwrap()
        .dpu_extension_services
        .unwrap()
        .service_configs;
    assert_eq!(extension_services_config.len(), 3);

    // However, internally we should track all services (including terminating ones) in status
    let status = instance.status.unwrap().dpu_extension_services.unwrap();

    // We expect 4 services total:
    // - service1 v1 (terminating, was replaced by v2)
    // - service1 v2 (active)
    // - service2 v1 (active)
    // - service3 v1 (active)
    assert_eq!(
        status.dpu_extension_services.len(),
        4,
        "Status should track all 4 services (including terminating ones)"
    );

    // Verify the services exist with correct versions (order-independent check)
    let mut service_versions: Vec<(String, u64, bool)> = status
        .dpu_extension_services
        .iter()
        .map(|s| {
            let version = s.version.parse::<ConfigVersion>().unwrap();
            (
                s.service_id.clone(),
                version.version_nr(),
                s.removed.is_some(),
            )
        })
        .collect();
    service_versions.sort();

    let mut expected_versions = vec![
        (service1.service_id.clone(), 1_u64, true),
        (service1.service_id.clone(), 2_u64, false),
        (service2.service_id.clone(), 1_u64, false),
        (service3.service_id.clone(), 1_u64, false),
    ];
    expected_versions.sort();

    assert_eq!(
        service_versions, expected_versions,
        "All service versions should be tracked in status"
    );

    // Update the extension service config with no services
    let updated_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![],
        }),
        power_profile: None,
    };
    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(updated_config),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .unwrap()
        .into_inner();

    // The extension services config in the instance rpc response should be empty because
    // we only return active services to users.
    let extension_services_config = instance.config.unwrap().dpu_extension_services;
    assert!(extension_services_config.is_none());

    // However, internally we should track all services (including terminating ones) in status
    let status = instance.status.unwrap().dpu_extension_services.unwrap();

    // We expect 4 services total:
    // - service1 v1 (terminating, was replaced by v2)
    // - service1 v2 (terminating, being removed)
    // - service2 v1 (terminating, being removed)
    // - service3 v1 (terminating, being removed)
    assert_eq!(
        status.dpu_extension_services.len(),
        4,
        "Status should track all 4 services (including terminating ones)"
    );

    // Verify the services exist with correct versions (order-independent check)
    let mut service_versions: Vec<(String, u64, bool)> = status
        .dpu_extension_services
        .iter()
        .map(|s| {
            let version = s.version.parse::<ConfigVersion>().unwrap();
            (
                s.service_id.clone(),
                version.version_nr(),
                s.removed.is_some(),
            )
        })
        .collect();
    service_versions.sort();

    let mut expected_versions = vec![
        (service1.service_id.clone(), 1_u64, true),
        (service1.service_id.clone(), 2_u64, true),
        (service2.service_id.clone(), 1_u64, true),
        (service3.service_id.clone(), 1_u64, true),
    ];
    expected_versions.sort();

    assert_eq!(
        service_versions, expected_versions,
        "All service versions should be tracked in status"
    );

    // Update the extension service config with non-existing service version, expect error
    // Update the extension service config with fewer new service
    let updated_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service1.service_id.clone(),
                version: service3_version.clone(),
            }],
        }),
        power_profile: None,
    };
    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(updated_config),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await;
    assert!(instance.is_err());
    let err = instance.unwrap_err();
    assert!(err.to_string().contains("does not exist or is deleted"));

    // Update the extension service config with duplicate service ID, expect error
    let updated_config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service1.service_id.clone(),
                    version: service1_version1.clone(),
                },
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service1.service_id.clone(),
                    version: service1_version2.clone(),
                },
            ],
        }),
        power_profile: None,
    };
    let instance = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
                if_version_match: None,
                config: Some(updated_config),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await;
    assert!(instance.is_err());
    let err = instance.unwrap_err();
    assert!(err.message().starts_with(
        "duplicate extension services in configuration. only one version of each service is allowed"
    ));

    Ok(())
}

#[crate::sqlx_test]
async fn test_attach_extension_service_rejected_on_dpf_host(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;
    let (service, _, _) = create_dpu_extension_services(&env).await?;
    let tinstance = mh
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    let mut txn = env.pool.begin().await?;
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &mh.id).await?;
    txn.commit().await?;

    let result = env
        .api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(tinstance.id),
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                network_security_group_id: None,
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                        service_id: service.service_id,
                        version: service.latest_version_info.unwrap().version,
                    }],
                }),
                power_profile: None,
            }),
            metadata: Some(rpc::Metadata {
                name: "dpf-extension-service".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .expect_err("attaching an extension service must fail on a DPF-managed host");

    assert_eq!(result.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        result.message(),
        format!(
            "DPU extension services are not supported on DPF-managed host {}",
            mh.id
        )
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_service_removed_after_all_dpus_report_terminated(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let (_, service2, _) = create_dpu_extension_services(&env).await?;
    let service2_version = service2
        .latest_version_info
        .as_ref()
        .unwrap()
        .version
        .clone();

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service2.service_id.clone(),
                version: service2_version,
            }],
        }),
        power_profile: None,
    };

    let tinstance = mh.instance_builer(&env).config(config).build().await;
    let instance_id = tinstance.id;

    // Explicitly mock healthy/running extension-service status from the DPU.
    network_configured_with_health_and_ext_services(&env, &mh.dpu_ids[0], None, None).await;

    // Remove all extension services from desired config.
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                network_security_group_id: None,
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![],
                }),
                power_profile: None,
            }),
            instance_id: Some(instance_id),
            metadata: Some(rpc::forge::Metadata {
                name: "newinstance".to_string(),
                description: "desc".to_string(),
                labels: vec![],
            }),
        }))
        .await?;

    // Update instance config should not change the instance state from Ready
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await;

    let rpc_instance = tinstance.rpc_instance().await.into_inner();

    // Since the extension services are removed from the instance config, the config should be empty.
    assert!(
        rpc_instance
            .config
            .unwrap()
            .dpu_extension_services
            .is_none()
    );

    // At this point, since DPUs have not reported any extension services, the tenant state should
    // be in Configuring state.
    let rpc_status = rpc_instance.status.unwrap();
    assert_eq!(
        rpc_status.tenant.unwrap().state,
        rpc::TenantState::Configuring as i32
    );

    // The extension services status should still be tracked until fully terminated.
    let dpu_extension_services_status = rpc_status.dpu_extension_services.unwrap();
    assert_eq!(
        dpu_extension_services_status.dpu_extension_services.len(),
        1
    );
    assert_eq!(
        dpu_extension_services_status.configs_synced,
        rpc::forge::SyncState::Pending as i32
    );
    // The status should be Unknown until the DPU reports the status.
    assert_eq!(
        dpu_extension_services_status.dpu_extension_services[0].deployment_status,
        rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceUnknown as i32
    );

    // Mock DPU reporting removed services as fully terminated.
    // Instance should be in Ready state after this.
    // Tenant state should be Ready.
    // Instance config and status should no long have the extension services.
    network_configured_with_health_and_ext_services(
        &env,
        &mh.dpu_ids[0],
        None,
        Some(rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminated),
    )
    .await;

    // Let state handler process cleanup and persist instance extension-services config.
    env.run_machine_state_controller_iteration().await;

    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    let instance_snapshot = snapshot.instance.unwrap();

    // The extension services should be removed from the instance config.
    assert!(
        instance_snapshot
            .config
            .extension_services
            .service_configs
            .is_empty(),
        "Instance config should not have extension services"
    );

    // However, the observations should still be in record.
    assert!(!instance_snapshot.observations.extension_services.is_empty(),);

    let rpc_instance = tinstance.rpc_instance().await.into_inner();
    assert!(
        rpc_instance
            .config
            .unwrap()
            .dpu_extension_services
            .is_none()
    );

    // The tenant status should now be Ready.
    let rpc_status = rpc_instance.status.unwrap();
    assert_eq!(
        rpc_status.tenant.unwrap().state,
        rpc::TenantState::Ready as i32
    );
    assert!(
        rpc_status
            .dpu_extension_services
            .unwrap()
            .dpu_extension_services
            .is_empty()
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_extension_services_status_observation(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Create an extension service
    let (service1, _, _) = create_dpu_extension_services(&env).await.unwrap();
    let versions = service1
        .active_versions
        .iter()
        .map(|v| v.parse::<ConfigVersion>().unwrap())
        .collect::<Vec<_>>();

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        nvlink: None,
        spxconfig: None,
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service1.service_id.clone(),
                version: versions[0].version_string(),
            }],
        }),
        power_profile: None,
    };

    let tinstance = mh
        .instance_builer(&env)
        .config(config.clone())
        .build()
        .await;

    // Verify the status is correctly updated
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    let instance_snapshot = snapshot.instance.unwrap();

    // Check that the observation was stored
    assert_eq!(instance_snapshot.observations.extension_services.len(), 1,);

    let dpu_observation = instance_snapshot
        .observations
        .extension_services
        .get(&mh.dpu_ids[0])
        .and_then(|observations| {
            observations
                .for_service_type(model::extension_service::ExtensionServiceType::KubernetesPod)
        })
        .unwrap();

    assert_eq!(
        dpu_observation.config_version,
        instance_snapshot.extension_services_config_version,
    );

    assert_eq!(dpu_observation.extension_service_statuses.len(), 1,);

    let service_status = &dpu_observation.extension_service_statuses[0];
    assert_eq!(
        service_status.service_id.to_string(),
        service1.service_id.clone()
    );
    assert_eq!(service_status.version, versions[0].clone());
    assert_eq!(
        service_status.overall_state,
        model::instance::status::extension_service::ExtensionServiceDeploymentStatus::Running
    );

    // Now verify the RPC instance status
    let instance = tinstance.rpc_instance().await.into_inner();
    let ext_status = instance
        .status
        .as_ref()
        .unwrap()
        .dpu_extension_services
        .as_ref()
        .unwrap();

    // Since we have matching config version observation, status should be synced
    assert_eq!(
        ext_status.configs_synced,
        rpc::forge::SyncState::Synced as i32
    );

    // Verify the service status
    assert_eq!(ext_status.dpu_extension_services.len(), 1,);

    let service_status = &ext_status.dpu_extension_services[0];
    assert_eq!(service_status.service_id, service1.service_id.clone());
    assert_eq!(service_status.version, versions[0].to_string());
    assert_eq!(
        service_status.deployment_status,
        rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32,
    );

    // Verify DPU status details
    assert_eq!(service_status.dpu_statuses.len(), 1,);

    let dpu_status = &service_status.dpu_statuses[0];
    assert_eq!(dpu_status.dpu_machine_id, Some(mh.dpu().id.into()));
    assert_eq!(
        dpu_status.status,
        rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceRunning as i32,
    );

    Ok(())
}

/// Allocate instance with non-existent OS image ID.
/// Expect: FailedPrecondition error indicating image does not exist.
#[crate::sqlx_test]
async fn test_allocate_instance_with_invalid_os_image(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Use a non-existent OS image ID
    let invalid_os_image_id = uuid::Uuid::new_v4();

    let os_config = rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: None,
        variant: Some(
            rpc::forge::instance_operating_system_config::Variant::OsImageId(rpc::Uuid::from(
                invalid_os_image_id,
            )),
        ),
    };

    let result = env
        .api
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(mh.id.into()),
            config: Some(rpc::InstanceConfig {
                network_security_group_id: None,
                tenant: Some(default_tenant_config()),
                os: Some(os_config),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: None,
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: None,
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "test-invalid-os-image".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.message().contains("does not exist"),
        "Expected error about OS image not existing, got: {}",
        err.message()
    );

    Ok(())
}

/// Allocate instance with non-existent IB partition ID.
/// Expect: InvalidArgument error indicating partition is not created.
#[crate::sqlx_test]
async fn test_allocate_instance_with_invalid_ib_partition(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    // Use a non-existent IB partition ID
    let invalid_partition_id = carbide_uuid::infiniband::IBPartitionId::new();

    let ib_config = rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: vec![rpc::forge::InstanceIbInterfaceConfig {
            function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
            virtual_function_id: None,
            ib_partition_id: Some(invalid_partition_id),
            device: "MT2910 Family [ConnectX-7]".to_string(),
            vendor: None,
            device_instance: 0,
        }],
    };

    let result = env
        .api
        .allocate_instance(tonic::Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(mh.id.into()),
            config: Some(rpc::InstanceConfig {
                network_security_group_id: None,
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                infiniband: Some(ib_config),
                nvlink: None,
                spxconfig: None,
                dpu_extension_services: None,
                power_profile: None,
            }),
            instance_id: None,
            instance_type_id: None,
            metadata: Some(rpc::forge::Metadata {
                name: "test-invalid-ib-partition".to_string(),
                description: "".to_string(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await;

    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.message().contains("IB partition") || err.message().contains("not created"),
        "Expected error about IB partition not existing, got: {}",
        err.message()
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_can_not_create_instances_with_machine_in_quarantine(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let (host_machine_id, _dpu_machine_id) = create_managed_host(&env).await.into();

    let config = InstanceConfig::default_tenant_and_os()
        .network(single_interface_network_config(segment_id))
        .rpc();

    let instance_id: InstanceId = uuid::Uuid::new_v4().into();

    env.api
        .set_managed_host_quarantine_state(tonic::Request::new(
            rpc::forge::SetManagedHostQuarantineStateRequest {
                machine_id: Some(host_machine_id.into()),
                quarantine_state: Some(rpc::forge::ManagedHostQuarantineState {
                    mode: ManagedHostQuarantineMode::BlockAllTraffic as i32,
                    reason: Some("test".to_string()),
                }),
            },
        ))
        .await
        .unwrap();

    let result = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .instance_id(instance_id)
                .machine_id(host_machine_id)
                .config(config.clone())
                .metadata(rpc::Metadata {
                    name: "test_instance".to_string(),
                    description: "tests/instance".to_string(),
                    labels: Vec::new(),
                })
                .tonic_request(),
        )
        .await;

    // TODO: Do not leak the full database error to users
    let err = result.expect_err("Expect instance creation to fail");
    assert!(
        err.message()
            .contains("host is not available for allocation due to health probe alert")
    );
}

/// With the site flag on, tenant release drives the host through the full
/// `FactoryResetBmc` sub-flow before the existing power-cycle path: it suppresses
/// site-explorer, waits for the acknowledgement, factory-resets and re-probes the
/// BMC, verifies the factory credentials, restores the device to its previous
/// per-device Vault credential (read-only), removes the suppression, and hands
/// off to `PowerCycle`. Asserts the suppression is cleaned up, the per-device
/// Vault secret is preserved (never written), and the BMC ends on the restored
/// per-device password.
#[crate::sqlx_test]
async fn test_factory_reset_bmc_on_release_sanitizes_host_bmc(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    use carbide_secrets::credentials::{
        BmcCredentialType, CredentialKey, CredentialReader, CredentialWriter, Credentials,
    };
    use model::bmc_suppression::BmcSuppressionSubsystem;
    use model::machine::HostPlatformConfigurationState;

    const FACTORY_USER: &str = "root";
    const FACTORY_PW: &str = "factory-default";
    const PER_DEVICE_PW: &str = "prev-per-device";
    const SITE_EXPLORER: BmcSuppressionSubsystem = BmcSuppressionSubsystem::SiteExplorer;

    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();

    // Opt the site in to the deletion-only BMC factory-reset sub-flow.
    let mut config = get_config();
    config.bmc_factory_reset_on_instance_termination_enabled = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;

    let segment_ids = env.create_vpc_and_tenant_segments(1).await;
    let mh = create_managed_host_multi_dpu(&env, 1).await;

    let mut txn = env.db_txn().await;
    let host_machine = mh.host().db_machine(&mut txn).await;
    let device_locators: Vec<DeviceLocator> = mh
        .dpu_ids
        .iter()
        .map(|dpu_id| {
            host_machine
                .get_device_locator_for_dpu_id(dpu_id)
                .expect("device locator for DPU")
        })
        .collect();
    let host_bmc_mac = host_machine
        .status
        .bmc_info
        .mac
        .expect("fixture host should have a BMC MAC");
    txn.commit().await.unwrap();

    // Allocate and drive the instance to Assigned/Ready.
    let tinstance = mh
        .instance_builer(&env)
        .network(interface_network_config_with_devices(
            &segment_ids,
            &device_locators,
        ))
        .build()
        .await;

    // Seed the state the sub-flow depends on, after provisioning so nothing
    // overwrites it: non-empty factory credentials in expected_machines (the
    // recovery path for the factory password), the sim's factory `root` account,
    // and the device's previous per-device Vault credential (the restore target).
    let per_device_key = CredentialKey::BmcCredentials {
        credential_type: BmcCredentialType::BmcRoot {
            bmc_mac_address: host_bmc_mac,
        },
    };
    let per_device_creds = Credentials::UsernamePassword {
        username: FACTORY_USER.to_string(),
        password: PER_DEVICE_PW.to_string(),
    };
    {
        let mut expected = db::expected_machine::find_by_bmc_mac_address(&env.pool, host_bmc_mac)
            .await
            .unwrap()
            .expect("expected_machines row for the fixture host");
        let mut txn = env.pool.begin().await.unwrap();
        db::expected_machine::update_bmc_credentials(
            &mut expected,
            txn.as_mut(),
            FACTORY_USER.to_string(),
            FACTORY_PW.to_string(),
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
    }
    env.redfish_sim.seed_user(FACTORY_USER, FACTORY_PW);
    env.test_credential_manager
        .set_credentials(&per_device_key, &per_device_creds)
        .await
        .expect("staging the per-device Vault credential should succeed");

    // Tenant release: mark the instance deleted.
    env.api
        .release_instance(tonic::Request::new(InstanceReleaseRequest {
            id: Some(tinstance.id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .expect("release_instance failed");

    // Drive the sub-flow one iteration at a time, acknowledging the suppression
    // once it appears (site-explorer does not run in this env), and stop as soon
    // as the machine advances past FactoryResetBmc.
    let mut acknowledged = false;
    let mut entered_factory_reset = false;
    let mut state_after_factory_reset = None;
    for _ in 0..60 {
        env.run_machine_state_controller_iteration().await;

        if !acknowledged
            && let Some(suppression) =
                db::bmc_suppression::find(&env.pool, host_bmc_mac, SITE_EXPLORER)
                    .await
                    .unwrap()
            && suppression.acknowledged_at.is_none()
        {
            let mut txn = env.pool.begin().await.unwrap();
            db::bmc_suppression::acknowledge(txn.as_mut(), host_bmc_mac, SITE_EXPLORER)
                .await
                .unwrap();
            txn.commit().await.unwrap();
            acknowledged = true;
        }

        let mut txn = env.pool.begin().await.unwrap();
        let machine = mh.host().db_machine(&mut txn).await;
        let state = machine.current_state().clone();
        txn.commit().await.unwrap();

        // WaitForBmc holds a fixed 30s wall-clock settle window before its first
        // readiness probe. Backdate the state version timestamp (preserving the
        // version number so the next transition's optimistic check still passes)
        // so the settle elapses and the always-up sim BMC is probed without a
        // real 30s wait.
        if matches!(
            state,
            ManagedHostState::Assigned {
                instance_state: InstanceState::HostPlatformConfiguration {
                    platform_config_state: HostPlatformConfigurationState::FactoryResetBmc {
                        reset_state: model::machine::FactoryResetBmcState::WaitForBmc,
                    },
                },
            }
        ) {
            let aged_version = format!(
                "V{}-T{}",
                machine.current_version().version_nr(),
                (chrono::Utc::now() - chrono::Duration::seconds(120)).timestamp_micros()
            );
            let mut txn = env.pool.begin().await.unwrap();
            sqlx::query("UPDATE machines SET controller_state_version=$1 WHERE id=$2")
                .bind(aged_version)
                .bind(machine.id.to_string())
                .execute(txn.as_mut())
                .await
                .unwrap();
            txn.commit().await.unwrap();
        }

        if matches!(
            state,
            ManagedHostState::Assigned {
                instance_state: InstanceState::HostPlatformConfiguration {
                    platform_config_state: HostPlatformConfigurationState::FactoryResetBmc { .. },
                },
            }
        ) {
            entered_factory_reset = true;
        } else if entered_factory_reset {
            state_after_factory_reset = Some(state);
            break;
        }
    }

    assert!(
        entered_factory_reset,
        "tenant release with the flag on must enter FactoryResetBmc"
    );
    assert!(
        acknowledged,
        "the sub-flow must upsert a site-explorer suppression to acknowledge"
    );
    let state_after_factory_reset =
        state_after_factory_reset.expect("machine must advance past FactoryResetBmc");
    assert!(
        matches!(
            state_after_factory_reset,
            ManagedHostState::Assigned {
                instance_state: InstanceState::HostPlatformConfiguration {
                    platform_config_state: HostPlatformConfigurationState::PowerCycle { .. },
                },
            }
        ),
        "FactoryResetBmc must hand off to PowerCycle, got {state_after_factory_reset:?}"
    );

    // RemoveSuppression deletes the row it created.
    assert!(
        db::bmc_suppression::find(&env.pool, host_bmc_mac, SITE_EXPLORER)
            .await
            .unwrap()
            .is_none(),
        "RemoveSuppression must delete the site-explorer suppression"
    );

    // The per-device Vault secret is preserved verbatim: this flow only reads it.
    let persisted = env
        .test_credential_manager
        .get_credentials(&per_device_key)
        .await
        .expect("reading the per-device Vault credential should succeed")
        .expect("the per-device Vault credential must still be present");
    assert_eq!(
        persisted, per_device_creds,
        "the per-device Vault credential must be preserved (read-only restore target)"
    );

    // RestoreCredentials changed the BMC root password from the factory default
    // back to the device's previous per-device password.
    assert_eq!(
        env.redfish_sim.user_password(FACTORY_USER).as_deref(),
        Some(PER_DEVICE_PW),
        "the BMC root account must end on the restored per-device password"
    );
}
