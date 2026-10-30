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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, SystemTime};

use ::rpc::forge::forge_server::Forge;
use carbide_dpf::{DpfError, DpuDeploymentType};
use carbide_machine_controller::dpf::{DpfOperations, MockDpfOperations};
use carbide_network::virtualization::VpcVirtualizationType;
use carbide_redfish::libredfish::test_support::RedfishSimAction;
use carbide_uuid::extension_service::ExtensionServiceId;
use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::{DpuMachineId, StableHostMachineId};
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::network::{NetworkPrefixId, NetworkSegmentId};
use carbide_uuid::vpc::{VpcId, VpcPrefixId};
use common::api_fixtures::instance::{
    advance_created_instance_into_ready_state, default_os_config, default_tenant_config,
    interface_network_config_with_devices, seed_service_vpc_endpoint,
    single_interface_network_config, single_interface_network_config_with_vpc_prefix,
};
use common::api_fixtures::tenant::create_fixture_tenant;
use common::api_fixtures::tpm_attestation::{CA_CERT_SERIALIZED, EK_CERT_SERIALIZED};
use common::api_fixtures::{
    TestEnvOverrides, create_managed_host, create_managed_host_with_config, create_test_env,
    create_test_env_with_overrides, get_config, inject_machine_measurements,
    network_configured_with_health, network_configured_with_health_and_ext_services,
    persist_machine_validation_result, populate_network_security_groups, site_explorer,
};
use config_version::ConfigVersion;
use db::instance_address::UsedOverlayNetworkIpResolver;
use db::ip_allocator::UsedIpResolver;
use db::network_segment::IdColumn;
use db::{self, ObjectColumnFilter};
use futures_util::future::join_all;
use ipnetwork::{IpNetwork, Ipv4Network, Ipv6Network};
use itertools::Itertools;
use mac_address::MacAddress;
use model::controller_outcome::PersistentStateHandlerOutcome;
use model::dpu_machine_update::DpuMachineUpdate;
use model::expected_machine::{ExpectedInterface, ExpectedMachineData};
use model::extension_service::{ExtensionServiceLifecycleState, ExtensionServiceType};
use model::instance::config::extension_services::{
    InstanceExtensionServiceConfig, InstanceExtensionServicesConfig,
    RequestedInstanceExtensionServiceConfig, RequestedInstanceExtensionServicesConfig,
};
use model::instance::config::network::{
    DeviceLocator, InstanceInterfaceIpFamilyMode, InstanceInterfaceVpcSelection,
    InstanceNetworkConfig, InstanceServiceInterfaceConfig, InterfaceFunctionId,
    Ipv6InterfaceConfig, NetworkDetails,
};
use model::instance::snapshot::InstanceSnapshot;
use model::machine::{
    AttestationMode, CleanupContext, CleanupState, FailureDetails, HostMachine,
    HostPlatformConfigurationState, InstanceState, MachineState, MachineValidatingState,
    ManagedHostState, ManagedHostStateSnapshot, MeasuringState, NetworkConfigUpdateState,
    SpdmMeasuringState, ValidationState,
};
use model::metadata::Metadata;
use model::network_prefix::NewNetworkPrefix;
use model::network_segment::{
    NetworkSegmentControllerState, NetworkSegmentSearchConfig, NetworkSegmentType,
    NewNetworkSegment,
};
use model::resource_pool::{OwnerType, ResourcePoolEntryState};
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
use crate::instance::allocate_network;
use crate::network_segment::allocate::PrefixAllocator;
use crate::test_support::fixture_config::{FixtureDefault as _, ManagedHostConfigExt as _};
use crate::test_support::metadata;
use crate::test_support::network_segment::{
    FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS, FIXTURE_TENANT_ORG_ID, create_tenant_network_segment,
};
use crate::tests::common;
use crate::tests::common::api_fixtures::instance::single_interface_network_config_with_vfs;
use crate::tests::common::api_fixtures::rpc_instance::RpcInstance;
use crate::tests::common::api_fixtures::{
    TestEnv, TestManagedHost, create_managed_host_multi_dpu, create_managed_host_with_ek,
    remove_health_report_entry, send_health_report_entry, update_time_params,
};
use crate::tests::common::attestation::spdm_attestation_run_to_failed_then_to_success;
use crate::tests::common::postgres::wait_for_blocked_query;
use crate::tests::common::rpc_builder::{
    InstanceAllocationRequest, InstanceConfig, InstanceConfigExt, VpcCreationRequest,
};

/// Aborts a spawned service-VPC race task if a failed lock assertion unwinds the test.
struct TestTaskHandle<T>(tokio::task::JoinHandle<T>);

impl<T> TestTaskHandle<T> {
    /// Stores a spawned task so dropping the test-local handle cannot detach database work.
    fn new(task: tokio::task::JoinHandle<T>) -> Self {
        Self(task)
    }

    /// Joins the stored task while retaining abort-on-drop behavior during cancellation.
    async fn join(mut self) -> Result<T, tokio::task::JoinError> {
        (&mut self.0).await
    }
}

impl<T> Drop for TestTaskHandle<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Bounds a spawned service-VPC race task so a lock-order regression fails instead of hanging.
async fn join_test_task<T>(task: TestTaskHandle<T>, description: &str) -> T {
    match tokio::time::timeout(std::time::Duration::from_secs(30), task.join()).await {
        Ok(result) => result.unwrap_or_else(|error| panic!("{description} task failed: {error}")),
        Err(_) => panic!("{description} did not finish within 30 seconds"),
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

async fn test_measurement_assigned_ready_to_waiting_for_measurements_to_ca_failed_to_ready_body(
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
            machine_id: Some(mh.host().id),
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

/// Verifies ordinary release atomically begins attachment termination, retries idempotently, and
/// waits before deleting the Instance while its attachment and service-interface records remain for cleanup.
#[crate::sqlx_test]
async fn test_release_waits_for_service_vpc_endpoint_cleanup(pool: sqlx::PgPool) {
    // Create a Ready Instance whose attachment and service-interface records must outlive host teardown.
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let endpoint =
        seed_service_vpc_endpoint(&env, instance.id, managed_host.dpu_ids[0], segment_id).await;
    let before_release = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .unwrap()
        .expect("instance exists before release");

    // Enter the real release workflow without running background cleanup yet.
    env.api
        .release_instance(Request::new(InstanceReleaseRequest {
            id: Some(instance.id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .unwrap();

    // Reload after release to verify `removed` and both configuration-version increments.
    let after_release = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .unwrap()
        .expect("terminating instance remains stored");
    let removal_time = after_release
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|attachment| attachment.id == Some(endpoint.attachment_id))
        .and_then(|attachment| attachment.removed)
        .expect("release marks the endpoint owner terminating");
    assert_eq!(
        after_release.extension_services_config_version.version_nr(),
        before_release
            .extension_services_config_version
            .version_nr()
            + 1
    );
    assert_eq!(
        after_release.network_config_version.version_nr(),
        before_release.network_config_version.version_nr() + 1
    );
    assert_eq!(
        after_release.config.network.service_interfaces,
        vec![endpoint.clone()]
    );

    // Repeat release to prove the retry preserves `removed`, `service_interfaces`, and both versions.
    env.api
        .release_instance(Request::new(InstanceReleaseRequest {
            id: Some(instance.id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await
        .unwrap();
    let after_retry = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .unwrap()
        .expect("terminating instance remains stored after retry");
    assert_eq!(
        after_retry.extension_services_config_version,
        after_release.extension_services_config_version
    );
    assert_eq!(
        after_retry.network_config_version,
        after_release.network_config_version
    );
    assert_eq!(
        after_retry.config.network.service_interfaces,
        vec![endpoint.clone()]
    );
    assert_eq!(
        after_retry
            .config
            .extension_services
            .service_configs
            .iter()
            .find(|attachment| attachment.id == Some(endpoint.attachment_id))
            .and_then(|attachment| attachment.removed),
        Some(removal_time)
    );

    // Drive background release to the final Admin-network wait after termination is durable.
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        8,
        ManagedHostState::Assigned {
            instance_state: InstanceState::HostPlatformConfiguration {
                platform_config_state:
                    model::machine::HostPlatformConfigurationState::CheckHostConfig,
            },
        },
    )
    .await;
    managed_host.network_configured(&env).await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        2,
        ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForDpusToUp,
        },
    )
    .await;
    managed_host.network_configured(&env).await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        1,
        ManagedHostState::Assigned {
            instance_state: InstanceState::BootingWithDiscoveryImage {
                retry: model::machine::RetryInfo { count: 0 },
            },
        },
    )
    .await;
    // Simulate the requested reboot so release advances from
    // `BootingWithDiscoveryImage` to `WaitingForNetworkReconfig`.
    let mut txn = env.db_txn().await;
    let host = managed_host.host().db_machine(&mut txn).await;
    db::machine::update_reboot_time(&host, &mut txn)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let waiting = ManagedHostState::Assigned {
        instance_state: InstanceState::WaitingForNetworkReconfig,
    };
    env.run_machine_state_controller_iteration_until_state_matches(
        &managed_host.host().id,
        2,
        waiting.clone(),
    )
    .await;
    network_configured_with_health_and_ext_services(
        &env,
        &managed_host.dpu_ids[0],
        None,
        Some(rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminated),
    )
    .await;

    // With placement already terminated, only the endpoint guard can own this wait.
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    let snapshot = managed_host.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, waiting);
    assert!(
        matches!(
            &snapshot.host_snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Wait { reason, .. }) if reason
                == "Waiting for service VPC network cleanup before deleting the instance."
        ),
        "expected the service-network cleanup wait, got {:?}",
        snapshot.host_snapshot.controller_state_outcome
    );
    let persisted = snapshot
        .instance
        .expect("terminating instance remains assigned");
    assert!(persisted.deleted.is_some());
    assert_eq!(
        persisted
            .config
            .extension_services
            .service_configs
            .iter()
            .find(|attachment| attachment.id == Some(endpoint.attachment_id))
            .and_then(|attachment| attachment.removed),
        Some(removal_time)
    );
    assert_eq!(persisted.config.network.service_interfaces, vec![endpoint]);
    txn.commit().await.unwrap();
}

/// Verifies public whole-instance release marks every attachment retaining endpoints in one write
/// while preserving an earlier removal timestamp, both service interfaces, generated network
/// segments, and Instance addresses for observed cleanup.
#[crate::sqlx_test]
async fn test_release_terminates_multiple_service_vpc_owners_atomically(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create two valid service attachments on distinct VPCs and slots of one Ready DPF instance.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, host_segment_id) = create_fnn_host_segment(
        &env,
        "multi-owner release host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let host = create_service_vpc_test_host(&env).await?;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let first_vpc_id = create_fnn_vpc(&env, "multi-owner release service VPC 1").await;
    let second_vpc_id = create_fnn_vpc(&env, "multi-owner release service VPC 2").await;
    let first_service = create_networked_dpf_service(&env, "multi-owner-release-1").await?;
    let second_service = create_networked_dpf_service(&env, "multi-owner-release-2").await?;
    mark_networked_dpf_service_ready(&env, first_service.service_id.parse()?).await?;
    mark_networked_dpf_service_ready(&env, second_service.service_id.parse()?).await?;
    let first_endpoint = seed_networked_attachment_with_prefix(
        &env,
        instance.id,
        host.dpu_ids[0],
        &first_service,
        first_vpc_id,
        0,
        "multi-owner release prefix 1",
        "10.226.0.0/30".parse()?,
    )
    .await?;
    let second_endpoint = seed_networked_attachment_with_prefix(
        &env,
        instance.id,
        host.dpu_ids[0],
        &second_service,
        second_vpc_id,
        1,
        "multi-owner release prefix 2",
        "10.227.0.0/30".parse()?,
    )
    .await?;

    // Mark the second attachment terminating before release so timestamp preservation is observable.
    let earlier_removal = chrono::Utc::now() - chrono::Duration::minutes(1);
    let stored = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("multi-owner instance remains stored");
    let mut partially_terminating = stored.config.extension_services.clone();
    partially_terminating
        .service_configs
        .iter_mut()
        .find(|attachment| attachment.id == Some(second_endpoint.attachment_id))
        .expect("second endpoint has an attachment owner")
        .removed = Some(earlier_removal);
    let mut termination_txn = env.db_txn().await;
    assert!(matches!(
        db::instance::update_service_network_config(
            termination_txn.as_mut(),
            stored.id,
            stored.extension_services_config_version,
            &stored.config.extension_services,
            &partially_terminating,
            stored.network_config_version,
            &stored.config.network,
            &stored.config.network,
            true,
        )
        .await?,
        db::ConditionalWrite::Applied(_)
    ));
    termination_txn.commit().await?;
    let before_release = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("partially terminating instance remains stored");

    // Invoke public release once so all still-active attachments with endpoints change together.
    env.api
        .release_instance(Request::new(InstanceReleaseRequest {
            id: Some(instance.id),
            issue: None,
            is_repair_tenant: None,
            delete_attribution: None,
        }))
        .await?;

    // A fresh Instance read proves one new `removed` value, one preserved value, and one increment
    // each to `extension_services_config_version` and `network_config_version`.
    let after_release = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("released instance remains stored for cleanup");
    let first_removal = after_release
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|attachment| attachment.id == Some(first_endpoint.attachment_id))
        .and_then(|attachment| attachment.removed)
        .expect("active endpoint owner becomes terminating");
    assert!(first_removal > earlier_removal);
    assert_eq!(
        after_release
            .config
            .extension_services
            .service_configs
            .iter()
            .find(|attachment| attachment.id == Some(second_endpoint.attachment_id))
            .and_then(|attachment| attachment.removed),
        Some(earlier_removal)
    );
    assert_eq!(
        after_release.extension_services_config_version.version_nr(),
        before_release
            .extension_services_config_version
            .version_nr()
            + 1
    );
    assert_eq!(
        after_release.network_config_version.version_nr(),
        before_release.network_config_version.version_nr() + 1
    );
    assert_eq!(
        after_release.config.network.service_interfaces,
        vec![first_endpoint.clone(), second_endpoint.clone()]
    );

    // Both generated segments and normalized addresses remain until external cleanup is observed.
    for endpoint in [first_endpoint, second_endpoint] {
        let segments = db::network_segment::find_by(
            &mut *env.pool.acquire().await?,
            ObjectColumnFilter::One(IdColumn, &endpoint.network_segment_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?;
        assert_eq!(segments.len(), 1);
        let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            &mut *env.pool.acquire().await?,
            &instance.id,
            &endpoint.network_segment_id,
        )
        .await?;
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].prefix, endpoint.link_prefix);
    }

    Ok(())
}

#[test]
fn test_measurement_assigned_ready_to_waiting_for_measurements_to_ca_failed_to_ready() {
    let mut args = ::sqlx::testing::TestArgs::new(concat!(
        module_path!(),
        "::test_measurement_assigned_ready_to_waiting_for_measurements_to_ca_failed_to_ready"
    ));
    args.fixtures(Box::leak(Box::new(vec![])));

    let test_fn: fn(PgPoolOptions, PgConnectOptions) -> _ =
        test_measurement_assigned_ready_to_waiting_for_measurements_to_ca_failed_to_ready_body;

    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(move || sqlx_testing::TestFn::run_test(test_fn, args))
        .expect("failed to spawn measurement test thread")
        .join()
        .expect("measurement test thread panicked");
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

    assert_eq!(instance_matched_by_label.machine_id.unwrap(), mh.host().id);

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
                dpu_machine_id: Some(mh.dpu().id),
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
                dpu_machine_id: Some(mh.dpu().id),
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

    let err = result.expect_err("Expect instance creation to fail");
    assert_eq!(err.code(), tonic::Code::Internal);
    assert_eq!(
        err.message(),
        "internal database error: duplicate key value violates unique constraint"
    );
    assert_eq!(
        err.metadata().get("nico-error-text").unwrap(),
        "internal database error: duplicate key value violates unique constraint"
    );
    let source = std::error::Error::source(&err)
        .unwrap()
        .downcast_ref::<crate::errors::CarbideErrorWithLocation>()
        .unwrap();
    assert_eq!(source.error.pg_sqlstate().as_deref(), Some("23505"));
    assert!(!source.error.is_retryable_transaction_error());
    let crate::errors::CarbideError::DBError(annotated) = &source.error else {
        panic!("expected the original database error: {source:?}");
    };
    let sqlx::Error::Database(database_error) = &annotated.source else {
        panic!("expected a database constraint error: {annotated:?}");
    };
    assert_eq!(database_error.constraint(), Some("instances_pkey"));
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
                dpu_machine_id: Some(mh.dpu().id),
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
    allocate_network(&mut config.network, &mut txn)
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
async fn initial_network_wait_keeps_admin_until_tenant_configuration_is_ready(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = get_config();
    config.dpu_config.restart_ovs_on_use_admin_network_change = true;
    let env = Box::pin(create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_fnn_config(None),
    ))
    .await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID)
        .await
        .unwrap();
    let vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .unwrap();
    let prefix_id = create_tenant_overlay_prefix(&env, vpc_id).await;

    struct Case {
        scenario: &'static str,
        predecessor_selected_tenant: bool,
    }
    for Case {
        scenario,
        predecessor_selected_tenant,
    } in [
        Case {
            scenario: "new allocation",
            predecessor_selected_tenant: false,
        },
        Case {
            scenario: "predecessor selected tenant before network readiness",
            predecessor_selected_tenant: true,
        },
    ] {
        let mh = create_managed_host_multi_dpu(&env, 2).await;
        env.api
            .allocate_instance(
                InstanceAllocationRequest::builder(false)
                    .machine_id(mh.id)
                    .config(
                        InstanceConfig::default_tenant_and_os()
                            .network(single_interface_network_config_with_vpc_prefix(prefix_id)),
                    )
                    .tonic_request(),
            )
            .await
            .unwrap();
        let waiting = ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForNetworkSegmentToBeReady,
        };
        env.run_machine_state_controller_iteration_until_state_matches(
            &mh.id.into(),
            10,
            waiting.clone(),
        )
        .await;

        let mut txn = env.db_txn().await;
        let snapshot = mh.snapshot(&mut txn).await;
        assert_eq!(
            snapshot.host_snapshot.network_config.use_admin_network,
            Some(true),
            "{scenario}"
        );
        assert_eq!(snapshot.managed_state, waiting, "{scenario}");
        assert!(
            snapshot
                .dpu_snapshots
                .iter()
                .all(|dpu| dpu.network_config.use_admin_network_changed != Some(true)),
            "{scenario}"
        );
        let primary_dpu_id = snapshot
            .host_snapshot
            .status
            .interfaces
            .iter()
            .find(|interface| interface.primary_interface)
            .and_then(|interface| interface.attached_dpu_machine_id)
            .unwrap();
        txn.commit().await.unwrap();
        if predecessor_selected_tenant {
            // The predecessor persisted tenant mode and its restart before entering this wait.
            let mut txn = env.db_txn().await;
            let snapshot = mh.snapshot(&mut txn).await;
            let mut network = snapshot.host_snapshot.network_config.value.clone();
            network.use_admin_network = Some(false);
            assert_eq!(
                db::machine::try_update_network_config(
                    &mut txn,
                    &mh.id.into(),
                    snapshot.host_snapshot.network_config.version,
                    &network,
                )
                .await
                .unwrap(),
                db::ConditionalWrite::Applied(()),
                "{scenario}"
            );
            db::machine::set_use_admin_network_changed(&mut txn, &primary_dpu_id, true)
                .await
                .unwrap();
            txn.commit().await.unwrap();
        }

        let mut admin_responses = Vec::new();
        for dpu_id in &mh.dpu_ids {
            let response = env
                .api
                .get_managed_host_network_config(Request::new(
                    rpc::forge::ManagedHostNetworkConfigRequest {
                        dpu_machine_id: Some(*dpu_id),
                    },
                ))
                .await
                .unwrap()
                .into_inner();
            assert!(response.use_admin_network, "{scenario}");
            assert!(response.admin_interface.is_some(), "{scenario}");
            assert!(response.tenant_interfaces.is_empty(), "{scenario}");
            assert!(
                response.instance_network_config_version.is_empty(),
                "{scenario}"
            );
            assert!(response.vni_device.is_empty(), "{scenario}");
            admin_responses.push((*dpu_id, response));
        }
        // This also consumes the predecessor's early restart acknowledgement.
        mh.network_configured(&env).await;

        env.run_machine_state_controller_iteration().await;
        let tenant_wait = ManagedHostState::Assigned {
            instance_state: InstanceState::WaitingForNetworkConfig,
        };
        let mut txn = env.db_txn().await;
        let snapshot = mh.snapshot(&mut txn).await;
        assert_eq!(snapshot.managed_state, tenant_wait, "{scenario}");
        assert_eq!(
            snapshot.host_snapshot.network_config.use_admin_network,
            Some(false),
            "{scenario}"
        );
        let tenant_version = snapshot.host_snapshot.network_config.version;
        let (_, previous_admin_response) = &admin_responses[0];
        assert_ne!(
            tenant_version.version_string(),
            previous_admin_response.managed_host_config_version,
            "{scenario}"
        );
        for dpu in &snapshot.dpu_snapshots {
            assert_eq!(dpu.network_config.version, tenant_version, "{scenario}");
            assert_eq!(
                dpu.network_config.use_admin_network_changed == Some(true),
                dpu.id == primary_dpu_id,
                "{scenario}"
            );
        }
        txn.commit().await.unwrap();

        for (dpu_id, admin_response) in admin_responses {
            let response = env
                .api
                .get_managed_host_network_config(Request::new(
                    rpc::forge::ManagedHostNetworkConfigRequest {
                        dpu_machine_id: Some(dpu_id),
                    },
                ))
                .await
                .unwrap()
                .into_inner();
            assert_eq!(
                response.use_admin_network,
                dpu_id != primary_dpu_id,
                "{scenario}"
            );
            assert_eq!(
                response.tenant_interfaces.len(),
                usize::from(dpu_id == primary_dpu_id),
                "{scenario}"
            );
            assert_eq!(
                response.instance_network_config_version.is_empty(),
                dpu_id != primary_dpu_id,
                "{scenario}"
            );
            env.api
                .record_dpu_network_status(Request::new(rpc::forge::DpuNetworkStatus {
                    dpu_machine_id: Some(dpu_id),
                    network_config_version: Some(admin_response.managed_host_config_version),
                    dpu_health: Some(
                        health_report::HealthReport::empty("forge-dpu-agent".to_string()).into(),
                    ),
                    ..Default::default()
                }))
                .await
                .unwrap();
        }
        env.run_machine_state_controller_iteration().await;
        let mut txn = env.db_txn().await;
        let snapshot = mh.snapshot(&mut txn).await;
        assert_eq!(snapshot.managed_state, tenant_wait, "{scenario}");
        assert!(
            matches!(
                &snapshot.host_snapshot.controller_state_outcome,
                Some(PersistentStateHandlerOutcome::Wait { reason, .. })
                    if reason.contains("Waiting for DPU agent(s) to apply network config")
            ),
            "{scenario}"
        );
        assert!(
            snapshot
                .instance
                .as_ref()
                .unwrap()
                .observations
                .network
                .is_empty(),
            "{scenario}"
        );
        assert_eq!(
            snapshot
                .dpu_snapshots
                .iter()
                .find(|dpu| dpu.id == primary_dpu_id)
                .unwrap()
                .network_config
                .use_admin_network_changed,
            Some(true),
            "{scenario}"
        );
        txn.commit().await.unwrap();

        mh.network_configured(&env).await;
        let mut txn = env.db_txn().await;
        let snapshot = mh.snapshot(&mut txn).await;
        assert!(
            snapshot.managed_host_network_config_version_synced(),
            "{scenario}"
        );
        let instance = snapshot.instance.as_ref().unwrap();
        assert_eq!(
            instance.observations.network[&primary_dpu_id].config_version,
            instance.network_config_version,
            "{scenario}"
        );
        txn.commit().await.unwrap();
        env.run_machine_state_controller_iteration().await;
        let mut txn = env.db_txn().await;
        let snapshot = mh.snapshot(&mut txn).await;
        assert_eq!(
            snapshot.managed_state,
            ManagedHostState::Assigned {
                instance_state: InstanceState::WaitingForRebootToReady,
            },
            "{scenario}"
        );
        txn.commit().await.unwrap();
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

/// Verifies automatic selection remains deterministic across
/// every supported address-family mode.
#[crate::sqlx_test]
async fn test_auto_vpc_prefix_selection_uses_static_first_fit(pool: PgPool) {
    let fixture = create_auto_vpc_selection_fixture(pool).await;

    // Consume the original IPv4 capacity while checking first-fit,
    // persisted RPC projection, and final exhaustion.
    assert_static_ipv4_first_fit(&fixture).await;
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
    allocate_network(&mut historical_config, txn.as_mut())
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
        let error = allocate_network(&mut config, &mut txn)
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
    allocate_network(&mut explicit_config, txn.as_mut())
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
                dpu_machine_id: Some(managed_host.dpu().id),
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
    assert_eq!(address.prefix, network_prefix.prefix.to_string());
    assert!(address.gateway.is_none());

    // A retry sees the retained interface prefix as the durable allocation
    // result and does not consume another linknet.
    let mut retried_network = snapshot.config.network.clone();
    let expected_network = retried_network.clone();
    let mut txn = fixture.env.db_txn().await;
    allocate_network(&mut retried_network, txn.as_mut())
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
                dpu_machine_id: Some(dual_stack_host.dpu().id),
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
    let error = allocate_network(&mut exhausted_config, txn.as_mut())
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
) -> (InstanceId, HostMachine, ConfigVersion) {
    let managed_host = create_managed_host(env).await;
    let instance_id = InstanceId::new();
    let network_config_version = ConfigVersion::initial();
    let mut txn = env.db_txn().await;
    let machine = managed_host.host_as_any().db_machine(&mut txn).await;
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
    let final_original_allocation =
        allocate_automatic_ipv4_network(fixture.env.pool.clone(), fixture.vpc_id)
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
    let retried = allocate_automatic_ipv4_network(fixture.env.pool.clone(), fixture.vpc_id)
        .await
        .unwrap();
    assert_eq!(
        retried.interfaces[0].network_details,
        Some(NetworkDetails::VpcPrefixId(inserted_prefix_id)),
    );
}

/// Verifies trusted Core callers can allocate either selector across tenants and
/// that ownership, selector intent, and generated resources survive a public re-read.
#[crate::sqlx_test]
async fn test_cross_tenant_vpc_selectors_persist_allocations(pool: PgPool) {
    const CONSUMER_TENANT: &str = "cross-tenant-vpc-consumer";

    // Keep the VPC under its original owner and create a distinct Instance tenant.
    let fixture = create_auto_vpc_selection_fixture(pool).await;
    create_fixture_tenant(&fixture.env, CONSUMER_TENANT)
        .await
        .unwrap();
    let cases = [
        // Automatic selection must preserve caller intent alongside its resolved Prefix.
        ("automatic VPC", true),
        // Explicit selection must accept foreign ownership without requiring automatic intent.
        ("explicit Prefix", false),
    ];
    for (scenario, automatic) in cases {
        let managed_host = create_managed_host(&fixture.env).await;
        let network = if automatic {
            automatic_ipv4_rpc_network_config(fixture.vpc_id)
        } else {
            single_interface_network_config_with_vpc_prefix(fixture.lower_ipv4_prefix_id)
        };

        // Allocate through the public RPC and reconcile to populate persisted address state.
        let instance = managed_host
            .instance_builer(&fixture.env)
            .tenant_org(CONSUMER_TENANT)
            .network(network)
            .build()
            .await;
        let persisted = instance.rpc_instance().await;
        assert_eq!(
            persisted.config().tenant().tenant_organization_id,
            CONSUMER_TENANT,
            "{scenario}",
        );
        let interface = &persisted.config().network().interfaces[0];
        if automatic {
            assert_ipv4_auto_rpc_resolution(
                persisted.inner(),
                fixture.vpc_id,
                fixture.lower_ipv4_prefix_id,
            );
        } else {
            assert_eq!(
                interface.network_details,
                Some(
                    rpc::forge::instance_interface_config::NetworkDetails::VpcPrefixId(
                        fixture.lower_ipv4_prefix_id,
                    )
                ),
            );
        }
        assert!(
            !persisted.status().network().interfaces[0]
                .addresses
                .is_empty()
        );

        // The generated segment must belong to the selected VPC, not the Instance tenant.
        let mut txn = fixture.env.db_txn().await;
        let segment = db::network_segment::find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(IdColumn, &interface.network_segment_id.unwrap()),
            NetworkSegmentSearchConfig::default(),
        )
        .await
        .unwrap()
        .pop()
        .unwrap();
        assert_eq!(segment.config.vpc_id, Some(fixture.vpc_id), "{scenario}");
        assert_eq!(
            segment.prefixes[0].vpc_prefix_id,
            Some(fixture.lower_ipv4_prefix_id)
        );
        assert_eq!(
            db::instance_address::count_by_segment_id(&mut txn, &segment.id)
                .await
                .unwrap(),
            1,
            "{scenario}",
        );
        txn.rollback().await.unwrap();
    }
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

    let request = AdminForceDeleteMachineRequest {
        host_query: managed_host.id.to_string(),
        delete_interfaces: false,
        delete_bmc_interfaces: false,
        delete_bmc_credentials: false,
        allow_delete_with_orphaned_dpf_crds: false,
        delete_bmc_suppressions: false,
        delete_retained_boot_interfaces: false,
        release_preserved_addresses: false,
        wait_for_instance_dpu: false,
    };
    // Force delete must route automatic intent through generated-resource cleanup.
    let response = fixture
        .env
        .api
        .admin_force_delete_machine(Request::new(request))
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
        allocate_network(&mut network_config, &mut txn)
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
    let error = allocate_network(&mut exhausted_config, &mut txn)
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
    let ipv6_segment_prefix = ipv6_only_segment[0].prefixes[0].prefix;
    assert_eq!(
        ipv6_segment_prefix,
        "fd42:218::/127".parse::<IpNetwork>().unwrap(),
    );

    let ipv6_only_addresses =
        db::instance_address::find_by_segment_id(txn.as_mut(), &ipv6_only_segment_id)
            .await
            .unwrap();

    // Persistence contains one odd IPv6 host address from the selected /127.
    assert_eq!(ipv6_only_addresses.len(), 1);
    let tenant_address = ipv6_only_addresses[0].address;
    assert!(matches!(
        tenant_address,
        IpAddr::V6(address) if address.to_bits() & 1 == 1
    ));
    let tenant_interface_prefix = IpNetwork::new(tenant_address, 128).unwrap();
    assert_eq!(ipv6_only_addresses[0].prefix, tenant_interface_prefix);
    txn.commit().await.unwrap();

    // Core publishes only the real V6 family and leaves deprecated V4 fields
    // empty rather than fabricating an unusable V4 address configuration.
    let managed_config = fixture
        .env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu().id),
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
    assert_eq!(address.ip, tenant_address.to_string());
    assert_eq!(
        address.interface_prefix,
        tenant_interface_prefix.to_string()
    );
    assert_eq!(address.prefix, ipv6_segment_prefix.to_string());
    assert!(address.gateway.is_none());
    let legacy_ipv6 = tenant_interface
        .ipv6_interface_config
        .as_ref()
        .expect("IPv6-only config has a compatibility sidecar");
    assert_eq!(legacy_ipv6.ip, address.ip);
    assert_eq!(legacy_ipv6.interface_prefix, address.interface_prefix);
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
                dpu_machine_id: Some(managed_host.dpu().id),
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
) -> Result<InstanceNetworkConfig, crate::CarbideError> {
    let mut config = automatic_network_config(vpc_id, InstanceInterfaceIpFamilyMode::Ipv4Only);
    let mut txn = pool.begin().await.unwrap();
    let result = allocate_network(&mut config, txn.as_mut()).await;

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
            overlap_vpc_id: None,
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

/// Verifies service allocation persists real IPv4 /31 and IPv6 /127 network segments, network
/// prefixes, and Instance addresses because
/// family-specific link semantics cannot be proven by pure slot-planning tests.
#[crate::sqlx_test]
async fn test_service_vpc_allocator_persists_both_link_families(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create one Instance and IPv4 and IPv6 VPC prefixes; only the prefix CIDR differs between rows.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_test_env(pool).await;
    let host_segment_id = env.create_vpc_and_tenant_segment().await;
    let vpc_id = db::vpc::find_by_segment(&env.pool, host_segment_id)
        .await?
        .expect("fixture segment belongs to a VPC")
        .id;
    let managed_host = create_managed_host(&env).await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let cases = [
        // IPv4 proves the allocator uses /31 links and selects the second address for the service.
        (
            "IPv4",
            "10.218.0.0/30".parse::<IpNetwork>()?,
            31,
            "10.218.0.0/31".parse::<IpNetwork>()?,
            "10.218.0.1".parse::<IpAddr>()?,
            0,
        ),
        // IPv6 proves the allocator uses /127 links without applying the IPv4 gateway reservation.
        (
            "IPv6",
            "fd42:218::/126".parse::<IpNetwork>()?,
            127,
            "fd42:218::/127".parse::<IpNetwork>()?,
            "fd42:218::1".parse::<IpAddr>()?,
            1,
        ),
    ];

    for (family, parent_prefix, link_prefix_length, expected_link, expected_address, reserved) in
        cases
    {
        // Persist the VPC prefix outside the allocation transaction so the generated segment,
        // network prefix, and address can be
        // rolled back independently after all database observations are complete.
        let parent_id = create_tenant_overlay_prefix_with_prefix(
            &env,
            vpc_id,
            &format!("service allocator {family}"),
            parent_prefix,
        )
        .await;
        let mut txn = env.db_txn().await;
        let allocator = PrefixAllocator::new(parent_id, parent_prefix, None, link_prefix_length)?;

        // Allocate the link, its dedicated segment and prefix, then its normalized service address.
        let link_prefix = allocator.next_free_prefix(txn.as_mut()).await?;
        let (segment_id, network_prefix) = allocator
            .allocate_service_network_segment_for_prefix(txn.as_mut(), vpc_id, link_prefix)
            .await?;
        let address = db::instance_address::insert_service_address(
            txn.as_mut(),
            instance.id,
            segment_id,
            vpc_id,
            &network_prefix,
        )
        .await?;

        // Reload the generated segment, network prefix, and address to prove family-specific values.
        let segment = db::network_segment::find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(IdColumn, &segment_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?
        .pop()
        .expect("generated service segment exists");
        assert_eq!(link_prefix, expected_link, "{family}");
        assert_eq!(address, expected_address, "{family}");
        assert_eq!(
            segment.config.mtu,
            i32::try_from(carbide_dpf::SERVICE_VPC_MTU).expect("service MTU fits segment storage")
        );
        assert_eq!(segment.config.segment_type, NetworkSegmentType::Tenant);
        assert_eq!(segment.status.can_stretch, Some(false));
        assert_eq!(segment.prefixes.len(), 1);
        assert_eq!(segment.prefixes[0].id, network_prefix.id);
        assert_eq!(segment.prefixes[0].prefix, expected_link);
        assert_eq!(segment.prefixes[0].vpc_prefix_id, Some(parent_id));
        assert_eq!(segment.prefixes[0].vpc_prefix, Some(parent_prefix));
        assert_eq!(
            segment.prefixes[0].gateway,
            expected_link.is_ipv4().then_some(expected_link.network())
        );
        assert_eq!(segment.prefixes[0].num_reserved, reserved);
        let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &instance.id,
            &segment_id,
        )
        .await?;
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].address, expected_address);
        assert_eq!(addresses[0].prefix, expected_link);
        assert!(addresses[0].hostname.is_none());
        txn.rollback().await?;
    }

    Ok(())
}

/// Verifies the enabled IPv6 coordinator path persists a /127 endpoint and normalized address,
/// proving deployment-family admission reaches the transactional service allocator.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_allocates_enabled_ipv6_127_endpoint(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Enable only IPv6 and construct the FNN host and service topology required for activation.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env_with_capacity_and_families(
        pool,
        service_vpc_deployment_mock(),
        2,
        2,
        &[model::extension_service::ServiceVpcAddressFamily::Ipv6],
    )
    .await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, host_segment_id) = create_fnn_host_segment(
        &env,
        "IPv6 coordinator host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "IPv6 coordinator service VPC").await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        service_vpc_id,
        "IPv6 coordinator service prefix",
        "fd42:230::/126".parse()?,
    )
    .await;
    let host = create_service_vpc_test_host(&env).await?;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let service = create_networked_dpf_service_with_target_and_family(
        &env,
        "ipv6-coordinator-service",
        rpc::forge::DpuExtensionServiceDpuTarget::All,
        rpc::forge::ServiceVpcAddressFamily::Ipv6,
    )
    .await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let (attachment, selection) = coordinator_attachment(&service, service_vpc_id)?;
    let extension_config = InstanceExtensionServicesConfig {
        service_configs: vec![attachment],
    };
    let request = RequestedInstanceExtensionServicesConfig {
        service_configs: vec![selection],
    };
    let mut snapshot_txn = env.db_txn().await;
    let host_snapshot = host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let instance_snapshot = host_snapshot
        .instance
        .clone()
        .expect("IPv6 coordinator host has an instance");

    // Allocate and write through `update_service_network_config`, as complete updates do.
    let mut allocation = db::Transaction::begin(&env.pool).await?;
    let network =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation,
            instance.id,
            &extension_config,
            &request,
            &instance_snapshot.config.network,
            &host_snapshot,
            false,
            Some(&instance_snapshot),
        )
        .await?;
    assert!(matches!(
        db::instance::update_service_network_config(
            allocation.as_mut(),
            instance.id,
            instance_snapshot.extension_services_config_version,
            &instance_snapshot.config.extension_services,
            &extension_config,
            instance_snapshot.network_config_version,
            &instance_snapshot.config.network,
            &network,
            true,
        )
        .await?,
        db::ConditionalWrite::Applied(_)
    ));
    allocation.commit().await?;

    // Fresh reads prove the service-interface fields, /127 link, address, and
    // `VpcPrefix.status.last_used_prefix` all committed.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("IPv6 coordinator instance remains stored");
    let [endpoint] = persisted.config.network.service_interfaces.as_slice() else {
        panic!("IPv6 coordinator must persist exactly one service endpoint");
    };
    assert_eq!(endpoint.vpc_id, service_vpc_id);
    assert_eq!(endpoint.vpc_prefix_id, parent_id);
    assert_eq!(endpoint.dpu_id, host.dpu_ids[0]);
    assert_eq!(endpoint.link_prefix, "fd42:230::/127".parse()?);
    let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut *env.pool.acquire().await?,
        &instance.id,
        &endpoint.network_segment_id,
    )
    .await?;
    assert_eq!(addresses.len(), 1);
    assert_eq!(addresses[0].address, "fd42:230::1".parse::<IpAddr>()?);
    assert_eq!(addresses[0].prefix, endpoint.link_prefix);
    let parent =
        db::vpc_prefix::get_for_allocation_by_ids(&mut *env.pool.acquire().await?, &[parent_id])
            .await?
            .pop()
            .expect("IPv6 service parent remains stored");
    assert_eq!(parent.status.last_used_prefix, Some(endpoint.link_prefix));

    Ok(())
}

/// Verifies competing coordinators serialize on the exclusive `instance_addresses` table lock
/// before VPC reads and commit distinct service-interface, segment, prefix, and address records.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_serializes_competing_allocations(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create two distinct DPF hosts, host VPCs, instances, and registrations sharing one service VPC.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, first_host_segment) = create_fnn_host_segment(
        &env,
        "coordinator host VPC 1",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let (_, second_host_segment) = create_fnn_host_segment(
        &env,
        "coordinator host VPC 2",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "shared coordinator service VPC").await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        service_vpc_id,
        "shared coordinator service prefix",
        "10.220.0.0/29".parse()?,
    )
    .await;
    let first_host = create_service_vpc_test_host(&env).await?;
    let second_host = create_service_vpc_test_host(&env).await?;
    let first_instance = first_host
        .instance_builer(&env)
        .single_interface_network_config(first_host_segment)
        .build()
        .await;
    let second_instance = second_host
        .instance_builer(&env)
        .single_interface_network_config(second_host_segment)
        .build()
        .await;
    let first_service = create_networked_dpf_service(&env, "coordinator-service-1").await?;
    let second_service = create_networked_dpf_service(&env, "coordinator-service-2").await?;
    mark_networked_dpf_service_ready(&env, first_service.service_id.parse()?).await?;
    mark_networked_dpf_service_ready(&env, second_service.service_id.parse()?).await?;
    let (first_attachment, first_selection) =
        coordinator_attachment(&first_service, service_vpc_id)?;
    let (second_attachment, second_selection) =
        coordinator_attachment(&second_service, service_vpc_id)?;
    let first_extension_config = InstanceExtensionServicesConfig {
        service_configs: vec![first_attachment],
    };
    let first_request = RequestedInstanceExtensionServicesConfig {
        service_configs: vec![first_selection],
    };
    let second_extension_config = InstanceExtensionServicesConfig {
        service_configs: vec![second_attachment],
    };
    let second_request = RequestedInstanceExtensionServicesConfig {
        service_configs: vec![second_selection],
    };

    // Load both `ManagedHostStateSnapshot` and `InstanceSnapshot` values before either transaction takes allocator locks.
    let mut snapshot_txn = env.db_txn().await;
    let first_host_snapshot = first_host.snapshot(&mut snapshot_txn).await;
    let second_host_snapshot = second_host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let first_instance_snapshot = first_host_snapshot
        .instance
        .clone()
        .expect("first host has an instance");
    let second_instance_snapshot = second_host_snapshot
        .instance
        .clone()
        .expect("second host has an instance");

    // Let the first transaction allocate and publish conditionally while retaining every lock.
    let mut first_txn = db::Transaction::begin(&env.pool).await?;
    let first_network =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut first_txn,
            first_instance.id,
            &first_extension_config,
            &first_request,
            &first_instance_snapshot.config.network,
            &first_host_snapshot,
            false,
            Some(&first_instance_snapshot),
        )
        .await?;
    assert!(matches!(
        db::instance::update_service_network_config(
            first_txn.as_mut(),
            first_instance.id,
            first_instance_snapshot.extension_services_config_version,
            &first_instance_snapshot.config.extension_services,
            &first_extension_config,
            first_instance_snapshot.network_config_version,
            &first_instance_snapshot.config.network,
            &first_network,
            true,
        )
        .await?,
        db::ConditionalWrite::Applied(_)
    ));
    let first_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(first_txn.as_pgconn())
        .await?;

    // Start the disjoint second attachment and verify it waits for the exclusive
    // `instance_addresses` table lock before performing the allocation-authorizing VPC read.
    let second_instance_id = second_instance.id;
    let second_extension_version = second_instance_snapshot.extension_services_config_version;
    let second_network_version = second_instance_snapshot.network_config_version;
    let second_pool = env.pool.clone();
    let second_api = env.api.clone();
    let pending_second = TestTaskHandle::new(tokio::spawn(async move {
        let mut txn = db::Transaction::begin(&second_pool).await.unwrap();
        let network =
            crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
                &second_api,
                &mut txn,
                second_instance_id,
                &second_extension_config,
                &second_request,
                &second_instance_snapshot.config.network,
                &second_host_snapshot,
                false,
                Some(&second_instance_snapshot),
            )
            .await
            .unwrap();
        let write = db::instance::update_service_network_config(
            txn.as_mut(),
            second_instance_id,
            second_extension_version,
            &second_instance_snapshot.config.extension_services,
            &second_extension_config,
            second_network_version,
            &second_instance_snapshot.config.network,
            &network,
            true,
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        write
    }));
    wait_for_blocked_query(
        &env.pool,
        first_pid,
        "LOCK TABLE instance_addresses IN ACCESS EXCLUSIVE MODE",
    )
    .await;
    first_txn.commit().await?;
    assert!(matches!(
        join_test_task(pending_second, "second coordinator").await,
        db::ConditionalWrite::Applied(_)
    ));

    // Fresh reads prove each Instance committed distinct service-interface, segment, prefix, and address records.
    let first_persisted = db::instance::find_by_id(&env.pool, first_instance.id)
        .await?
        .expect("first instance remains stored");
    let second_persisted = db::instance::find_by_id(&env.pool, second_instance_id)
        .await?
        .expect("second instance remains stored");
    let [first_endpoint] = first_persisted.config.network.service_interfaces.as_slice() else {
        panic!("first coordinator transaction must persist one endpoint");
    };
    let [second_endpoint] = second_persisted
        .config
        .network
        .service_interfaces
        .as_slice()
    else {
        panic!("second coordinator transaction must persist one endpoint");
    };
    assert_eq!(
        first_persisted
            .config
            .extension_services
            .service_configs
            .len(),
        1
    );
    assert_eq!(
        second_persisted
            .config
            .extension_services
            .service_configs
            .len(),
        1
    );
    assert_eq!(
        first_persisted.config.extension_services.service_configs[0].id,
        Some(first_endpoint.attachment_id)
    );
    assert_eq!(
        second_persisted.config.extension_services.service_configs[0].id,
        Some(second_endpoint.attachment_id)
    );
    assert_ne!(first_endpoint.link_prefix, second_endpoint.link_prefix);
    assert_ne!(
        db::instance_address::service_address_for_prefix(first_endpoint.link_prefix)?,
        db::instance_address::service_address_for_prefix(second_endpoint.link_prefix)?
    );
    assert_ne!(
        first_endpoint.network_segment_id,
        second_endpoint.network_segment_id
    );
    assert_ne!(
        first_endpoint.network_prefix_id,
        second_endpoint.network_prefix_id
    );
    assert_eq!(first_endpoint.vpc_prefix_id, parent_id);
    assert_eq!(second_endpoint.vpc_prefix_id, parent_id);
    let mut observer = env.pool.acquire().await?;
    for (instance_id, endpoint) in [
        (first_instance.id, first_endpoint),
        (second_instance_id, second_endpoint),
    ] {
        let segment = db::network_segment::find_by(
            &mut *observer,
            ObjectColumnFilter::One(IdColumn, &endpoint.network_segment_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?
        .pop()
        .expect("generated service segment remains stored");
        assert_eq!(segment.prefixes.len(), 1);
        assert_eq!(segment.prefixes[0].id, endpoint.network_prefix_id);
        let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            &mut observer,
            &instance_id,
            &endpoint.network_segment_id,
        )
        .await?;
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].prefix, endpoint.link_prefix);
    }
    assert_eq!(
        first_persisted
            .extension_services_config_version
            .version_nr(),
        first_instance_snapshot
            .extension_services_config_version
            .version_nr()
            + 1
    );
    assert_eq!(
        first_persisted.network_config_version.version_nr(),
        first_instance_snapshot.network_config_version.version_nr() + 1
    );
    assert_eq!(
        second_persisted
            .extension_services_config_version
            .version_nr(),
        second_extension_version.version_nr() + 1
    );
    assert_eq!(
        second_persisted.network_config_version.version_nr(),
        second_network_version.version_nr() + 1
    );
    let parent = db::vpc_prefix::get_for_allocation_by_ids(&mut observer, &[parent_id])
        .await?
        .pop()
        .expect("shared parent remains stored");
    assert_eq!(
        parent.status.last_used_prefix,
        Some(second_endpoint.link_prefix)
    );

    Ok(())
}

/// Verifies same-instance additions competing for its final endpoint reservation serialize to one
/// winner and leave no second service-interface, network-segment, network-prefix, or
/// Instance-address record from the stale loser transaction.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_rolls_back_competing_same_instance_addition(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Limit each DPU to one endpoint and prepare two additions from the same InstanceSnapshot, whose
    // network has no service_interfaces.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env_with_capacity(
        pool,
        service_vpc_deployment_mock(),
        2,
        1,
    )
    .await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "same-instance-race").await?;
    let service_vpc_id = fixture.request.service_configs[0].service_vpc_ids[0];
    let competing_service = create_networked_dpf_service(&env, "same-instance-race-2").await?;
    mark_networked_dpf_service_ready(&env, competing_service.service_id.parse()?).await?;
    let (competing_attachment, competing_selection) =
        coordinator_attachment(&competing_service, service_vpc_id)?;
    let competing_extension_config = InstanceExtensionServicesConfig {
        service_configs: vec![competing_attachment],
    };
    let competing_request = RequestedInstanceExtensionServicesConfig {
        service_configs: vec![competing_selection],
    };

    // Publish the first endpoint conditionally while retaining all allocation and instance locks.
    let mut winner_txn = db::Transaction::begin(&env.pool).await?;
    let winner_network =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut winner_txn,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await?;
    assert!(matches!(
        db::instance::update_service_network_config(
            winner_txn.as_mut(),
            fixture.instance_id,
            fixture.instance_snapshot.extension_services_config_version,
            &fixture.instance_snapshot.config.extension_services,
            &fixture.extension_config,
            fixture.instance_snapshot.network_config_version,
            &fixture.instance_snapshot.config.network,
            &winner_network,
            true,
        )
        .await?,
        db::ConditionalWrite::Applied(_)
    ));
    let winner_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(winner_txn.as_pgconn())
        .await?;

    // Start the competing addition from the same InstanceSnapshot and verify the exclusive
    // `instance_addresses` table lock blocks it before any allocation-authorizing VPC read.
    let competing_pool = env.pool.clone();
    let competing_api = env.api.clone();
    let competing_instance_snapshot = fixture.instance_snapshot.clone();
    let competing_host_snapshot = fixture.host_snapshot.clone();
    let instance_id = fixture.instance_id;
    let pending_loser = TestTaskHandle::new(tokio::spawn(async move {
        let mut txn = db::Transaction::begin(&competing_pool).await.unwrap();
        let result =
            crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
                &competing_api,
                &mut txn,
                instance_id,
                &competing_extension_config,
                &competing_request,
                &competing_instance_snapshot.config.network,
                &competing_host_snapshot,
                false,
                Some(&competing_instance_snapshot),
            )
            .await;
        txn.rollback().await.unwrap();
        result
    }));
    wait_for_blocked_query(
        &env.pool,
        winner_pid,
        "LOCK TABLE instance_addresses IN ACCESS EXCLUSIVE MODE",
    )
    .await;
    winner_txn.commit().await?;

    // The stale transaction must fail its locked Instance comparison rather than publish a second
    // endpoint.
    let loser_error = join_test_task(pending_loser, "competing coordinator")
        .await
        .expect_err("the stale same-instance addition must fail");
    assert!(matches!(
        loser_error,
        crate::CarbideError::ConcurrentModificationError(..)
    ));

    // Fresh reads prove only the winner's service interface, network segment, network prefix,
    // Instance address, `VpcPrefix.status.last_used_prefix` update, and configuration-version
    // increments survived.
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("instance remains stored");
    let [endpoint] = persisted.config.network.service_interfaces.as_slice() else {
        panic!("exactly one competing addition must win");
    };
    assert_eq!(
        persisted.config.extension_services,
        fixture.extension_config
    );
    assert_eq!(
        persisted.extension_services_config_version.version_nr(),
        fixture
            .instance_snapshot
            .extension_services_config_version
            .version_nr()
            + 1
    );
    assert_eq!(
        persisted.network_config_version.version_nr(),
        fixture
            .instance_snapshot
            .network_config_version
            .version_nr()
            + 1
    );
    let mut observer = env.pool.acquire().await?;
    let generated_segments = db::network_segment::find_by(
        &mut *observer,
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &service_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert_eq!(generated_segments.len(), 1);
    assert_eq!(generated_segments[0].id, endpoint.network_segment_id);
    let persisted_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut observer,
        &fixture.instance_id,
        &endpoint.network_segment_id,
    )
    .await?;
    assert_eq!(persisted_addresses.len(), 1);
    let parent = db::vpc_prefix::get_for_allocation_by_ids(&mut observer, &[fixture.parent_id])
        .await?
        .pop()
        .expect("shared parent remains stored");
    assert_eq!(parent.status.last_used_prefix, Some(endpoint.link_prefix));

    Ok(())
}

/// Verifies one All attachment commits one service interface, network segment, network prefix, and
/// Instance address per targeted DPU so activation cannot reuse records or store an incorrect `dpu_id`.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_persists_all_target_dpu_identities(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Give a two-DPU host one explicitly located FNN interface per DPU and shared service capacity.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, first_host_segment) = create_fnn_host_segment(
        &env,
        "multi-DPU success host VPC 1",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let (_, second_host_segment) = create_fnn_host_segment(
        &env,
        "multi-DPU success host VPC 2",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "multi-DPU success service VPC").await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        service_vpc_id,
        "multi-DPU success service prefix",
        "10.233.0.0/29".parse()?,
    )
    .await;
    let host = create_service_vpc_test_host_with_dpu_count(&env, 2).await?;
    let mut locator_txn = env.db_txn().await;
    let host_machine = host.host().db_machine(&mut locator_txn).await;
    let dpu_locators = host
        .dpu_ids
        .iter()
        .map(|dpu_id| {
            host_machine
                .get_device_locator_for_dpu_id(dpu_id)
                .expect("fixture DPU has a device locator")
        })
        .collect_vec();
    locator_txn.rollback().await?;
    let instance = host
        .instance_builer(&env)
        .network(interface_network_config_with_devices(
            &[first_host_segment, second_host_segment],
            &dpu_locators,
        ))
        .build()
        .await;
    let service = create_networked_dpf_service(&env, "multi-dpu-success-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let (attachment, selection) = coordinator_attachment(&service, service_vpc_id)?;
    let attachment_id = attachment.id.expect("new attachment has durable identity");
    let extension_config = InstanceExtensionServicesConfig {
        service_configs: vec![attachment],
    };
    let request = RequestedInstanceExtensionServicesConfig {
        service_configs: vec![selection],
    };
    let mut snapshot_txn = env.db_txn().await;
    let host_snapshot = host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let instance_snapshot = host_snapshot
        .instance
        .clone()
        .expect("multi-DPU host has an instance");

    // Allocate both service interfaces and write them through `update_service_network_config`.
    let mut activation = db::Transaction::begin(&env.pool).await?;
    let network =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut activation,
            instance.id,
            &extension_config,
            &request,
            &instance_snapshot.config.network,
            &host_snapshot,
            false,
            Some(&instance_snapshot),
        )
        .await?;
    assert!(matches!(
        db::instance::update_service_network_config(
            activation.as_mut(),
            instance.id,
            instance_snapshot.extension_services_config_version,
            &instance_snapshot.config.extension_services,
            &extension_config,
            instance_snapshot.network_config_version,
            &instance_snapshot.config.network,
            &network,
            true,
        )
        .await?,
        db::ConditionalWrite::Applied(_)
    ));
    activation.commit().await?;

    // Fresh reads must retain one independently backed service-interface record for each targeted DPU.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("multi-DPU activation remains stored");
    assert_eq!(persisted.config.extension_services, extension_config);
    let endpoints = &persisted.config.network.service_interfaces;
    let [first_endpoint, second_endpoint] = endpoints.as_slice() else {
        panic!("multi-DPU activation must persist two endpoints");
    };
    assert!(endpoints.iter().all(|endpoint| {
        endpoint.attachment_id == attachment_id
            && endpoint.interface_ordinal == 0
            && endpoint.vpc_id == service_vpc_id
    }));
    assert_eq!(
        endpoints
            .iter()
            .map(|endpoint| endpoint.dpu_id)
            .collect::<HashSet<_>>(),
        host.dpu_ids.iter().copied().collect::<HashSet<_>>()
    );
    assert_ne!(
        first_endpoint.network_segment_id,
        second_endpoint.network_segment_id
    );
    assert_ne!(
        first_endpoint.network_prefix_id,
        second_endpoint.network_prefix_id
    );
    assert_ne!(first_endpoint.link_prefix, second_endpoint.link_prefix);
    assert_ne!(
        db::instance_address::service_address_for_prefix(first_endpoint.link_prefix)?,
        db::instance_address::service_address_for_prefix(second_endpoint.link_prefix)?
    );
    assert_eq!(first_endpoint.vpc_prefix_id, parent_id);
    assert_eq!(second_endpoint.vpc_prefix_id, parent_id);

    let mut observer = env.pool.acquire().await?;
    for endpoint in endpoints {
        let segment = db::network_segment::find_by(
            &mut *observer,
            ObjectColumnFilter::One(IdColumn, &endpoint.network_segment_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?
        .pop()
        .expect("generated service segment remains stored");
        assert_eq!(segment.prefixes.len(), 1);
        assert_eq!(segment.prefixes[0].id, endpoint.network_prefix_id);
        assert_eq!(segment.prefixes[0].prefix, endpoint.link_prefix);
        assert_eq!(segment.prefixes[0].vpc_prefix_id, Some(parent_id));

        let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            &mut observer,
            &instance.id,
            &endpoint.network_segment_id,
        )
        .await?;
        assert_eq!(addresses.len(), 1);
        assert_eq!(addresses[0].prefix, endpoint.link_prefix);
        assert_eq!(
            addresses[0].address,
            db::instance_address::service_address_for_prefix(endpoint.link_prefix)?
        );
    }
    let parent = db::vpc_prefix::get_for_allocation_by_ids(&mut observer, &[parent_id])
        .await?
        .pop()
        .expect("shared parent remains stored");
    assert_eq!(
        parent.status.last_used_prefix,
        Some(second_endpoint.link_prefix)
    );

    Ok(())
}

/// Verifies a late second-DPU capacity failure rolls back the first DPU's network segment, network
/// prefix, Instance address, and `VpcPrefix.status.last_used_prefix` update,
/// preventing an all-DPU attachment from publishing only a subset of its required targets.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_rolls_back_earlier_intent_after_late_exhaustion(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Give one all-DPU service a two-DPU host and one shared VPC prefix containing exactly one /31.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, first_host_segment) = create_fnn_host_segment(
        &env,
        "rollback host VPC 1",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let (_, second_host_segment) = create_fnn_host_segment(
        &env,
        "rollback host VPC 2",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "rollback service VPC").await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        service_vpc_id,
        "single-link rollback prefix",
        "10.221.0.0/31".parse()?,
    )
    .await;
    let host = create_service_vpc_test_host_with_dpu_count(&env, 2).await?;
    let mut locator_txn = env.db_txn().await;
    let host_machine = host.host().db_machine(&mut locator_txn).await;
    let dpu_locators = host
        .dpu_ids
        .iter()
        .map(|dpu_id| {
            host_machine
                .get_device_locator_for_dpu_id(dpu_id)
                .expect("fixture DPU has a device locator")
        })
        .collect_vec();
    locator_txn.rollback().await?;
    let instance = host
        .instance_builer(&env)
        .network(interface_network_config_with_devices(
            &[first_host_segment, second_host_segment],
            &dpu_locators,
        ))
        .build()
        .await;
    let service = create_networked_dpf_service(&env, "rollback-all-dpu-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let (attachment, selection) = coordinator_attachment(&service, service_vpc_id)?;
    let extension_config = InstanceExtensionServicesConfig {
        service_configs: vec![attachment],
    };
    let request = RequestedInstanceExtensionServicesConfig {
        service_configs: vec![selection],
    };
    let mut snapshot_txn = env.db_txn().await;
    let host_snapshot = host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let instance_snapshot = host_snapshot
        .instance
        .clone()
        .expect("host has an instance");

    // The first sorted DPU allocates before the second exhausts the VPC prefix.
    let mut allocation_txn = db::Transaction::begin(&env.pool).await?;
    let error =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation_txn,
            instance.id,
            &extension_config,
            &request,
            &instance_snapshot.config.network,
            &host_snapshot,
            false,
            Some(&instance_snapshot),
        )
        .await
        .expect_err("the second endpoint must exhaust the only link prefix");
    assert!(matches!(error, crate::CarbideError::ResourceExhausted(_)));
    let generated = db::network_segment::find_by(
        allocation_txn.as_pgconn(),
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &service_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert_eq!(generated.len(), 1);
    let address_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM instance_addresses WHERE vpc_id = $1")
            .bind(service_vpc_id)
            .fetch_one(allocation_txn.as_pgconn())
            .await?;
    assert_eq!(address_count, 1);
    let parent = db::vpc_prefix::get_for_allocation_by_ids(allocation_txn.as_mut(), &[parent_id])
        .await?
        .pop()
        .expect("parent remains visible in the transaction");
    assert_eq!(
        parent.status.last_used_prefix,
        Some("10.221.0.0/31".parse()?)
    );
    allocation_txn.rollback().await?;

    // Fresh reads prove the failed coordinator created no network segment or Instance address,
    // changed neither `extension_services_config` nor `network_config`, left
    // `VpcPrefix.status.last_used_prefix` unchanged, and incremented neither
    // `extension_services_config_version` nor `network_config_version`.
    let generated = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &service_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert!(generated.is_empty());
    let address_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM instance_addresses WHERE vpc_id = $1")
            .bind(service_vpc_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(address_count, 0);
    let parent =
        db::vpc_prefix::get_for_allocation_by_ids(&mut *env.pool.acquire().await?, &[parent_id])
            .await?
            .pop()
            .expect("parent remains stored after rollback");
    assert_eq!(parent.status.last_used_prefix, None);
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored after rollback");
    assert_eq!(
        persisted.config.extension_services,
        instance_snapshot.config.extension_services
    );
    assert_eq!(persisted.config.network, instance_snapshot.config.network);
    assert_eq!(
        persisted.extension_services_config_version,
        instance_snapshot.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        instance_snapshot.network_config_version
    );

    Ok(())
}

/// Verifies batch allocation rolls back an earlier member's generated network-segment and
/// Instance-address records when a later member exhausts the shared VPC prefix.
#[crate::sqlx_test]
async fn test_batch_allocate_instances_rolls_back_earlier_service_vpc_allocation_on_late_exhaustion(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Give two one-DPU requests distinct FNN host paths and one shared VPC prefix with exactly one /31.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, first_host_segment) = create_fnn_host_segment(
        &env,
        "batch rollback host VPC 1",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let (_, second_host_segment) = create_fnn_host_segment(
        &env,
        "batch rollback host VPC 2",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "batch rollback service VPC").await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        service_vpc_id,
        "batch rollback single-link prefix",
        "10.231.0.0/31".parse()?,
    )
    .await;
    let first_host = create_service_vpc_test_host(&env).await?;
    let second_host = create_service_vpc_test_host(&env).await?;
    let service = create_networked_dpf_service(&env, "batch-rollback-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let service_version = service
        .latest_version_info
        .as_ref()
        .expect("created batch service has a version")
        .version
        .clone();
    let first_instance_id = InstanceId::new();
    let second_instance_id = InstanceId::new();
    let requests = [
        (&first_host, first_host_segment, first_instance_id, "first"),
        (
            &second_host,
            second_host_segment,
            second_instance_id,
            "second",
        ),
    ]
    .into_iter()
    .map(
        |(host, host_segment, instance_id, name)| rpc::forge::InstanceAllocationRequest {
            machine_id: Some(host.host().id),
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(host_segment)),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                        service_id: service.service_id.clone(),
                        version: service_version.clone(),
                        service_vpc_ids: vec![service_vpc_id],
                    }],
                }),
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            instance_id: Some(instance_id),
            instance_type_id: None,
            metadata: Some(rpc::Metadata {
                name: format!("batch-service-vpc-{name}"),
                description: "service-VPC batch rollback fixture".to_string(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        },
    )
    .collect_vec();

    // The first request consumes the only link inside the batch transaction before the second fails.
    let error = crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
        env.api
            .allocate_instances(Request::new(rpc::forge::BatchInstanceAllocationRequest {
                instance_requests: requests,
            })),
    )
    .await
    .expect_err("the later batch member must exhaust shared service capacity");
    assert_eq!(error.code(), tonic::Code::ResourceExhausted);

    // Public and host snapshot reads prove neither batch member survived the failed transaction.
    let persisted = env
        .api
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![first_instance_id, second_instance_id],
        }))
        .await?
        .into_inner();
    assert!(persisted.instances.is_empty());
    let mut snapshot_txn = env.db_txn().await;
    assert!(
        first_host
            .snapshot(&mut snapshot_txn)
            .await
            .instance
            .is_none()
    );
    assert!(
        second_host
            .snapshot(&mut snapshot_txn)
            .await
            .instance
            .is_none()
    );
    snapshot_txn.rollback().await?;

    // Generated network segments, service and host `instance_addresses`, and
    // `VpcPrefix.status.last_used_prefix` must all roll back.
    let generated_segments = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &service_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert!(generated_segments.is_empty());
    let service_address_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM instance_addresses WHERE vpc_id = $1")
            .bind(service_vpc_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(service_address_count, 0);
    for segment_id in [first_host_segment, second_host_segment] {
        assert_eq!(
            db::instance_address::count_by_segment_id(
                &mut *env.pool.acquire().await?,
                &segment_id,
            )
            .await?,
            0
        );
    }
    let parent =
        db::vpc_prefix::get_for_allocation_by_ids(&mut *env.pool.acquire().await?, &[parent_id])
            .await?
            .pop()
            .expect("batch service parent remains stored");
    assert_eq!(parent.status.last_used_prefix, None);

    Ok(())
}

/// Verifies an all-DPU attachment fails atomically when one targeted DPU has no remaining endpoint
/// capacity, preventing partial allocation on another targeted DPU that still has room.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_rejects_multi_dpu_addition_when_one_target_is_full(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Give a two-DPU host one FNN tenant interface per DPU and only one endpoint per-DPU capacity.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env_with_capacity(
        pool,
        service_vpc_deployment_mock(),
        2,
        1,
    )
    .await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let host_segments = env
        .create_vpc_and_tenant_segments_with_vpc_details(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "multi-DPU capacity host VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .rpc(),
            2,
        )
        .await;
    let host = create_service_vpc_test_host_with_dpu_count(&env, 2).await?;
    let mut locator_txn = env.db_txn().await;
    let host_machine = host.host().db_machine(&mut locator_txn).await;
    let dpu_locators = host
        .dpu_ids
        .iter()
        .map(|dpu_id| {
            host_machine
                .get_device_locator_for_dpu_id(dpu_id)
                .expect("fixture DPU has a device locator")
        })
        .collect_vec();
    locator_txn.rollback().await?;
    let instance = host
        .instance_builer(&env)
        .network(interface_network_config_with_devices(
            &host_segments,
            &dpu_locators,
        ))
        .build()
        .await;

    // Occupy the primary DPU's sole endpoint reservation with one valid retained attachment.
    let service_vpc_id = create_fnn_vpc(&env, "multi-DPU capacity service VPC").await;
    let retained_service = create_networked_dpf_service_with_target(
        &env,
        "multi-dpu-retained-service",
        rpc::forge::DpuExtensionServiceDpuTarget::Primary,
    )
    .await?;
    mark_networked_dpf_service_ready(&env, retained_service.service_id.parse()?).await?;
    let mut initial_snapshot_txn = env.db_txn().await;
    let initial_host_snapshot = host.snapshot(&mut initial_snapshot_txn).await;
    initial_snapshot_txn.rollback().await?;
    let primary_dpu = initial_host_snapshot
        .host_snapshot
        .primary_attached_dpu_machine_id()
        .expect("two-DPU fixture has a primary DPU");
    let retained_endpoint = seed_networked_attachment_with_prefix(
        &env,
        instance.id,
        primary_dpu,
        &retained_service,
        service_vpc_id,
        0,
        "multi-DPU retained service prefix",
        "10.224.0.0/29".parse()?,
    )
    .await?;

    // Request another attachment on every DPU so one target is full while the other remains free.
    let requested_service =
        create_networked_dpf_service(&env, "multi-dpu-requested-service").await?;
    mark_networked_dpf_service_ready(&env, requested_service.service_id.parse()?).await?;
    let (requested_attachment, requested_selection) =
        coordinator_attachment(&requested_service, service_vpc_id)?;
    let mut snapshot_txn = env.db_txn().await;
    let host_snapshot = host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let instance_snapshot = host_snapshot
        .instance
        .clone()
        .expect("multi-DPU host has an instance");
    let mut desired_extension_config = instance_snapshot.config.extension_services.clone();
    desired_extension_config
        .service_configs
        .push(requested_attachment);
    let request = RequestedInstanceExtensionServicesConfig {
        service_configs: vec![requested_selection],
    };

    // Run the coordinator in an outer transaction and roll back the rejected all-DPU addition.
    let mut allocation_txn = db::Transaction::begin(&env.pool).await?;
    let error =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation_txn,
            instance.id,
            &desired_extension_config,
            &request,
            &instance_snapshot.config.network,
            &host_snapshot,
            false,
            Some(&instance_snapshot),
        )
        .await
        .expect_err("one full target DPU must reject the complete attachment");
    assert!(matches!(error, crate::CarbideError::ResourceExhausted(_)));
    allocation_txn.rollback().await?;

    // Fresh reads prove the retained attachment, service interface, network segment, and Instance
    // address survived and the free DPU received no new service interface.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored");
    assert_eq!(
        persisted.config.extension_services,
        instance_snapshot.config.extension_services
    );
    assert_eq!(
        persisted.config.network.service_interfaces,
        vec![retained_endpoint.clone()]
    );
    assert_eq!(
        persisted.extension_services_config_version,
        instance_snapshot.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        instance_snapshot.network_config_version
    );
    let generated_segments = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &service_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert_eq!(generated_segments.len(), 1);
    assert_eq!(
        generated_segments[0].id,
        retained_endpoint.network_segment_id
    );
    let address_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM instance_addresses WHERE vpc_id = $1")
            .bind(service_vpc_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(address_count, 1);
    let parent = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[retained_endpoint.vpc_prefix_id],
    )
    .await?
    .pop()
    .expect("retained parent remains stored");
    assert_eq!(
        parent.status.last_used_prefix,
        Some(retained_endpoint.link_prefix)
    );

    Ok(())
}

/// Verifies a combined detach and add cannot reuse a terminating attachment's only VPC slot,
/// preventing requested removal from releasing capacity before cleanup is observed.
#[crate::sqlx_test]
async fn test_update_instance_retains_service_vpc_slot_during_replacement(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Leave endpoint budget for replacement while limiting the DPU to one VPC slot.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env_with_capacity(
        pool,
        service_vpc_deployment_mock(),
        1,
        2,
    )
    .await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, host_segment_id) = create_fnn_host_segment(
        &env,
        "retained slot host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let host = create_service_vpc_test_host(&env).await?;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let retained_vpc_id = create_fnn_vpc(&env, "retained slot service VPC").await;
    let retained_service = create_networked_dpf_service(&env, "retained-slot-service").await?;
    mark_networked_dpf_service_ready(&env, retained_service.service_id.parse()?).await?;
    let retained_endpoint = seed_networked_attachment(
        &env,
        instance.id,
        host.dpu_ids[0],
        &retained_service,
        retained_vpc_id,
    )
    .await?;
    let replacement_vpc_id = create_fnn_vpc(&env, "replacement slot service VPC").await;
    let replacement_parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        replacement_vpc_id,
        "replacement slot service prefix",
        "10.232.0.0/29".parse()?,
    )
    .await;
    let replacement_service =
        create_networked_dpf_service(&env, "replacement-slot-service").await?;
    mark_networked_dpf_service_ready(&env, replacement_service.service_id.parse()?).await?;
    let baseline = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("retained-slot instance remains stored");

    // Remove the existing attachment and add a different VPC in the same complete request.
    let current = instance.rpc_instance().await.into_inner();
    let mut requested_config = current.config.expect("retained-slot instance has a config");
    requested_config.dpu_extension_services =
        Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: replacement_service.service_id,
                version: replacement_service
                    .latest_version_info
                    .expect("replacement service has a version")
                    .version,
                service_vpc_ids: vec![replacement_vpc_id],
            }],
        });
    let error = crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
        env.api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance.id),
                if_version_match: None,
                config: Some(requested_config),
                metadata: current.metadata,
            })),
    )
    .await
    .expect_err("a terminating endpoint must retain the only service-VPC slot");
    assert_eq!(error.code(), tonic::Code::ResourceExhausted);
    assert!(error.message().contains("no free service-VPC slot"));

    // Fresh reads prove the failed request retained the service-interface record, created no
    // replacement network segment, and did not change `VpcPrefix.status.last_used_prefix`.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("retained-slot instance remains stored after rejection");
    assert_eq!(
        serde_json::to_value(&persisted.config)?,
        serde_json::to_value(&baseline.config)?
    );
    assert_eq!(
        persisted.config.network.service_interfaces,
        vec![retained_endpoint]
    );
    assert_eq!(
        persisted.extension_services_config_version,
        baseline.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        baseline.network_config_version
    );
    assert!(
        db::network_segment::find_by(
            &mut *env.pool.acquire().await?,
            ObjectColumnFilter::One(db::network_segment::VpcColumn, &replacement_vpc_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?
        .is_empty()
    );
    assert_eq!(
        db::vpc_prefix::get_for_allocation_by_ids(
            &mut *env.pool.acquire().await?,
            &[replacement_parent_id],
        )
        .await?
        .pop()
        .expect("replacement parent remains stored")
        .status
        .last_used_prefix,
        None
    );

    Ok(())
}

/// Verifies coordinator admission distinguishes an unknown service VPC from a known soft-deleted
/// VPC so clients receive entity absence only when the selected identifier never existed.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_distinguishes_missing_and_deleted_vpcs(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build one otherwise valid coordinator request whose selected VPC is the only varied input.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, host_segment_id) = create_fnn_host_segment(
        &env,
        "classification host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let host = create_service_vpc_test_host(&env).await?;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let service = create_networked_dpf_service(&env, "vpc-classification-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let deleted_vpc_id = create_fnn_vpc(&env, "deleted coordinator service VPC").await;
    let mut txn = env.db_txn().await;
    let deleted = db::vpc::try_delete(txn.as_mut(), deleted_vpc_id)
        .await?
        .expect("unused VPC can be soft-deleted");
    assert!(deleted.deleted.is_some());
    txn.commit().await?;
    let missing_vpc_id = VpcId::new();
    let mut snapshot_txn = env.db_txn().await;
    let host_snapshot = host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let instance_snapshot = host_snapshot
        .instance
        .clone()
        .expect("host has an instance");

    #[derive(Clone, Copy)]
    enum ExpectedError {
        NotFound,
        Deleted,
    }
    let cases = [
        // An identifier that never existed must retain typed entity-absence semantics.
        ("missing VPC", missing_vpc_id, ExpectedError::NotFound),
        // A durable soft-deleted row is known but unusable and therefore fails admission.
        ("deleted VPC", deleted_vpc_id, ExpectedError::Deleted),
    ];
    for (scenario, selected_vpc_id, expected_error) in cases {
        // Give each case a fresh attachment ID so a rejected row cannot influence the next.
        let (attachment, selection) = coordinator_attachment(&service, selected_vpc_id)?;
        let extension_config = InstanceExtensionServicesConfig {
            service_configs: vec![attachment],
        };
        let request = RequestedInstanceExtensionServicesConfig {
            service_configs: vec![selection],
        };
        let mut txn = db::Transaction::begin(&env.pool).await?;

        // Run the complete pre-gate coordinator so classification comes only from the selected VPC row.
        let error =
            crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
                &env.api,
                &mut txn,
                instance.id,
                &extension_config,
                &request,
                &instance_snapshot.config.network,
                &host_snapshot,
                false,
                Some(&instance_snapshot),
            )
            .await
            .expect_err("invalid service VPC selection must be rejected");
        match expected_error {
            ExpectedError::NotFound => assert!(matches!(
                error,
                crate::CarbideError::NotFoundError {
                    kind: "vpc",
                    id,
                } if id == selected_vpc_id.to_string()
            )),
            ExpectedError::Deleted => assert!(matches!(
                error,
                crate::CarbideError::FailedPrecondition(ref message)
                    if message == &format!("VPC {selected_vpc_id} is deleted")
            )),
        }
        txn.rollback().await?;

        // Fresh reads prove classification created no network segment or Instance address, changed
        // neither `extension_services_config` nor `network_config`, and incremented neither
        // `extension_services_config_version` nor `network_config_version`.
        let address_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM instance_addresses WHERE vpc_id = $1")
                .bind(selected_vpc_id)
                .fetch_one(&env.pool)
                .await?;
        assert_eq!(address_count, 0, "{scenario}");
        let segments = db::network_segment::find_by(
            &mut *env.pool.acquire().await?,
            ObjectColumnFilter::One(db::network_segment::VpcColumn, &selected_vpc_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await?;
        assert!(segments.is_empty(), "{scenario}");
        let persisted = db::instance::find_by_id(&env.pool, instance.id)
            .await?
            .expect("instance remains stored after VPC rejection");
        assert_eq!(
            persisted.config.extension_services, instance_snapshot.config.extension_services,
            "{scenario}"
        );
        assert_eq!(
            persisted.config.network, instance_snapshot.config.network,
            "{scenario}"
        );
        assert_eq!(
            persisted.extension_services_config_version,
            instance_snapshot.extension_services_config_version,
            "{scenario}"
        );
        assert_eq!(
            persisted.network_config_version, instance_snapshot.network_config_version,
            "{scenario}"
        );
    }

    Ok(())
}

/// Verifies the coordinator rejects a Machine that entered HostPlatformConfiguration after planning
/// because the locked reload must recheck Assigned/Ready eligibility.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_revalidates_machine_eligibility_after_locked_reload(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Capture an admission-valid snapshot before a Machine-only transition invalidates it.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "late-machine-fence").await?;
    assert!(matches!(
        fixture.host_snapshot.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    ));
    let mut transition = env.db_txn().await;
    let machine = db::machine::find_one(
        transition.as_mut(),
        &fixture.instance_snapshot.machine_id,
        Default::default(),
    )
    .await?
    .expect("fixture Machine remains stored");
    db::machine::advance(
        &machine,
        transition.as_mut(),
        &ManagedHostState::Assigned {
            instance_state: InstanceState::HostPlatformConfiguration {
                platform_config_state: HostPlatformConfigurationState::CheckHostConfig,
            },
        },
        None,
    )
    .await?;
    transition.commit().await?;

    // Plan from the stale Ready snapshot and require the locked Machine reload to reject it.
    let mut allocation = db::Transaction::begin(&env.pool).await?;
    let error =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await
        .expect_err("a non-Ready locked Machine must fail activation");
    assert!(matches!(
        error,
        crate::CarbideError::FailedPrecondition(ref message)
            if message == &format!(
                "machine {} is no longer eligible for service VPC activation",
                fixture.instance_snapshot.machine_id
            )
    ));
    allocation.rollback().await?;

    // Fresh database reads prove late rejection created no service interface, network segment, or
    // Instance address; changed neither `extension_services_config` nor `network_config`; and
    // incremented neither corresponding configuration version.
    assert_service_vpc_fixture_unmodified(&env, &fixture).await?;

    Ok(())
}

/// Verifies a soft-deleted Instance discovered by the coordinator's locked reload keeps the
/// established deletion diagnostic and cannot create service-interface, network-segment,
/// network-prefix, or Instance-address records from a stale `ManagedHostStateSnapshot`.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_rejects_soft_deleted_locked_instance(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Capture an admission-valid snapshot before a separate transaction marks the Instance deleted.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "late-instance-deletion").await?;
    let mut deletion = env.db_txn().await;
    db::instance::mark_as_deleted(fixture.instance_id, deletion.as_mut()).await?;
    deletion.commit().await?;

    // Plan from the stale live snapshot and require the locked Instance reload to classify deletion.
    let mut allocation = db::Transaction::begin(&env.pool).await?;
    let error =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await
        .expect_err("a soft-deleted locked Instance must reject activation");
    assert!(matches!(
        error,
        crate::CarbideError::InvalidConfiguration(
            model::ConfigValidationError::InstanceDeletionIsRequested
        )
    ));
    allocation.rollback().await?;

    // Fresh database reads prove the deleted row retained its configurations and allocated no service interface.
    assert_service_vpc_fixture_unmodified(&env, &fixture).await?;
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("soft-deleted fixture Instance remains stored");
    assert!(persisted.deleted.is_some());

    Ok(())
}

/// Verifies service-prefix allocation retries only the named overlap constraint while transaction
/// abort SQLSTATEs and unrelated database failures propagate without an in-transaction retry.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_classifies_service_candidate_failures(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    #[derive(Clone, Copy)]
    enum Expected {
        Success,
        Unavailable,
        Sqlstate(&'static str),
    }

    struct Case {
        name: &'static str,
        faults: Vec<crate::instance::service_vpc::ServiceCandidateFault>,
        expected_attempts: usize,
        expected: Expected,
    }

    // Use one complete fixture so every row differs only in the injected PostgreSQL failure.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "candidate-failure").await?;
    let cases = vec![
        // A single exclusion conflict must roll back and retry allocation in a fresh savepoint.
        Case {
            name: "one overlap then success",
            faults: vec![crate::instance::service_vpc::ServiceCandidateFault::PrefixOverlap],
            expected_attempts: 2,
            expected: Expected::Success,
        },
        // Repeated conflicts on the named prefix constraint must stop with retry guidance.
        Case {
            name: "repeated overlap",
            faults: vec![
                crate::instance::service_vpc::ServiceCandidateFault::PrefixOverlap,
                crate::instance::service_vpc::ServiceCandidateFault::PrefixOverlap,
            ],
            expected_attempts: 2,
            expected: Expected::Unavailable,
        },
        // Serialization failure preserves SQLSTATE 40001 without retrying inside the transaction.
        Case {
            name: "serialization failure",
            faults: vec![crate::instance::service_vpc::ServiceCandidateFault::SerializationFailure],
            expected_attempts: 1,
            expected: Expected::Sqlstate("40001"),
        },
        // Deadlock detection preserves SQLSTATE 40P01 without retrying inside the transaction.
        Case {
            name: "deadlock",
            faults: vec![crate::instance::service_vpc::ServiceCandidateFault::Deadlock],
            expected_attempts: 1,
            expected: Expected::Sqlstate("40P01"),
        },
        // Division by zero is unrelated to prefix races and must not be rewritten as unavailable.
        Case {
            name: "unrelated database failure",
            faults: vec![
                crate::instance::service_vpc::ServiceCandidateFault::UnrelatedDatabaseFailure,
            ],
            expected_attempts: 1,
            expected: Expected::Sqlstate("22012"),
        },
    ];

    for case in cases {
        // Run each fault plan in a fresh outer transaction so no row can contaminate the next case.
        let mut allocation = db::Transaction::begin(&env.pool).await?;
        let (result, attempts) =
            crate::instance::service_vpc::with_service_candidate_faults_for_test(
                case.faults,
                crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
                    &env.api,
                    &mut allocation,
                    fixture.instance_id,
                    &fixture.extension_config,
                    &fixture.request,
                    &fixture.instance_snapshot.config.network,
                    &fixture.host_snapshot,
                    false,
                    Some(&fixture.instance_snapshot),
                ),
            )
            .await;
        assert_eq!(attempts, case.expected_attempts, "{}", case.name);

        // Check classification before rolling back any successful allocation performed by the case.
        match (case.expected, result) {
            (Expected::Success, Ok(network)) => {
                assert_eq!(network.service_interfaces.len(), 1, "{}", case.name);
            }
            (Expected::Unavailable, Err(error)) => {
                let status: tonic::Status = error.into();
                assert_eq!(status.code(), tonic::Code::Unavailable, "{}", case.name);
            }
            (Expected::Sqlstate(expected), Err(error)) => {
                assert_eq!(
                    error.pg_sqlstate().as_deref(),
                    Some(expected),
                    "{}",
                    case.name
                );
                let status: tonic::Status = error.into();
                assert_eq!(status.code(), tonic::Code::Internal, "{}", case.name);
            }
            (Expected::Success, Err(error)) => panic!("{} failed unexpectedly: {error}", case.name),
            (_, Ok(_)) => panic!("{} succeeded unexpectedly", case.name),
        }
        allocation.rollback().await?;

        // Every case must leave the persisted fixture and its VPC-prefix capacity untouched.
        assert_service_vpc_fixture_unmodified(&env, &fixture).await?;
    }

    Ok(())
}

/// Verifies default force deletion waits for in-flight activation and then rejects the committed
/// attachment and service interface, preventing an earlier empty-membership decision.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_serializes_with_default_force_delete(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Stage a complete service-interface write while retaining the coordinator's Machine row lock.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "default-force-delete-race").await?;
    let mut activation = db::Transaction::begin(&env.pool).await?;
    let network =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut activation,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await?;
    assert!(matches!(
        db::instance::update_service_network_config(
            activation.as_mut(),
            fixture.instance_id,
            fixture.instance_snapshot.extension_services_config_version,
            &fixture.instance_snapshot.config.extension_services,
            &fixture.extension_config,
            fixture.instance_snapshot.network_config_version,
            &fixture.instance_snapshot.config.network,
            &network,
            true,
        )
        .await?,
        db::ConditionalWrite::Applied(_)
    ));
    let activation_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(activation.as_pgconn())
        .await?;

    // Start the public default decision and verify it waits for activation's Machine row lock.
    let api = env.api.clone();
    let host_id = fixture.instance_snapshot.machine_id;
    let pending_force_delete = TestTaskHandle::new(tokio::spawn(async move {
        api.admin_force_delete_machine(Request::new(service_vpc_force_delete_request(
            host_id, false,
        )))
        .await
    }));
    wait_for_blocked_query(&env.pool, activation_pid, "UPDATE machines").await;
    activation.commit().await?;
    let error = join_test_task(pending_force_delete, "default force deletion")
        .await
        .expect_err("default force deletion must reject the newly committed endpoint");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("retains service VPC endpoints"));

    // Fresh reads prove the attachment, service interface, segment, prefix, and address committed
    // and the Machine did not enter `ForceDeletion`.
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("default rejection retains the service-interface attachment");
    assert_eq!(
        persisted.config.extension_services,
        fixture.extension_config
    );
    assert_eq!(persisted.config.network, network);
    let mut observer = env.db_txn().await;
    let machine = db::machine::find_one(
        &mut *observer,
        &host_id,
        model::machine::machine_search_config::MachineSearchConfig::default(),
    )
    .await?
    .expect("default rejection retains the host Machine");
    assert!(matches!(
        machine.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    ));
    observer.rollback().await?;

    Ok(())
}

/// Verifies explicit orphan force deletion waits for in-flight activation before reclaiming the
/// committed attachment and service interface, preventing records from appearing after deletion.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_serializes_with_orphan_force_delete(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Stage a complete service-interface write while retaining the coordinator's Machine row lock.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let mut dpf_sdk = MockDpfOperations::new();
    dpf_sdk
        .expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    dpf_sdk
        .expect_force_delete_host()
        .times(1)
        .returning(|_, _| Ok(()));
    let env = create_service_vpc_coordinator_test_env_with_dpf_sdk(pool, Arc::new(dpf_sdk)).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "orphan-force-delete-race").await?;
    let mut activation = db::Transaction::begin(&env.pool).await?;
    let network =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut activation,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await?;
    assert!(matches!(
        db::instance::update_service_network_config(
            activation.as_mut(),
            fixture.instance_id,
            fixture.instance_snapshot.extension_services_config_version,
            &fixture.instance_snapshot.config.extension_services,
            &fixture.extension_config,
            fixture.instance_snapshot.network_config_version,
            &fixture.instance_snapshot.config.network,
            &network,
            true,
        )
        .await?,
        db::ConditionalWrite::Applied(_)
    ));
    let [endpoint] = network.service_interfaces.as_slice() else {
        panic!("orphan race activation must stage one endpoint");
    };
    let endpoint = endpoint.clone();
    let activation_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(activation.as_pgconn())
        .await?;

    // Start the public override and verify its decision waits for the same Machine row lock.
    let api = env.api.clone();
    let host_id = fixture.instance_snapshot.machine_id;
    let pending_force_delete = TestTaskHandle::new(tokio::spawn(async move {
        api.admin_force_delete_machine(Request::new(service_vpc_force_delete_request(
            host_id, true,
        )))
        .await
    }));
    wait_for_blocked_query(&env.pool, activation_pid, "UPDATE machines").await;
    activation.commit().await?;
    let response = join_test_task(pending_force_delete, "orphan force deletion")
        .await?
        .into_inner();
    assert!(response.all_done);

    // Fresh reads prove the override removed the Instance and address and marked the segment deleted.
    assert!(
        db::instance::find_by_id(&env.pool, fixture.instance_id)
            .await?
            .is_none()
    );
    assert!(
        db::instance_address::find_all_by_instance_id_and_segment_id(
            &mut *env.pool.acquire().await?,
            &fixture.instance_id,
            &endpoint.network_segment_id,
        )
        .await?
        .is_empty()
    );
    let generated_segment_deleted: bool =
        sqlx::query_scalar("SELECT deleted IS NOT NULL FROM network_segments WHERE id = $1")
            .bind(endpoint.network_segment_id)
            .fetch_one(&env.pool)
            .await?;
    assert!(generated_segment_deleted);

    Ok(())
}

/// Verifies public service admission loads the persisted expected-machine CX9 declaration because
/// BF4 Astra classification must not depend on DPA rows absent from `ManagedHostStateSnapshot`.
#[crate::sqlx_test]
async fn test_update_instance_service_vpc_uses_persisted_expected_machine_astra_classification(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Record the classifier input and return Astra only when the expected-machine CX9 signal is set.
    let expected_machine_signal_seen = Arc::new(AtomicBool::new(false));
    let mut dpf_sdk = MockDpfOperations::new();
    dpf_sdk.expect_deployment_type_for_dpu().returning({
        let expected_machine_signal_seen = Arc::clone(&expected_machine_signal_seen);
        move |_, expected_machine_has_cx9| {
            expected_machine_signal_seen.store(expected_machine_has_cx9, Ordering::Relaxed);
            Ok(if expected_machine_has_cx9 {
                DpuDeploymentType::Bf4Astra
            } else {
                DpuDeploymentType::Bf4Generic
            })
        }
    });
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env_with_dpf_sdk(pool, Arc::new(dpf_sdk)).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, host_segment_id) = create_fnn_host_segment(
        &env,
        "expected-machine Astra host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "expected-machine Astra service VPC").await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        service_vpc_id,
        "expected-machine Astra service prefix",
        "10.230.0.0/29".parse()?,
    )
    .await;
    let host = create_bf4_astra_host(&env, 9).await;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let service = create_networked_dpf_service(&env, "expected-machine-astra-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let baseline = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("Astra fixture instance remains stored");

    // Submit a public attachment so the handler must derive the classifier signal from persistence.
    let current = instance.rpc_instance().await.into_inner();
    let mut requested_config = current.config.expect("Astra fixture has a public config");
    requested_config.dpu_extension_services =
        Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service.service_id,
                version: service
                    .latest_version_info
                    .expect("created service has a version")
                    .version,
                service_vpc_ids: vec![service_vpc_id],
            }],
        });
    let error = crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
        env.api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance.id),
                if_version_match: None,
                config: Some(requested_config),
                metadata: current.metadata,
            })),
    )
    .await
    .expect_err("persisted CX9 classification must reject Astra activation");
    assert!(expected_machine_signal_seen.load(Ordering::Relaxed));
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("uses Astra, which does not support service VPCs")
    );

    // Fresh reads prove classification rejected before storing the attachment, service interface,
    // or `last_used_prefix`.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("Astra fixture instance remains stored after rejection");
    assert_eq!(
        serde_json::to_value(&persisted.config)?,
        serde_json::to_value(&baseline.config)?
    );
    assert_eq!(
        persisted.extension_services_config_version,
        baseline.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        baseline.network_config_version
    );
    let parent =
        db::vpc_prefix::get_for_allocation_by_ids(&mut *env.pool.acquire().await?, &[parent_id])
            .await?
            .pop()
            .expect("service parent remains stored");
    assert_eq!(parent.status.last_used_prefix, None);

    Ok(())
}

/// Verifies an exact retry fails closed after the primary DPU changes because accepted endpoint
/// stored target DPU IDs cannot be reinterpreted after the primary DPU changes.
#[crate::sqlx_test]
async fn test_update_instance_rejects_service_vpc_retry_after_primary_dpu_change(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Seed a Primary-target attachment on the original primary DPU of a two-DPU host.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, host_segment_id) = create_fnn_host_segment(
        &env,
        "primary-history host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "primary-history service VPC").await;
    let host = create_service_vpc_test_host_with_dpu_count(&env, 2).await?;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let service = create_networked_dpf_service_with_target(
        &env,
        "primary-history-service",
        rpc::forge::DpuExtensionServiceDpuTarget::Primary,
    )
    .await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let mut snapshot_txn = env.db_txn().await;
    let original_primary_dpu = host
        .snapshot(&mut snapshot_txn)
        .await
        .host_snapshot
        .primary_attached_dpu_machine_id()
        .expect("two-DPU host has an original primary");
    snapshot_txn.rollback().await?;
    let endpoint = seed_networked_attachment(
        &env,
        instance.id,
        original_primary_dpu,
        &service,
        service_vpc_id,
    )
    .await?;

    // Promote the other DPU-backed interface through the operator API.
    let mut interface_txn = env.db_txn().await;
    let interfaces = db::machine_interface::find_by_machine_ids(interface_txn.as_mut(), &[host.id])
        .await?
        .remove(&host.id)
        .expect("two-DPU host has interface rows");
    interface_txn.rollback().await?;
    let replacement_primary_interface = interfaces
        .iter()
        .find(|interface| {
            !interface.primary_interface
                && interface.attached_dpu_machine_id.is_some()
                && interface.attached_dpu_machine_id != Some(original_primary_dpu)
        })
        .expect("two-DPU host has a non-primary DPU-backed interface")
        .id;
    env.api
        .set_primary_interface(Request::new(rpc::forge::SetPrimaryInterfaceRequest {
            host_machine_id: Some(host.id),
            interface_id: Some(replacement_primary_interface),
            force_reconcile: false,
            ..Default::default()
        }))
        .await?;
    let after_primary_change = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored after primary change");

    // Retry the unchanged attachment and require current target resolution to reject historical drift.
    let current = instance.rpc_instance().await.into_inner();
    let error = env
        .api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(instance.id),
            if_version_match: None,
            config: current.config,
            metadata: current.metadata,
        }))
        .await
        .expect_err("a changed Primary target must reject the retained endpoint retry");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("does not match its immutable stored service interfaces")
    );

    // Fresh database reads prove rejection preserved both configuration versions, the service
    // interface, its address, and `VpcPrefix.status.last_used_prefix`.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored after topology rejection");
    assert_eq!(
        serde_json::to_value(&persisted.config)?,
        serde_json::to_value(&after_primary_change.config)?
    );
    assert_eq!(
        persisted.extension_services_config_version,
        after_primary_change.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        after_primary_change.network_config_version
    );
    assert_eq!(
        persisted.config.network.service_interfaces,
        vec![endpoint.clone()]
    );
    assert_eq!(
        db::instance_address::find_all_by_instance_id_and_segment_id(
            &mut *env.pool.acquire().await?,
            &instance.id,
            &endpoint.network_segment_id,
        )
        .await?
        .len(),
        1
    );
    let parent = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[endpoint.vpc_prefix_id],
    )
    .await?
    .pop()
    .expect("historical endpoint parent remains stored");
    assert_eq!(parent.status.last_used_prefix, Some(endpoint.link_prefix));

    Ok(())
}

/// Verifies an exact retry preserves an accepted endpoint after its host is classified as Astra
/// because topology changes must not strand cleanup or unrelated updates for durable allocations.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_preserves_existing_endpoint_after_astra_reclassification(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Return Astra for the durable expected-machine signal used after the endpoint was accepted.
    let expected_machine_signal_seen = Arc::new(AtomicBool::new(false));
    let mut dpf_sdk = MockDpfOperations::new();
    dpf_sdk.expect_deployment_type_for_dpu().returning({
        let expected_machine_signal_seen = Arc::clone(&expected_machine_signal_seen);
        move |_, expected_machine_has_cx9| {
            expected_machine_signal_seen.store(expected_machine_has_cx9, Ordering::Relaxed);
            Ok(DpuDeploymentType::Bf4Astra)
        }
    });
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env_with_dpf_sdk(pool, Arc::new(dpf_sdk)).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, host_segment_id) = create_fnn_host_segment(
        &env,
        "Astra retry host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "Astra retry service VPC").await;
    let host = create_service_vpc_test_host(&env).await?;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let service = create_networked_dpf_service(&env, "astra-retry-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let endpoint = seed_networked_attachment_with_prefix(
        &env,
        instance.id,
        host.dpu_ids[0],
        &service,
        service_vpc_id,
        0,
        "Astra retry service prefix",
        "10.228.0.0/30".parse()?,
    )
    .await?;
    let mut snapshot_txn = env.db_txn().await;
    let host_snapshot = host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let instance_snapshot = host_snapshot
        .instance
        .clone()
        .expect("Astra retry host has an instance");
    let attachment = instance_snapshot.config.extension_services.service_configs[0].clone();
    let request = RequestedInstanceExtensionServicesConfig {
        service_configs: vec![RequestedInstanceExtensionServiceConfig {
            service_id: attachment.service_id,
            version: attachment.version,
            service_vpc_ids: vec![service_vpc_id],
        }],
    };

    // Retry the exact attachment after reclassification without invoking any new-allocation gate.
    let mut retry_txn = db::Transaction::begin(&env.pool).await?;
    let retry_network =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut retry_txn,
            instance.id,
            &instance_snapshot.config.extension_services,
            &request,
            &instance_snapshot.config.network,
            &host_snapshot,
            true,
            Some(&instance_snapshot),
        )
        .await?;
    assert!(expected_machine_signal_seen.load(Ordering::Relaxed));
    assert_eq!(retry_network, instance_snapshot.config.network);
    retry_txn.rollback().await?;

    // Fresh database reads prove the retry neither replaced the service interface nor incremented
    // `extension_services_config_version` or `network_config_version`.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("Astra retry instance remains stored");
    assert_eq!(persisted.config.network.service_interfaces, vec![endpoint]);
    assert_eq!(
        persisted.extension_services_config_version,
        instance_snapshot.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        instance_snapshot.network_config_version
    );

    Ok(())
}

/// Verifies deployment-family configuration remains an independent admission prerequisite because
/// test-only implementation support must not enable a family disabled for the selected platform.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_rejects_family_disabled_for_deployment(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Configure BF3 with no enabled service-VPC families while keeping every other fixture valid.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let mut config = get_config();
    config.dpf.enabled = true;
    config.site_global_vpc_vni = None;
    config.dpu_config.service_vpc_slot_count = 2;
    config.dpu_config.max_active_service_vpc_interfaces_per_dpu = 2;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families = Default::default();
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config)
            .with_dpf_sdk(service_vpc_deployment_mock())
            .with_fnn_config(None),
    )
    .await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "disabled-family").await?;

    // Open only the implementation test gate and require deployment configuration to reject IPv4.
    let mut allocation = db::Transaction::begin(&env.pool).await?;
    let error =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await
        .expect_err("a disabled deployment family must reject activation");
    assert!(matches!(
        error,
        crate::CarbideError::FailedPrecondition(ref message)
            if message.contains("Ipv4 is disabled for target DPU")
    ));
    allocation.rollback().await?;

    // Fresh reads prove family admission changed neither `extension_services_config` nor
    // `network_config`, incremented neither `extension_services_config_version` nor
    // `network_config_version`, left `update_network_config_request` and
    // `VpcPrefix.status.last_used_prefix` unchanged, and created no network-segment or
    // Instance-address record.
    assert_service_vpc_fixture_unmodified(&env, &fixture).await?;

    Ok(())
}

/// Verifies a DPF registration that has not reached Ready cannot activate endpoints because durable
/// registration convergence is required even when the coordinator implementation gate is bypassed.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_rejects_deferred_registration(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build a valid fixture, then move its service back to Creating to model deferred reconciliation.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "deferred-registration").await?;
    let service_id = fixture.extension_config.service_configs[0].service_id;
    let mut state_txn = env.db_txn().await;
    let service =
        db::extension_service::find_by_ids(state_txn.as_mut(), &[service_id], false, false)
            .await?
            .pop()
            .expect("fixture service remains stored");
    let state_change = service.status.controller_state.version.incremental_change();
    assert_eq!(
        db::extension_service::try_update_controller_state(
            state_txn.as_mut(),
            service_id,
            state_change.current,
            state_change.new,
            &ExtensionServiceLifecycleState::Creating,
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    state_txn.commit().await?;

    // Run the coordinator with family support enabled and require the non-Ready lifecycle state to fail first.
    let mut allocation = db::Transaction::begin(&env.pool).await?;
    let error =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await
        .expect_err("a deferred DPF registration must reject activation");
    assert!(matches!(
        error,
        crate::CarbideError::FailedPrecondition(ref message)
            if message.contains("does not have a converged DPF Helm registration")
    ));
    allocation.rollback().await?;

    // Fresh reads prove registration rejection changed neither `extension_services_config` nor
    // `network_config`, incremented neither `extension_services_config_version` nor
    // `network_config_version`, left `update_network_config_request` and
    // `VpcPrefix.status.last_used_prefix` unchanged, and created no network-segment or
    // Instance-address record.
    assert_service_vpc_fixture_unmodified(&env, &fixture).await?;

    Ok(())
}

/// Verifies a target DPU whose deployment cannot be classified fails closed because family and
/// Astra admission cannot be evaluated safely from an unknown platform result.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_rejects_unknown_dpu_deployment(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Inject one deterministic classifier failure while retaining normal DPF-enabled configuration.
    let mut dpf_sdk = MockDpfOperations::new();
    dpf_sdk
        .expect_deployment_type_for_dpu()
        .returning(|_, _| Err(DpfError::InvalidState("deployment unavailable".to_string())));
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env_with_dpf_sdk(pool, Arc::new(dpf_sdk)).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "unknown-deployment").await?;

    // Attempt activation and require the classifier error to remain an admission failure.
    let mut allocation = db::Transaction::begin(&env.pool).await?;
    let error =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await
        .expect_err("an unclassified target DPU must reject activation");
    assert!(matches!(
        error,
        crate::CarbideError::FailedPrecondition(ref message)
            if message.contains("has no supported DPF deployment")
                && message.contains("deployment unavailable")
    ));
    allocation.rollback().await?;

    // Fresh database reads prove unknown deployment rejected the transaction before allocation.
    assert_service_vpc_fixture_unmodified(&env, &fixture).await?;

    Ok(())
}

/// Verifies an all-DPU attachment evaluates every target's concrete deployment because one enabled
/// BF3 target cannot authorize a generic BF4 target whose family was not enabled.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_rejects_mixed_target_family_support(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Classify one chosen fixture DPU as BF3 and every other target as generic BF4.
    let bf3_target = Arc::new(OnceLock::<DpuMachineId>::new());
    let mut dpf_sdk = MockDpfOperations::new();
    dpf_sdk.expect_deployment_type_for_dpu().returning({
        let bf3_target = Arc::clone(&bf3_target);
        move |dpu, _| {
            Ok(match bf3_target.get() {
                Some(target) if *target != dpu.id => DpuDeploymentType::Bf4Generic,
                _ => DpuDeploymentType::Bf3,
            })
        }
    });
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env_with_dpf_sdk(pool, Arc::new(dpf_sdk)).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let host_segments = env
        .create_vpc_and_tenant_segments_with_vpc_details(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "mixed-deployment host VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .rpc(),
            2,
        )
        .await;
    let host = create_service_vpc_test_host_with_dpu_count(&env, 2).await?;
    bf3_target
        .set(host.dpu_ids[0])
        .expect("mixed deployment target is assigned once");
    let mut locator_txn = env.db_txn().await;
    let host_machine = host.host().db_machine(&mut locator_txn).await;
    let dpu_locators = host
        .dpu_ids
        .iter()
        .map(|dpu_id| {
            host_machine
                .get_device_locator_for_dpu_id(dpu_id)
                .expect("fixture DPU has a device locator")
        })
        .collect_vec();
    locator_txn.rollback().await?;
    let instance = host
        .instance_builer(&env)
        .network(interface_network_config_with_devices(
            &host_segments,
            &dpu_locators,
        ))
        .build()
        .await;
    let service_vpc_id = create_fnn_vpc(&env, "mixed-deployment service VPC").await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        service_vpc_id,
        "mixed-deployment service prefix",
        "10.225.0.0/29".parse()?,
    )
    .await;
    let service = create_networked_dpf_service(&env, "mixed-deployment-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let (attachment, selection) = coordinator_attachment(&service, service_vpc_id)?;
    let mut snapshot_txn = env.db_txn().await;
    let host_snapshot = host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let instance_snapshot = host_snapshot
        .instance
        .clone()
        .expect("mixed-deployment host has an instance");
    let fixture = ServiceVpcCoordinatorFixture {
        instance_id: instance.id,
        parent_id,
        host_snapshot,
        instance_snapshot,
        extension_config: InstanceExtensionServicesConfig {
            service_configs: vec![attachment],
        },
        request: RequestedInstanceExtensionServicesConfig {
            service_configs: vec![selection],
        },
    };

    // Bypass only implementation support and require the disabled generic BF4 target to reject all targets.
    let mut allocation = db::Transaction::begin(&env.pool).await?;
    let error =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await
        .expect_err("one unsupported deployment must reject the all-DPU attachment");
    assert!(matches!(
        error,
        crate::CarbideError::FailedPrecondition(ref message)
            if message.contains("Bf4Generic") && message.contains("Ipv4 is disabled")
    ));
    allocation.rollback().await?;

    // Fresh reads prove the supported target received no service interface, network segment, or
    // Instance address and `VpcPrefix.status.last_used_prefix` remained unchanged.
    assert_service_vpc_fixture_unmodified(&env, &fixture).await?;

    Ok(())
}

/// Verifies an IPv4 registration cannot consume IPv6-only VPC-prefix capacity because family-specific
/// allocation exhaustion must fail instead of silently changing the service interface contract.
#[crate::sqlx_test]
async fn test_service_vpc_coordinator_does_not_fall_back_to_another_prefix_family(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Replace the fixture's IPv4 VPC prefix with active IPv6 capacity in the selected service VPC.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    let fixture =
        create_service_vpc_coordinator_fixture(&env, "allocation-family-fallback").await?;
    let selected_vpc_id = fixture.request.service_configs[0].service_vpc_ids[0];
    let ipv6_parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        selected_vpc_id,
        "IPv6-only fallback candidate",
        "fd42:224::/126".parse()?,
    )
    .await;
    env.api
        .delete_vpc_prefix(Request::new(rpc::forge::VpcPrefixDeletionRequest {
            id: Some(fixture.parent_id),
        }))
        .await?;

    // Request the registered IPv4 interface and require exact-family candidate validation to fail.
    let mut allocation = db::Transaction::begin(&env.pool).await?;
    let error =
        crate::instance::service_vpc::allocate_for_instance_with_supported_families_for_test(
            &env.api,
            &mut allocation,
            fixture.instance_id,
            &fixture.extension_config,
            &fixture.request,
            &fixture.instance_snapshot.config.network,
            &fixture.host_snapshot,
            false,
            Some(&fixture.instance_snapshot),
        )
        .await
        .expect_err("IPv6 capacity must not satisfy an IPv4 service requirement");
    assert!(matches!(
        error,
        crate::CarbideError::FailedPrecondition(ref message)
            if message.contains("has no eligible Ipv4 prefix")
    ));
    allocation.rollback().await?;

    // Fresh reads prove no generated segment, address, or Instance configuration used the IPv6 VPC prefix.
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("fixture instance remains stored");
    assert_eq!(
        persisted.config.extension_services,
        fixture.instance_snapshot.config.extension_services
    );
    assert_eq!(
        persisted.config.network,
        fixture.instance_snapshot.config.network
    );
    assert_eq!(
        persisted.extension_services_config_version,
        fixture.instance_snapshot.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        fixture.instance_snapshot.network_config_version
    );
    let ipv6_parent = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[ipv6_parent_id],
    )
    .await?
    .pop()
    .expect("IPv6-only parent remains stored");
    assert_eq!(ipv6_parent.status.last_used_prefix, None);
    let segments = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &selected_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert!(segments.is_empty());
    let address_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM instance_addresses WHERE vpc_id = $1")
            .bind(selected_vpc_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(address_count, 0);

    Ok(())
}

/// Verifies the public complete-config path rejects a newly shared host and service VPC under the
/// default restrictive policy, proving final overlap admission is wired after request conversion.
#[crate::sqlx_test]
async fn test_update_instance_complete_config_rejects_tenant_service_vpc_overlap(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create one valid attachment and a replacement host segment inside its selected service VPC.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "public-overlap-rejection").await?;
    let selected_vpc_id = fixture.request.service_configs[0].service_vpc_ids[0];
    let overlapping_host_segment = create_tenant_network_segment(
        &env.api,
        Some(selected_vpc_id),
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
        "public overlapping host segment",
        true,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    let request =
        service_vpc_complete_config_update_request(&env, &fixture, overlapping_host_segment, None)
            .await;

    // Open only implementation support and require restrictive overlap policy to stop activation.
    let error = crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
        env.api.update_instance_config(Request::new(request)),
    )
    .await
    .expect_err("the default policy must reject a shared tenant and service VPC");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(error.message().contains("cannot be used by both"));

    // Fresh reads prove overlap rejection left only the pre-existing host segment in that VPC.
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("fixture instance remains stored");
    assert_eq!(
        persisted.config.extension_services,
        fixture.instance_snapshot.config.extension_services
    );
    assert_eq!(
        persisted.config.network,
        fixture.instance_snapshot.config.network
    );
    assert!(persisted.update_network_config_request.is_none());
    assert_eq!(
        persisted.extension_services_config_version,
        fixture.instance_snapshot.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        fixture.instance_snapshot.network_config_version
    );
    let segments = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &selected_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].id, overlapping_host_segment);
    let allocated_host_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut *env.pool.acquire().await?,
        &fixture.instance_id,
        &overlapping_host_segment,
    )
    .await?;
    assert!(allocated_host_addresses.is_empty());
    let parent = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[fixture.parent_id],
    )
    .await?
    .pop()
    .expect("configured service parent remains stored");
    assert_eq!(parent.status.last_used_prefix, None);

    Ok(())
}

/// Verifies a public complete-config update atomically persists a real service endpoint while
/// staging overlapping host networking when the explicit permissive policy is enabled.
#[crate::sqlx_test]
async fn test_update_instance_complete_config_activates_service_vpc_and_stages_host_network(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Enable the explicit overlap policy and prepare one otherwise production-valid attachment.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let mut config = get_config();
    config.dpf.enabled = true;
    config.site_global_vpc_vni = None;
    config.dpu_config.service_vpc_slot_count = 2;
    config.dpu_config.max_active_service_vpc_interfaces_per_dpu = 2;
    config.dpu_config.allow_service_vpc_tenant_overlap = true;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families =
        [model::extension_service::ServiceVpcAddressFamily::Ipv4]
            .into_iter()
            .collect();
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config)
            .with_dpf_sdk(service_vpc_deployment_mock())
            .with_fnn_config(None),
    )
    .await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "public-complete-success").await?;
    let selected_vpc_id = fixture.request.service_configs[0].service_vpc_ids[0];

    // Put the replacement host interface in the selected service VPC to exercise permissive wiring.
    let replacement_host_segment = create_tenant_network_segment(
        &env.api,
        Some(selected_vpc_id),
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
        "public complete replacement host VPC",
        true,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    let request =
        service_vpc_complete_config_update_request(&env, &fixture, replacement_host_segment, None)
            .await;

    // Open implementation support only for this handler future and submit both changes together.
    let response = crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
        env.api.update_instance_config(Request::new(request)),
    )
    .await?
    .into_inner();
    let response_attachment = &response
        .config
        .as_ref()
        .expect("updated instance has a config")
        .dpu_extension_services
        .as_ref()
        .expect("updated instance has extension services")
        .service_configs[0];
    assert_eq!(
        response_attachment.service_vpc_ids,
        fixture.request.service_configs[0].service_vpc_ids
    );

    // Re-read through the public API so the response cannot stand in for committed attachment state.
    let public_persisted = env.one_instance(fixture.instance_id).await.into_inner();
    let public_attachment = &public_persisted
        .config
        .as_ref()
        .expect("persisted instance has a config")
        .dpu_extension_services
        .as_ref()
        .expect("persisted instance has extension services")
        .service_configs[0];
    assert_eq!(
        public_attachment.service_vpc_ids,
        fixture.request.service_configs[0].service_vpc_ids
    );

    // Fresh database reads prove the attachment, service interface, generated network segment,
    // network prefix, Instance address, and `update_network_config_request` committed together.
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("updated instance remains stored");
    let [endpoint] = persisted.config.network.service_interfaces.as_slice() else {
        panic!("complete update must persist one service endpoint");
    };
    let [persisted_attachment] = persisted
        .config
        .extension_services
        .service_configs
        .as_slice()
    else {
        panic!("complete update must persist one service attachment");
    };
    assert_eq!(
        persisted_attachment.service_id,
        fixture.extension_config.service_configs[0].service_id
    );
    assert_eq!(
        persisted_attachment.version,
        fixture.extension_config.service_configs[0].version
    );
    assert_eq!(persisted_attachment.id, Some(endpoint.attachment_id));
    let pending_host_update = persisted
        .update_network_config_request
        .as_ref()
        .expect("host networking must remain staged for reconciliation");
    assert_eq!(
        pending_host_update.new_config.interfaces[0].network_segment_id,
        Some(replacement_host_segment)
    );
    assert_eq!(
        persisted.extension_services_config_version.version_nr(),
        fixture
            .instance_snapshot
            .extension_services_config_version
            .version_nr()
            + 1
    );
    assert_eq!(
        persisted.network_config_version.version_nr(),
        fixture
            .instance_snapshot
            .network_config_version
            .version_nr()
            + 1
    );
    let generated_segments = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &selected_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert_eq!(generated_segments.len(), 2);
    assert!(
        generated_segments
            .iter()
            .any(|segment| segment.id == endpoint.network_segment_id)
    );
    let persisted_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut *env.pool.acquire().await?,
        &fixture.instance_id,
        &endpoint.network_segment_id,
    )
    .await?;
    assert_eq!(persisted_addresses.len(), 1);
    assert_eq!(persisted_addresses[0].prefix, endpoint.link_prefix);
    let parent = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[fixture.parent_id],
    )
    .await?
    .pop()
    .expect("configured service parent remains stored");
    assert_eq!(parent.status.last_used_prefix, Some(endpoint.link_prefix));

    Ok(())
}

/// Verifies a later InfiniBand ownership rejection rolls back a service endpoint allocated earlier
/// in the complete-update transaction, preventing service, network, and InfiniBand configurations
/// from committing independently.
#[crate::sqlx_test]
async fn test_update_instance_complete_config_rolls_back_service_vpc_allocation_on_late_ib_rejection(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build one production-valid service attachment and an IB partition owned by another tenant.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "late-ib-rollback").await?;
    let foreign_tenant = "foreign-service-vpc-ib-tenant";
    create_fixture_tenant(&env, foreign_tenant).await?;
    let foreign_partition = env
        .api
        .create_ib_partition(Request::new(rpc::forge::IbPartitionCreationRequest {
            id: None,
            config: Some(rpc::forge::IbPartitionConfig {
                name: "foreign service-VPC IB partition".to_string(),
                tenant_organization_id: foreign_tenant.to_string(),
                pkey: None,
            }),
            metadata: Some(rpc::Metadata {
                name: "foreign service-VPC IB partition".to_string(),
                description: "late update rejection fixture".to_string(),
                labels: vec![],
            }),
        }))
        .await?
        .into_inner();
    let foreign_partition_id = foreign_partition
        .id
        .expect("created foreign IB partition has an ID");
    let host_segment_id = fixture.instance_snapshot.config.network.interfaces[0]
        .network_segment_id
        .expect("coordinator fixture has a host segment");
    let mut request =
        service_vpc_complete_config_update_request(&env, &fixture, host_segment_id, None).await;
    // Add the foreign partition so IB ownership validation rejects after service allocation.
    request
        .config
        .as_mut()
        .expect("complete update has a config")
        .infiniband = Some(rpc::forge::InstanceInfinibandConfig {
        ib_interfaces: vec![rpc::forge::InstanceIbInterfaceConfig {
            function_type: rpc::forge::InterfaceFunctionType::Physical as i32,
            virtual_function_id: None,
            ib_partition_id: Some(foreign_partition_id),
            device: "MT2910 Family [ConnectX-7]".to_string(),
            vendor: None,
            device_instance: 0,
        }],
    });

    // Service allocation and `update_service_network_config` run before IB ownership validation
    // rejects the request.
    let error = crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
        env.api.update_instance_config(Request::new(request)),
    )
    .await
    .expect_err("a foreign IB partition must reject the complete update");
    assert_eq!(error.code(), tonic::Code::InvalidArgument);
    assert_eq!(
        error.message(),
        format!(
            "IB partition {foreign_partition_id} is not owned by the tenant {}",
            fixture
                .instance_snapshot
                .config
                .tenant
                .tenant_organization_id
        )
    );

    // Fresh database reads prove service-interface allocation rolled back, the InfiniBand
    // configuration and `ib_config_version` remained unchanged, and the
    // unchanged host network staged no update.
    assert_service_vpc_fixture_unmodified(&env, &fixture).await?;
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("late-IB fixture instance remains stored");
    assert_eq!(
        persisted.config.infiniband,
        fixture.instance_snapshot.config.infiniband
    );
    assert_eq!(
        persisted.ib_config_version,
        fixture.instance_snapshot.ib_config_version
    );
    assert!(persisted.update_network_config_request.is_none());

    Ok(())
}

/// Verifies service activation and an opposite host binding serialize so restrictive overlap
/// policy permits only the service use and a fresh host retry observes the committed conflict.
#[crate::sqlx_test]
async fn test_update_instance_serializes_service_activation_with_conflicting_host_binding(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Prepare a valid attachment and a host segment inside its selected service VPC.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    let fixture = create_service_vpc_coordinator_fixture(&env, "activation-host-race").await?;
    let service_vpc_id = fixture.request.service_configs[0].service_vpc_ids[0];
    let overlapping_host_segment = create_tenant_network_segment(
        &env.api,
        Some(service_vpc_id),
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
        "activation race overlapping host segment",
        true,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    let current = env.one_instance(fixture.instance_id).await.into_inner();
    let mut activation_config = current
        .config
        .clone()
        .expect("race fixture has a public config");
    activation_config.dpu_extension_services =
        Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: fixture.extension_config.service_configs[0]
                    .service_id
                    .to_string(),
                version: fixture.extension_config.service_configs[0]
                    .version
                    .to_string(),
                service_vpc_ids: vec![service_vpc_id],
            }],
        });
    let mut host_config = current.config.expect("race fixture has a public config");
    host_config.network = Some(single_interface_network_config(overlapping_host_segment));

    // Hold Instance so activation acquires the exclusive address-table lock before waiting to publish.
    let mut blocker = env.db_txn().await;
    db::instance::find_by_id_for_update(blocker.as_mut(), fixture.instance_id)
        .await?
        .expect("race fixture instance remains stored");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await?;
    let activation_api = env.api.clone();
    let instance_id = fixture.instance_id;
    let activation_metadata = current.metadata.clone();
    let pending_activation = TestTaskHandle::new(tokio::spawn(async move {
        crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
            activation_api.update_instance_config(Request::new(
                rpc::forge::InstanceConfigUpdateRequest {
                    instance_id: Some(instance_id),
                    if_version_match: None,
                    config: Some(activation_config),
                    metadata: activation_metadata,
                },
            )),
        )
        .await
    }));
    let activation_pid = wait_for_blocked_query(&env.pool, blocker_pid, "FOR UPDATE OF i").await;

    // Start the host mutation and verify it waits for activation's exclusive address-table lock.
    let host_api = env.api.clone();
    let host_metadata = current.metadata;
    let pending_host_update = TestTaskHandle::new(tokio::spawn(async move {
        host_api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance_id),
                if_version_match: None,
                config: Some(host_config),
                metadata: host_metadata,
            }))
            .await
    }));
    wait_for_blocked_query(
        &env.pool,
        activation_pid,
        "LOCK TABLE instance_addresses IN ACCESS EXCLUSIVE MODE",
    )
    .await;
    blocker.rollback().await?;
    join_test_task(pending_activation, "service activation").await?;
    let stale_host_error = join_test_task(pending_host_update, "conflicting host update")
        .await
        .expect_err("the stale opposite host mutation must lose");
    assert_eq!(stale_host_error.code(), tonic::Code::FailedPrecondition);

    // Retry from committed state so restrictive overlap policy sees and rejects both uses.
    let current = env.one_instance(fixture.instance_id).await.into_inner();
    let mut retry_config = current
        .config
        .expect("activated instance has a public config");
    retry_config.network = Some(single_interface_network_config(overlapping_host_segment));
    let retry_error = env
        .api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(fixture.instance_id),
            if_version_match: None,
            config: Some(retry_config),
            metadata: current.metadata,
        }))
        .await
        .expect_err("a fresh conflicting host binding must fail overlap admission");
    assert_eq!(retry_error.code(), tonic::Code::FailedPrecondition);
    assert!(retry_error.message().contains("cannot be used by both"));

    // Fresh database reads prove only the service interface and the increments to
    // `extension_services_config_version` and `network_config_version` committed.
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("activation race instance remains stored");
    let [endpoint] = persisted.config.network.service_interfaces.as_slice() else {
        panic!("activation must persist exactly one endpoint");
    };
    assert_eq!(endpoint.vpc_id, service_vpc_id);
    assert!(persisted.update_network_config_request.is_none());
    assert_eq!(
        persisted.config.network.interfaces,
        fixture.instance_snapshot.config.network.interfaces
    );
    assert_eq!(
        persisted.extension_services_config_version.version_nr(),
        fixture
            .instance_snapshot
            .extension_services_config_version
            .version_nr()
            + 1
    );
    assert_eq!(
        persisted.network_config_version.version_nr(),
        fixture
            .instance_snapshot
            .network_config_version
            .version_nr()
            + 1
    );
    assert_eq!(
        db::instance_address::find_all_by_instance_id_and_segment_id(
            &mut *env.pool.acquire().await?,
            &fixture.instance_id,
            &overlapping_host_segment,
        )
        .await?
        .len(),
        0
    );
    let parent = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[fixture.parent_id],
    )
    .await?
    .pop()
    .expect("service parent remains stored");
    assert_eq!(parent.status.last_used_prefix, Some(endpoint.link_prefix));

    Ok(())
}

/// Verifies service termination and a host contraction cannot overwrite one another because the
/// stale host request loses and its fresh retry preserves the terminating attachment and endpoint.
#[crate::sqlx_test]
async fn test_update_instance_serializes_service_termination_with_host_network_update(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build two host interfaces and one independent networked attachment for an unrelated update.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, retained_host_segment) = create_fnn_host_segment(
        &env,
        "termination race retained host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let (_, removed_host_segment) = create_fnn_host_segment(
        &env,
        "termination race removed host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "termination race service VPC").await;
    let host = create_service_vpc_test_host(&env).await?;
    let instance = host
        .instance_builer(&env)
        .network(single_interface_network_config_with_vfs(vec![
            retained_host_segment,
            removed_host_segment,
        ]))
        .build()
        .await;
    let service = create_networked_dpf_service(&env, "termination-race-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let endpoint =
        seed_networked_attachment(&env, instance.id, host.dpu_ids[0], &service, service_vpc_id)
            .await?;
    let current = instance.rpc_instance().await.into_inner();

    // Derive two requests from one Instance read so only service removal and host contraction race.
    let mut termination_config = current
        .config
        .clone()
        .expect("termination race instance has a config");
    termination_config.dpu_extension_services =
        Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![],
        });
    let mut host_config = current
        .config
        .clone()
        .expect("termination race instance has a config");
    host_config
        .network
        .as_mut()
        .expect("termination race instance has host networking")
        .interfaces
        .retain(|interface| interface.network_segment_id == Some(retained_host_segment));

    // Hold Instance so termination queues at `update_service_network_config` before the host
    // contraction starts.
    let mut blocker = env.db_txn().await;
    db::instance::find_by_id_for_update(blocker.as_mut(), instance.id)
        .await?
        .expect("termination race instance remains stored");
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await?;
    let termination_api = env.api.clone();
    let instance_id = instance.id;
    let termination_metadata = current.metadata.clone();
    let pending_termination = TestTaskHandle::new(tokio::spawn(async move {
        termination_api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance_id),
                if_version_match: None,
                config: Some(termination_config),
                metadata: termination_metadata,
            }))
            .await
    }));
    let termination_pid = wait_for_blocked_query(&env.pool, blocker_pid, "UPDATE instances").await;

    // Start the contraction from the same Instance read and prove it queues on the service row held
    // by the earlier termination transaction.
    let host_api = env.api.clone();
    let host_metadata = current.metadata;
    let pending_host_update = TestTaskHandle::new(tokio::spawn(async move {
        host_api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance_id),
                if_version_match: None,
                config: Some(host_config),
                metadata: host_metadata,
            }))
            .await
    }));
    wait_for_blocked_query(
        &env.pool,
        termination_pid,
        "extension_services WHERE id = ANY",
    )
    .await;

    // Release the Instance blocker so termination commits first; the stale contraction must then
    // fail its locked Instance/configuration recheck.
    blocker.rollback().await?;
    join_test_task(pending_termination, "service termination").await?;
    let stale_host_error = join_test_task(pending_host_update, "host contraction")
        .await
        .expect_err("the stale host contraction must lose");
    assert_eq!(stale_host_error.code(), tonic::Code::FailedPrecondition);
    let terminating = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("terminating attachment remains stored");
    let removal_time = terminating.config.extension_services.service_configs[0]
        .removed
        .expect("termination records its cleanup timestamp");

    // Retry the host contraction from a fresh Instance read while preserving the removed attachment
    // and its service-interface records.
    let current = instance.rpc_instance().await.into_inner();
    let mut retry_config = current
        .config
        .expect("terminating instance has a public config");
    retry_config
        .network
        .as_mut()
        .expect("terminating instance has host networking")
        .interfaces
        .retain(|interface| interface.network_segment_id == Some(retained_host_segment));
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(instance.id),
            if_version_match: None,
            config: Some(retry_config),
            metadata: current.metadata,
        }))
        .await?;

    // The retry must retain `removed` and `service_interfaces`, stage the host update request, and
    // preserve the live `network_config_version`.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("termination race instance remains stored after retry");
    assert_eq!(
        persisted.config.extension_services.service_configs[0].removed,
        Some(removal_time)
    );
    assert_eq!(persisted.config.network.service_interfaces, vec![endpoint]);
    assert_eq!(
        persisted.extension_services_config_version,
        terminating.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        terminating.network_config_version
    );
    let pending_host_network = persisted
        .update_network_config_request
        .expect("fresh host contraction remains staged");
    assert_eq!(pending_host_network.new_config.service_interfaces, vec![]);
    assert_eq!(pending_host_network.new_config.interfaces.len(), 1);
    assert_eq!(
        pending_host_network.new_config.interfaces[0].network_segment_id,
        Some(retained_host_segment)
    );

    Ok(())
}

/// Verifies a stale final `config_version` rolls back service allocation and host-update staging,
/// preventing service-interface, network-segment, network-prefix, or Instance-address records or a
/// pending host contraction from escaping together.
#[crate::sqlx_test]
async fn test_update_instance_complete_config_rolls_back_allocations_on_final_cas_failure(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Build a two-interface Instance so contraction bypasses the early expanding-update `config_version` check.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, retained_host_segment) = create_fnn_host_segment(
        &env,
        "public rollback retained host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let (_, removed_host_segment) = create_fnn_host_segment(
        &env,
        "public rollback removed host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "public rollback service VPC").await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        service_vpc_id,
        "public rollback service prefix",
        "10.224.0.0/29".parse()?,
    )
    .await;
    let host = create_service_vpc_test_host(&env).await?;
    let instance = host
        .instance_builer(&env)
        .network(single_interface_network_config_with_vfs(vec![
            retained_host_segment,
            removed_host_segment,
        ]))
        .build()
        .await;
    let service = create_networked_dpf_service(&env, "public-complete-rollback-service").await?;
    let service_id = service.service_id.parse()?;
    mark_networked_dpf_service_ready(&env, service_id).await?;
    let (attachment, selection) = coordinator_attachment(&service, service_vpc_id)?;
    let mut snapshot_txn = env.db_txn().await;
    let host_snapshot = host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let instance_snapshot = host_snapshot
        .instance
        .clone()
        .expect("rollback host has an instance");
    let fixture = ServiceVpcCoordinatorFixture {
        instance_id: instance.id,
        parent_id,
        host_snapshot,
        instance_snapshot,
        extension_config: InstanceExtensionServicesConfig {
            service_configs: vec![attachment],
        },
        request: RequestedInstanceExtensionServicesConfig {
            service_configs: vec![selection],
        },
    };

    // Contract the host network and use the current `config_version` so only a concurrent committed write
    // can make the final whole-config compare-and-swap stale.
    let current = env.one_instance(fixture.instance_id).await.into_inner();
    let mut requested_config = current
        .config
        .expect("fixture instance has a public config");
    let requested_network = requested_config
        .network
        .as_mut()
        .expect("fixture instance has public networking");
    requested_network
        .interfaces
        .retain(|interface| interface.network_segment_id == Some(retained_host_segment));
    requested_config.dpu_extension_services =
        Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: fixture.extension_config.service_configs[0]
                    .service_id
                    .to_string(),
                version: fixture.extension_config.service_configs[0]
                    .version
                    .to_string(),
                service_vpc_ids: vec![service_vpc_id],
            }],
        });
    let baseline_config_version = fixture.instance_snapshot.config_version.version_string();
    let request = rpc::forge::InstanceConfigUpdateRequest {
        instance_id: Some(fixture.instance_id),
        if_version_match: Some(baseline_config_version.clone()),
        config: Some(requested_config),
        metadata: current.metadata,
    };

    // Hold the service row so the API reads `config_version` before pausing ahead of allocation.
    let mut service_lock = env.db_txn().await;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(service_lock.as_mut())
        .await?;
    db::extension_service::find_by_ids(service_lock.as_mut(), &[service_id], false, true).await?;
    let api = env.api.clone();
    let pending_update = TestTaskHandle::new(tokio::spawn(async move {
        crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
            api.update_instance_config(Request::new(request)),
        )
        .await
    }));
    wait_for_blocked_query(&env.pool, blocker_pid, "extension_services").await;

    // Increment only `config_version` after the request's initial read, then let it proceed
    // through service allocation and host staging to the final compare-and-swap.
    let mut concurrent_write = db::Transaction::begin(&env.pool).await?;
    db::instance::update_config(
        concurrent_write.as_mut(),
        fixture.instance_id,
        fixture.instance_snapshot.config_version,
        fixture.instance_snapshot.config.clone(),
        fixture.instance_snapshot.metadata.clone(),
    )
    .await?;
    concurrent_write.commit().await?;
    let concurrent_config_version = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("concurrent whole-config write leaves the instance stored")
        .config_version;
    service_lock.commit().await?;
    let error = join_test_task(pending_update, "final config compare-and-swap")
        .await
        .expect_err("the concurrent whole-config write must reject the complete update");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        format!(
            "an object of type instance was intended to be modified did not have the expected version {}",
            baseline_config_version
        )
    );

    // Fresh public and database reads prove neither attachment nor pending host intent committed.
    let public_persisted = env.one_instance(fixture.instance_id).await.into_inner();
    assert!(
        public_persisted
            .config
            .as_ref()
            .expect("persisted instance has a config")
            .dpu_extension_services
            .is_none()
    );
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("fixture instance remains stored");
    assert_eq!(
        persisted.config.extension_services,
        fixture.instance_snapshot.config.extension_services
    );
    assert_eq!(
        persisted.config.network,
        fixture.instance_snapshot.config.network
    );
    assert!(persisted.update_network_config_request.is_none());
    assert_eq!(
        persisted.extension_services_config_version,
        fixture.instance_snapshot.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        fixture.instance_snapshot.network_config_version
    );
    assert_eq!(persisted.metadata, fixture.instance_snapshot.metadata);
    assert_eq!(persisted.config_version, concurrent_config_version);

    // Allocation tables and both original host-address sets must also reflect complete rollback.
    let selected_vpc_id = fixture.request.service_configs[0].service_vpc_ids[0];
    let generated_segments = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &selected_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert!(generated_segments.is_empty());
    let service_address_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM instance_addresses WHERE vpc_id = $1")
            .bind(selected_vpc_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(service_address_count, 0);
    for segment_id in [retained_host_segment, removed_host_segment] {
        assert_eq!(
            db::instance_address::count_by_segment_id(
                &mut *env.pool.acquire().await?,
                &segment_id,
            )
            .await?,
            1,
        );
    }
    let parent = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[fixture.parent_id],
    )
    .await?
    .pop()
    .expect("configured service parent remains stored");
    assert_eq!(parent.status.last_used_prefix, None);

    Ok(())
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
    let host_machine_id: StableHostMachineId = mh
        .host_snapshot
        .id
        .try_into()
        .expect("newly ingested host should have a stable ID");

    let segment_id = env.create_vpc_and_tenant_segment().await;

    // Try to create an instance, but send in a valid and
    // existing NSG ID that doesn't match the tenant of
    // instance being created.
    // This should fail.
    let _ = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(host_machine_id)
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
                .machine_id(host_machine_id)
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

/// Verifies an active retained endpoint rejects a non-FNN host replacement but permits another FNN
/// path, preventing an unrelated host update from removing the endpoint's required connectivity.
#[crate::sqlx_test]
async fn test_update_instance_revalidates_host_fnn_for_active_retained_endpoint(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create one active endpoint plus FNN and non-FNN replacement host networks on the same DPU.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, initial_host_segment) = create_fnn_host_segment(
        &env,
        "retained endpoint initial host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let (_, replacement_fnn_segment) = create_fnn_host_segment(
        &env,
        "retained endpoint replacement FNN VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[1],
    )
    .await;
    // Use a distinct fixture prefix so overlap validation cannot obscure the non-FNN admission check.
    let replacement_non_fnn_vpc_id = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "retained endpoint replacement non-FNN VPC".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(
                    rpc::forge::VpcVirtualizationType::EthernetVirtualizer as i32,
                )
                .tonic_request(),
        )
        .await?
        .into_inner()
        .id
        .expect("replacement non-FNN VPC has an ID");
    let replacement_non_fnn_segment = create_tenant_network_segment(
        &env.api,
        Some(replacement_non_fnn_vpc_id),
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[2],
        "RETAINED_ENDPOINT_REPLACEMENT_NON_FNN",
        true,
    )
    .await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    let replacement_non_fnn_vpc = db::vpc::find_by_segment(&env.pool, replacement_non_fnn_segment)
        .await?
        .expect("replacement segment belongs to a VPC");
    assert_ne!(
        replacement_non_fnn_vpc.config.network_virtualization_type,
        VpcVirtualizationType::Fnn
    );
    let service_vpc_id = create_fnn_vpc(&env, "retained endpoint service VPC").await;
    let host = create_service_vpc_test_host(&env).await?;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(initial_host_segment)
        .build()
        .await;
    let service = create_networked_dpf_service(&env, "retained-endpoint-host-replacement").await?;
    let endpoint =
        seed_networked_attachment(&env, instance.id, host.dpu_ids[0], &service, service_vpc_id)
            .await?;
    let baseline = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("retained-endpoint instance remains stored");

    // Replace only host networking with a non-FNN segment while retaining the public attachment.
    let current = env.one_instance(instance.id).await.into_inner();
    let mut rejected_config = current
        .config
        .expect("retained-endpoint instance has a config");
    rejected_config.network = Some(single_interface_network_config(replacement_non_fnn_segment));
    let error = crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
        env.api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance.id),
                if_version_match: None,
                config: Some(rejected_config),
                metadata: current.metadata,
            })),
    )
    .await
    .expect_err("an active endpoint must reject a non-FNN replacement host path");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        format!(
            "VPC {} must use FNN virtualization for a service attachment",
            replacement_non_fnn_vpc.id
        )
    );

    // Fresh reads prove rejection preserved `extension_services_config`, `network_config`, both
    // configuration versions, the service interface, generated network segment, Instance address,
    // and `VpcPrefix.status.last_used_prefix` without creating `update_network_config_request`.
    let rejected = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("retained-endpoint instance remains stored after rejection");
    assert_eq!(
        rejected.config.extension_services,
        baseline.config.extension_services
    );
    assert_eq!(rejected.config.network, baseline.config.network);
    assert_eq!(
        rejected.extension_services_config_version,
        baseline.extension_services_config_version
    );
    assert_eq!(
        rejected.network_config_version,
        baseline.network_config_version
    );
    assert!(rejected.update_network_config_request.is_none());
    let generated_segment = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(IdColumn, &endpoint.network_segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?
    .pop()
    .expect("retained endpoint segment remains stored after rejection");
    let [generated_prefix] = generated_segment.prefixes.as_slice() else {
        panic!("retained endpoint segment must keep exactly one prefix");
    };
    assert_eq!(generated_prefix.id, endpoint.network_prefix_id);
    assert_eq!(generated_prefix.prefix, endpoint.link_prefix);
    let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut *env.pool.acquire().await?,
        &instance.id,
        &endpoint.network_segment_id,
    )
    .await?;
    let [retained_address] = retained_addresses.as_slice() else {
        panic!("retained endpoint must keep exactly one normalized address");
    };
    assert_eq!(retained_address.prefix, endpoint.link_prefix);
    assert_eq!(
        retained_address.address,
        db::instance_address::service_address_for_prefix(endpoint.link_prefix)?
    );
    let parent = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[endpoint.vpc_prefix_id],
    )
    .await?
    .pop()
    .expect("retained endpoint parent remains stored after rejection");
    assert_eq!(parent.status.last_used_prefix, Some(endpoint.link_prefix));

    // Retry with a distinct FNN host VPC to prove active endpoint validation permits supported paths.
    let current = env.one_instance(instance.id).await.into_inner();
    let mut accepted_config = current
        .config
        .expect("retained-endpoint instance has a config");
    accepted_config.network = Some(single_interface_network_config(replacement_fnn_segment));
    crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
        env.api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance.id),
                if_version_match: None,
                config: Some(accepted_config),
                metadata: current.metadata,
            })),
    )
    .await?;

    // The successful replacement remains pending host-only work and retains the service-interface record.
    let accepted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("retained-endpoint instance remains stored after FNN replacement");
    assert_eq!(accepted.config.network.service_interfaces, vec![endpoint]);
    assert_eq!(
        accepted.extension_services_config_version,
        baseline.extension_services_config_version
    );
    assert_eq!(
        accepted.network_config_version,
        baseline.network_config_version
    );
    let pending = accepted
        .update_network_config_request
        .expect("FNN host replacement remains staged");
    assert_eq!(
        pending.new_config.interfaces[0].network_segment_id,
        Some(replacement_fnn_segment)
    );
    assert!(pending.new_config.service_interfaces.is_empty());

    Ok(())
}

/// Verifies a non-FNN tenant-network replacement remains valid when every attachment retaining
/// endpoints is terminating, while Core-managed service-interface records remain untouched.
#[crate::sqlx_test]
async fn test_allocate_and_update_network_config_instance(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = get_config();
    config.dpf.enabled = true;
    // Enable the standard FNN profiles required by the service VPC fixture.
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_fnn_config(None),
    )
    .await;
    let (segment_id, segment_id2) = env.create_vpc_and_dual_tenant_segment().await;
    let replacement_vpc = db::vpc::find_by_segment(&env.pool, segment_id2)
        .await
        .expect("replacement VPC lookup succeeds")
        .expect("replacement segment belongs to a VPC");
    assert_ne!(
        replacement_vpc.config.network_virtualization_type,
        VpcVirtualizationType::Fnn
    );
    // Create the tenant required by FNN admission before adding a distinct service VPC prefix.
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID)
        .await
        .unwrap();
    // Use a third fixture prefix so the service VPC cannot overlap either host segment.
    let (service_vpc_id, _) = create_fnn_host_segment(
        &env,
        "pending host-network service VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[2],
    )
    .await;

    // Create a DPF-ingested Ready host whose original segment has no preallocated address.
    let mh = create_managed_host(&env).await;

    let mut txn = env.db_txn().await;
    db::machine::mark_machine_ingestion_done_with_dpf(txn.as_mut(), &mh.id)
        .await
        .unwrap();
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

    // Allocate the baseline instance and prove tenant and host networking have converged.
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

    // Seed one valid attachment, service interface, segment, prefix, and address directly because
    // the production implementation gate rejects
    // activation, then move its attachment to the terminating state exercised below.
    create_fixture_tenant(&env, "best_org").await.unwrap();
    let service = create_networked_dpf_service(&env, "pending-host-network-service")
        .await
        .unwrap();
    let service_interface =
        seed_networked_attachment(&env, tinstance.id, mh.dpu_ids[0], &service, service_vpc_id)
            .await
            .unwrap();
    let mut txn = env.db_txn().await;
    let stored_instance = tinstance.db_instance(&mut txn).await;
    let current_extension_services = stored_instance.config.extension_services.clone();
    let mut seeded_extension_services = current_extension_services.clone();
    seeded_extension_services.service_configs[0].removed = Some(chrono::Utc::now());
    assert_eq!(
        db::instance::update_extension_services_config(
            txn.as_mut(),
            stored_instance.id,
            stored_instance.extension_services_config_version,
            &current_extension_services,
            &seeded_extension_services,
            false,
        )
        .await
        .expect("seed terminating service attachment"),
        db::ConditionalWrite::Applied(())
    );
    txn.commit()
        .await
        .expect("commit terminating service attachment");

    // Build a caller-visible replacement with only tenant networking because
    // hidden service-interface records cannot be submitted through the public API.
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

    // Submit the replacement through the public API without adding Core-managed service interfaces.
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

    // Re-read public status to prove the accepted host replacement remains pending reconciliation.
    let instance = tinstance.rpc_instance().await;

    assert_eq!(
        instance.status().network().configs_synced(),
        rpc::SyncState::Pending
    );

    // Reload the database row to prove live state keeps the record and pending work owns only hosts.
    let mut txn = env.db_txn().await;
    let instance = tinstance.db_instance(&mut txn).await;
    txn.rollback().await.unwrap();

    assert!(instance.update_network_config_request.is_some());
    assert!(
        instance
            .config
            .extension_services
            .active_services()
            .is_empty()
    );
    assert_eq!(
        instance
            .config
            .extension_services
            .terminating_services()
            .len(),
        1
    );
    assert_eq!(
        instance.config.network.service_interfaces,
        vec![service_interface]
    );
    let update_req = instance.update_network_config_request.unwrap();
    let expected = NetworkDetails::NetworkSegment(segment_id2);

    assert_eq!(
        expected,
        update_req.new_config.interfaces[0]
            .network_details
            .clone()
            .unwrap(),
    );
    assert!(update_req.old_config.service_interfaces.is_empty());
    assert!(update_req.new_config.service_interfaces.is_empty());
}

/// Verifies a corrupt service-interface record is reported as an internal error,
/// so a valid public read is not blamed for a response conversion failure.
#[crate::sqlx_test]
async fn test_find_instances_reports_corrupt_service_interface_as_internal(pool: sqlx::PgPool) {
    // Create an ordinary instance so only the seeded service-interface record is invalid.
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    // Seed an active attachment whose service-interface record uses an unsupported ordinal.
    let attachment_id = uuid::Uuid::new_v4();
    let mut txn = env.db_txn().await;
    let stored_instance = instance.db_instance(&mut txn).await;
    let current_extension_services = stored_instance.config.extension_services.clone();
    let mut seeded_extension_services = current_extension_services.clone();
    seeded_extension_services
        .service_configs
        .push(InstanceExtensionServiceConfig {
            id: Some(attachment_id),
            dpu_target: None,
            service_id: ExtensionServiceId::new(),
            version: ConfigVersion::initial(),
            removed: None,
        });
    let mut seeded_network_config = stored_instance.config.network.clone();
    seeded_network_config.service_interfaces = vec![InstanceServiceInterfaceConfig {
        attachment_id,
        interface_ordinal: 1,
        dpu_id: managed_host.dpu_ids[0],
        slot_index: 0,
        vpc_id: VpcId::new(),
        vpc_prefix_id: VpcPrefixId::new(),
        network_segment_id: segment_id,
        network_prefix_id: NetworkPrefixId::new(),
        link_prefix: "192.0.2.0/31".parse().expect("valid service prefix"),
        mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
        internal_uuid: uuid::Uuid::new_v4(),
    }];
    assert_eq!(
        db::instance::update_extension_services_config(
            txn.as_mut(),
            stored_instance.id,
            stored_instance.extension_services_config_version,
            &current_extension_services,
            &seeded_extension_services,
            false,
        )
        .await
        .expect("seed active service attachment"),
        db::ConditionalWrite::Applied(())
    );
    db::instance::update_network_config(
        txn.as_mut(),
        stored_instance.id,
        stored_instance.network_config_version,
        &seeded_network_config,
        false,
    )
    .await
    .expect("seed corrupt service-interface record");
    txn.commit()
        .await
        .expect("commit corrupt service-interface record");

    // A valid request must attribute the response failure to server-owned state.
    let error = env
        .api
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![instance.id],
        }))
        .await
        .expect_err("corrupt service-interface record must fail response conversion");
    assert_eq!(error.code(), tonic::Code::Internal);
    assert!(
        error
            .message()
            .starts_with("internal error: failed to convert managed-host snapshot for machine ")
    );
    assert!(error.message().contains("unsupported interface ordinals"));
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

/// Verifies a combined service/network update locks service rows before the
/// address table and the address table before the Instance, preventing deadlocks
/// with extension-only updates and physical deletion.
#[crate::sqlx_test]
async fn test_combined_service_and_network_update_preserves_deletion_lock_order(pool: PgPool) {
    // Create an instance whose shrinking network update must pass through the
    // address allocator, plus one valid nonnetworked attachment to persist.
    let env = create_test_env(pool).await;
    let (retained_segment_id, removed_segment_id) = env.create_vpc_and_dual_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    let test_instance = managed_host
        .instance_builer(&env)
        .network(single_interface_network_config_with_vfs(vec![
            retained_segment_id,
            removed_segment_id,
        ]))
        .build()
        .await;
    create_fixture_tenant(&env, "best_org").await.unwrap();
    let service = env
        .api
        .create_dpu_extension_service(Request::new(rpc::forge::CreateDpuExtensionServiceRequest {
            deployment_type: 0,
            dpu_target: None,
            service_vpc_interfaces: vec![],
            service_id: None,
            service_name: "combined-update-lock-order".to_string(),
            description: Some("Combined update lock-order fixture".to_string()),
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
            data: create_dpu_extension_service_data("combined-update-lock-order"),
            credential: None,
            observability: None,
        }))
        .await
        .unwrap()
        .into_inner();
    let requested_attachment = rpc::forge::InstanceDpuExtensionServiceConfig {
        service_id: service.service_id,
        version: service
            .latest_version_info
            .expect("created service has an initial version")
            .version,
        // Keeping the service nonnetworked isolates the host-address lock order.
        service_vpc_ids: vec![],
    };

    // Both changes must be present in one public request to exercise the
    // formerly inverted extension-before-address ordering.
    let current = test_instance.rpc_instance().await;
    let mut extension_only_config = current.config().inner().clone();
    extension_only_config.dpu_extension_services =
        Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![requested_attachment.clone()],
        });
    let extension_only_request = rpc::forge::InstanceConfigUpdateRequest {
        if_version_match: None,
        config: Some(extension_only_config.clone()),
        instance_id: Some(test_instance.id),
        metadata: Some(current.metadata().clone()),
    };
    let mut combined_config = extension_only_config;
    combined_config.network = Some(single_interface_network_config(retained_segment_id));
    let combined_request = rpc::forge::InstanceConfigUpdateRequest {
        if_version_match: None,
        config: Some(combined_config),
        instance_id: Some(test_instance.id),
        metadata: Some(current.metadata().clone()),
    };

    // Start the lock-order probe by holding the address rows that physical deletion locks first.
    let mut lock_order_probe_txn = env.db_txn().await;
    sqlx::query(
        "SELECT id FROM instance_addresses
         WHERE instance_id = $1
         ORDER BY id
         FOR UPDATE",
    )
    .bind(test_instance.id)
    .execute(lock_order_probe_txn.as_mut())
    .await
    .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(lock_order_probe_txn.as_mut())
        .await
        .unwrap();
    let api = env.api.clone();
    let pending_combined_update = tokio::spawn(async move {
        api.update_instance_config(Request::new(combined_request))
            .await
    });
    let combined_pid =
        wait_for_blocked_query(&env.pool, blocker_pid, "LOCK TABLE instance_addresses").await;

    // An extension-only request must wait behind the combined request's
    // already-held service lock, without either request owning the Instance.
    let api = env.api.clone();
    let pending_extension_only_update = tokio::spawn(async move {
        api.update_instance_config(Request::new(extension_only_request))
            .await
    });
    wait_for_blocked_query(&env.pool, combined_pid, "extension_services").await;

    // While waiting for the address table, the update must not own the
    // Instance row. This succeeds after the fix and fails with NOWAIT under
    // the inverted order.
    let instance_lock = sqlx::query_scalar::<_, InstanceId>(
        "SELECT id FROM instances WHERE id = $1 FOR UPDATE NOWAIT",
    )
    .bind(test_instance.id)
    .fetch_one(lock_order_probe_txn.as_mut())
    .await;

    // Release the probe's address lock only after checking the Instance row so both queued updates
    // resume under the address-to-Instance lock order established above.
    lock_order_probe_txn.rollback().await.unwrap();
    let updated = pending_combined_update
        .await
        .expect("combined update task should not panic")
        .expect("combined update should complete after the address lock is released")
        .into_inner();
    let extension_only_error = pending_extension_only_update
        .await
        .expect("extension-only update task should not panic")
        .expect_err("stale extension-only update must lose optimistic persistence");
    assert_eq!(
        instance_lock.expect("update must not lock Instance before the address table"),
        test_instance.id,
    );
    assert_eq!(extension_only_error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        extension_only_error.message(),
        format!(
            "extension-service attachments for instance {} changed or the instance no longer exists; read the instance again before updating",
            test_instance.id,
        ),
    );

    // Verify the response and a fresh public read both expose the attachment.
    assert_eq!(
        updated
            .config
            .as_ref()
            .unwrap()
            .dpu_extension_services
            .as_ref()
            .unwrap()
            .service_configs,
        vec![requested_attachment.clone()],
    );
    let persisted = env
        .api
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![test_instance.id],
        }))
        .await
        .unwrap()
        .into_inner()
        .instances
        .pop()
        .expect("updated instance remains visible");
    assert_eq!(
        persisted
            .config
            .unwrap()
            .dpu_extension_services
            .unwrap()
            .service_configs,
        vec![requested_attachment],
    );

    // Internal readback proves the host-network request was also staged.
    let persisted = db::instance::find_by_id(&env.pool, test_instance.id)
        .await
        .unwrap()
        .expect("updated instance remains stored");
    let pending_network = persisted
        .update_network_config_request
        .expect("combined update should stage its network change");
    assert_eq!(
        pending_network
            .new_config
            .interfaces
            .iter()
            .map(|interface| interface.network_segment_id)
            .collect_vec(),
        vec![Some(retained_segment_id)],
    );
}

/// Verifies a combined detachment and host-network update takes the exclusive address-table lock
/// before reading retained endpoint addresses, preventing concurrent updates from deadlocking on a
/// lock upgrade.
#[crate::sqlx_test]
async fn test_networked_detach_locks_address_table_before_persisted_address_read(pool: PgPool) {
    // Build a DPF-managed instance with two host interfaces and one persisted endpoint backed by a
    // generated segment, prefix, and address.
    let mut config = get_config();
    config.dpf.enabled = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let (retained_segment_id, removed_segment_id) = env.create_vpc_and_dual_tenant_segment().await;
    let selected_vpc = db::vpc::find_by_segment(&env.pool, retained_segment_id)
        .await
        .unwrap()
        .expect("created segment belongs to a VPC")
        .id;
    let managed_host = create_managed_host(&env).await;
    let mut txn = env.db_txn().await;
    db::machine::mark_machine_ingestion_done_with_dpf(txn.as_mut(), &managed_host.id)
        .await
        .unwrap();
    txn.commit().await.unwrap();
    let instance = managed_host
        .instance_builer(&env)
        .network(single_interface_network_config_with_vfs(vec![
            retained_segment_id,
            removed_segment_id,
        ]))
        .build()
        .await;
    create_fixture_tenant(&env, "best_org").await.unwrap();
    let service = create_networked_dpf_service(&env, "address-lock-order-service")
        .await
        .unwrap();
    seed_networked_attachment(
        &env,
        instance.id,
        managed_host.dpu_ids[0],
        &service,
        selected_vpc,
    )
    .await
    .unwrap();

    // Hold a table read lock so the combined update stops exactly at its exclusive lock request.
    let mut blocker = env.db_txn().await;
    sqlx::query("LOCK TABLE instance_addresses IN ACCESS SHARE MODE")
        .execute(blocker.as_mut())
        .await
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(blocker.as_mut())
        .await
        .unwrap();
    let current = instance.rpc_instance().await.into_inner();
    let mut requested_config = current.config.expect("instance has a config");
    requested_config.network = Some(single_interface_network_config(retained_segment_id));
    requested_config.dpu_extension_services =
        Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![],
        });
    let request = rpc::forge::InstanceConfigUpdateRequest {
        instance_id: Some(instance.id),
        if_version_match: None,
        config: Some(requested_config),
        metadata: current.metadata,
    };
    let api = env.api.clone();
    let pending_update = TestTaskHandle::new(tokio::spawn(async move {
        api.update_instance_config(Request::new(request)).await
    }));
    let update_pid =
        wait_for_blocked_query(&env.pool, blocker_pid, "LOCK TABLE instance_addresses").await;

    // Reaching the exclusive request without a granted read lock proves there is no lock upgrade.
    let holds_address_read_lock: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1
            FROM pg_locks
            WHERE pid = $1
              AND relation = 'instance_addresses'::regclass
              AND mode = 'AccessShareLock'
              AND granted
        )",
    )
    .bind(update_pid)
    .fetch_one(&env.pool)
    .await
    .unwrap();
    assert!(!holds_address_read_lock);

    // Release the probe and prove the public update completes with detachment staged.
    blocker.rollback().await.unwrap();
    join_test_task(pending_update, "combined detach update")
        .await
        .expect("combined update should resume after the address lock is released");
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .unwrap()
        .expect("instance remains stored after combined update");
    assert!(
        persisted.config.extension_services.service_configs[0]
            .removed
            .is_some()
    );
}

/// Verifies a terminating attachment that retains a service interface rejects same-version
/// reactivation even when VPC selections are omitted, preventing cleanup from racing renewed use.
#[crate::sqlx_test]
async fn test_update_instance_rejects_terminating_networked_reattachment(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Seed one persisted endpoint whose attachment, generated segment, prefix, and address remain
    // during removal.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, host_segment_id) = create_fnn_host_segment(
        &env,
        "networked reattachment host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "networked reattachment service VPC").await;
    let host = create_service_vpc_test_host(&env).await?;
    let instance = host
        .instance_builer(&env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let service = create_networked_dpf_service(&env, "networked-reattachment-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let endpoint =
        seed_networked_attachment(&env, instance.id, host.dpu_ids[0], &service, service_vpc_id)
            .await?;

    // Remove the public attachment while retaining its endpoint for asynchronous cleanup.
    let current = env.one_instance(instance.id).await.into_inner();
    let mut detached_config = current.config.expect("instance has a public config");
    detached_config.dpu_extension_services = Some(rpc::forge::InstanceDpuExtensionServicesConfig {
        service_configs: vec![],
    });
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(instance.id),
            if_version_match: None,
            config: Some(detached_config),
            metadata: current.metadata,
        }))
        .await?;
    let terminating = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("terminating networked attachment remains stored");
    assert!(
        terminating.config.extension_services.service_configs[0]
            .removed
            .is_some()
    );
    assert_eq!(
        terminating.config.network.service_interfaces,
        vec![endpoint]
    );

    // Re-add the exact tuple with the legacy empty selection and require the retained endpoint guard.
    let current = env.one_instance(instance.id).await.into_inner();
    let mut reattached_config = current.config.expect("instance still has a public config");
    reattached_config.dpu_extension_services =
        Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service.service_id.clone(),
                version: service
                    .latest_version_info
                    .as_ref()
                    .expect("created service has a version")
                    .version
                    .clone(),
                service_vpc_ids: vec![],
            }],
        });
    let error = env
        .api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(instance.id),
            if_version_match: None,
            config: Some(reattached_config),
            metadata: current.metadata,
        }))
        .await
        .expect_err("retained service interfaces must prevent networked reactivation");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "a terminating networked extension-service attachment cannot be reactivated; wait for service-interface cleanup and attach it again"
    );

    // Fresh database reads prove rejection preserved the removed attachment, service-interface
    // record, both configurations, and both configuration versions.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("rejected networked attachment remains stored");
    assert_eq!(
        serde_json::to_value(&persisted.config)?,
        serde_json::to_value(&terminating.config)?
    );
    assert_eq!(
        persisted.extension_services_config_version,
        terminating.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        terminating.network_config_version
    );

    Ok(())
}

#[crate::sqlx_test]
async fn test_slaac_network_update_locks_instance_before_releasing_segment(pool: PgPool) {
    let fixture = create_auto_vpc_selection_fixture_with_slaac(pool, true).await;
    let env = &fixture.env;
    let old_prefix_id = create_tenant_overlay_prefix_with_prefix(
        env,
        fixture.vpc_id,
        "old SLAAC prefix",
        "fd42:6455:1::/63".parse().unwrap(),
    )
    .await;
    let new_prefix_id = create_tenant_overlay_prefix_with_prefix(
        env,
        fixture.vpc_id,
        "new SLAAC prefix",
        "fd42:6455:2::/63".parse().unwrap(),
    )
    .await;
    let mh = create_managed_host(env).await;
    let tinstance = mh
        .instance_builer(env)
        .tenant_org(FIXTURE_TENANT_ORG_ID)
        .network(single_interface_network_config_with_vpc_prefix(
            old_prefix_id,
        ))
        .build()
        .await;
    let instance = tinstance.rpc_instance().await;
    let old_segment_id = instance.config().network().interfaces[0]
        .network_segment_id
        .unwrap();
    let mut config = instance.config().inner().clone();
    config.network = Some(single_interface_network_config_with_vpc_prefix(
        new_prefix_id,
    ));
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(tinstance.id),
            config: Some(config),
            metadata: Some(instance.metadata().clone()),
            if_version_match: None,
        }))
        .await
        .unwrap();

    env.run_network_segment_controller_iteration().await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: InstanceState::NetworkConfigUpdate {
                network_config_update_state: NetworkConfigUpdateState::WaitingForConfigSynced,
            },
        },
    )
    .await;
    mh.network_configured(env).await;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: InstanceState::NetworkConfigUpdate {
                network_config_update_state: NetworkConfigUpdateState::ReleaseOldResources,
            },
        },
    )
    .await;

    // SLAAC keeps its generated segment without creating address rows that
    // could serialize cleanup with force deletion.
    let mut txn = env.db_txn().await;
    let pending = tinstance.db_instance(&mut txn).await;
    let request = pending.update_network_config_request.as_ref().unwrap();
    assert!(request.old_config.interfaces[0].ip_addrs.is_empty());
    assert!(
        !request.old_config.interfaces[0]
            .interface_prefixes
            .is_empty()
    );
    let address_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM instance_addresses WHERE instance_id = $1")
            .bind(tinstance.id)
            .fetch_one(txn.as_mut())
            .await
            .unwrap();
    assert_eq!(address_count, 0);
    txn.rollback().await.unwrap();

    // Force deletion locks the Instance before its generated segments. While
    // cleanup waits for that Instance, it must leave the old segment unlocked.
    let mut instance_lock = env.db_txn().await;
    db::instance::find_by_id_for_update(instance_lock.as_mut(), tinstance.id)
        .await
        .unwrap()
        .unwrap();
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(instance_lock.as_mut())
        .await
        .unwrap();
    let probe_segment_lock = async move {
        wait_until_query_blocked_by(
            &env.pool,
            blocker_pid,
            "UPDATE instances SET update_network_config_request=NULL",
        )
        .await;
        let result = sqlx::query_scalar::<_, NetworkSegmentId>(
            "SELECT id FROM network_segments WHERE id = $1 FOR UPDATE NOWAIT",
        )
        .bind(old_segment_id)
        .fetch_one(instance_lock.as_mut())
        .await;
        // Release the Instance even when the probe fails so the controller
        // can finish before we report the assertion.
        instance_lock.rollback().await.unwrap();
        result
    };
    let ((), segment_lock) = tokio::join!(
        env.run_machine_state_controller_iteration(),
        probe_segment_lock,
    );
    segment_lock.expect("cleanup must lock the Instance before its generated segment");

    let mut txn = env.db_txn().await;
    let persisted = tinstance.db_instance(&mut txn).await;
    assert!(persisted.update_network_config_request.is_none());
    let segments = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(IdColumn, &old_segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    let [old_segment] = segments.as_slice() else {
        panic!("expected the released SLAAC segment");
    };
    assert!(old_segment.is_marked_as_deleted());
    assert!(matches!(
        mh.host().db_machine(&mut txn).await.current_state(),
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    ));
    txn.rollback().await.unwrap();
}

/// A removed DPU's failed Admin apply must retain its old network resources,
/// including after `ReleaseOldResources` is persisted.
#[crate::sqlx_test]
async fn test_network_update_waits_for_removed_dpu_admin_ack(pool: PgPool) {
    let fixture = create_auto_vpc_selection_fixture(pool).await;
    let env = &fixture.env;
    let mh = create_managed_host_multi_dpu(env, 2).await;
    let mut network = dual_physical_network_config_with_vpc_prefixes(
        fixture.lower_ipv4_prefix_id,
        fixture.higher_ipv4_prefix_id,
    );
    let mut txn = env.db_txn().await;
    let host = mh.host().db_machine(&mut txn).await;
    for (interface, dpu_id) in network.interfaces.iter_mut().zip(&mh.dpu_ids) {
        let locator = host.get_device_locator_for_dpu_id(dpu_id).unwrap();
        interface.device = Some(locator.device);
        interface.device_instance = locator.device_instance as u32;
    }
    txn.commit().await.unwrap();
    let tinstance = mh
        .instance_builer(env)
        .tenant_org(FIXTURE_TENANT_ORG_ID)
        .network(network)
        .build()
        .await;
    let instance = tinstance.rpc_instance().await;
    let mut config = instance.config().inner().clone();
    let interfaces = &mut config.network.as_mut().unwrap().interfaces;
    let removed_segment_id = interfaces.pop().unwrap().network_segment_id.unwrap();
    let retained_segment_id = interfaces[0].network_segment_id.unwrap();
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(tinstance.id),
            config: Some(config),
            metadata: Some(instance.metadata().clone()),
            if_version_match: None,
        }))
        .await
        .unwrap();
    let waiting = ManagedHostState::Assigned {
        instance_state: InstanceState::NetworkConfigUpdate {
            network_config_update_state: NetworkConfigUpdateState::WaitingForConfigSynced,
        },
    };
    env.run_machine_state_controller_iteration_until_state_matches(&mh.host().id, 10, waiting)
        .await;

    // Find the removed DPU through the config the API actually serves it.
    let mut removed_dpu_id = None;
    for dpu_id in &mh.dpu_ids {
        let response = env
            .api
            .get_managed_host_network_config(Request::new(
                rpc::forge::ManagedHostNetworkConfigRequest {
                    dpu_machine_id: Some(*dpu_id),
                },
            ))
            .await
            .unwrap()
            .into_inner();
        if response.use_admin_network {
            assert!(
                removed_dpu_id.is_none(),
                "only one DPU should lose its interfaces"
            );
            removed_dpu_id = Some(*dpu_id);
            assert!(response.tenant_interfaces.is_empty());
            assert!(response.instance_network_config_version.is_empty());
        } else {
            assert_eq!(response.tenant_interfaces.len(), 1);
            network_configured_with_health(env, dpu_id, None).await;
        }
    }
    let removed_dpu_id = removed_dpu_id.expect("removed DPU must receive Admin config");
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    let pending = snapshot.instance.as_ref().unwrap();
    assert_eq!(pending.observations.network.len(), 2);
    let update_request = pending.update_network_config_request.clone().unwrap();
    let old_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &tinstance.id,
        &removed_segment_id,
    )
    .await
    .unwrap()
    .into_iter()
    .map(|allocation| allocation.address)
    .collect_vec();
    assert_eq!(old_addresses.len(), 1);
    txn.commit().await.unwrap();

    for phase in [
        NetworkConfigUpdateState::WaitingForConfigSynced,
        NetworkConfigUpdateState::ReleaseOldResources,
    ] {
        if phase == NetworkConfigUpdateState::ReleaseOldResources {
            // Establish the normal transition, then lose the acknowledgement
            // before the next iteration handles the persisted release phase.
            network_configured_with_health(env, &removed_dpu_id, None).await;
            env.run_machine_state_controller_iteration().await;
        }
        let expected_state = ManagedHostState::Assigned {
            instance_state: InstanceState::NetworkConfigUpdate {
                network_config_update_state: phase,
            },
        };
        let mut txn = env.db_txn().await;
        assert_eq!(mh.snapshot(&mut txn).await.managed_state, expected_state);
        txn.commit().await.unwrap();

        // Inject the report emitted after a failed Admin apply: both version
        // stamps are absent, although NVUE/uplink health can still be clear.
        env.api
            .record_dpu_network_status(Request::new(rpc::forge::DpuNetworkStatus {
                dpu_machine_id: Some(removed_dpu_id),
                network_config_version: None,
                instance_network_config_version: None,
                network_config_error: Some(
                    "network update failed: injected Admin apply failure".to_string(),
                ),
                dpu_health: Some(
                    health_report::HealthReport::empty("forge-dpu-agent".to_string()).into(),
                ),
                ..Default::default()
            }))
            .await
            .unwrap();
        env.run_machine_state_controller_iteration().await;

        let mut txn = env.db_txn().await;
        let snapshot = mh.snapshot(&mut txn).await;
        assert_eq!(snapshot.managed_state, expected_state);
        assert!(
            matches!(
                &snapshot.host_snapshot.controller_state_outcome,
                Some(PersistentStateHandlerOutcome::Wait { reason, .. })
                    if reason == "Waiting for DPU agent(s) to apply network config and report healthy network"
            ),
            "expected the managed-host version wait, got {:?}",
            snapshot.host_snapshot.controller_state_outcome
        );
        assert!(!snapshot.managed_host_network_config_version_synced());
        let pending = snapshot.instance.as_ref().unwrap();
        // The remaining Instance receipts are current; the missing Admin
        // receipt, not Instance-version mismatch or health, must block cleanup.
        assert_eq!(pending.observations.network.len(), 1);
        assert!(!pending.observations.network.contains_key(&removed_dpu_id));
        assert!(
            pending.observations.network.values().all(|observation| {
                observation.config_version == pending.network_config_version
            })
        );
        assert_eq!(pending.config.network, update_request.new_config);
        assert_eq!(
            pending.update_network_config_request.as_ref(),
            Some(&update_request)
        );
        let addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &tinstance.id,
            &removed_segment_id,
        )
        .await
        .unwrap()
        .into_iter()
        .map(|allocation| allocation.address)
        .collect_vec();
        assert_eq!(addresses, old_addresses);
        let segments = db::network_segment::find_by(
            txn.as_mut(),
            ObjectColumnFilter::One(IdColumn, &removed_segment_id),
            NetworkSegmentSearchConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(segments.len(), 1);
        assert!(!segments[0].is_marked_as_deleted());
        txn.commit().await.unwrap();
    }

    // A lifecycle-blocking alert can arrive after the healthy transition to
    // `ReleaseOldResources`. Current receipts alone must not bypass that alert.
    let mut blocking_health = health_report::HealthReport::empty("forge-dpu-agent".to_string());
    blocking_health
        .alerts
        .push(health_report::HealthProbeAlert {
            id: health_report::HealthProbeId::post_config_check_wait(),
            target: None,
            in_alert_since: None,
            message: "injected health change before resource release".to_string(),
            tenant_message: None,
            classifications: vec![
                health_report::HealthAlertClassification::prevent_host_state_changes(),
            ],
        });
    network_configured_with_health(env, &removed_dpu_id, Some(blocking_health.into())).await;
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    assert!(snapshot.managed_host_network_config_version_synced());
    assert_eq!(
        snapshot.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::NetworkConfigUpdate {
                network_config_update_state: NetworkConfigUpdateState::ReleaseOldResources,
            },
        }
    );
    assert!(
        matches!(
            &snapshot.host_snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Error { err, .. })
                if err.contains("health")
        ),
        "expected the existing health error, got {:?}",
        snapshot.host_snapshot.controller_state_outcome
    );
    assert!(
        snapshot
            .instance
            .as_ref()
            .unwrap()
            .update_network_config_request
            .is_some()
    );
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &removed_segment_id)
            .await
            .unwrap(),
        1
    );
    txn.commit().await.unwrap();

    network_configured_with_health(env, &removed_dpu_id, None).await;
    env.run_machine_state_controller_iteration().await;
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    assert_eq!(
        snapshot.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready
        }
    );
    assert!(snapshot.managed_host_network_config_version_synced());
    assert!(
        snapshot
            .instance
            .as_ref()
            .unwrap()
            .update_network_config_request
            .is_none()
    );
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &removed_segment_id)
            .await
            .unwrap(),
        0
    );
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &retained_segment_id)
            .await
            .unwrap(),
        1
    );
    let segments = db::network_segment::find_by(
        txn.as_mut(),
        ObjectColumnFilter::One(IdColumn, &removed_segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(segments.len(), 1);
    assert!(segments[0].is_marked_as_deleted());
    txn.commit().await.unwrap();
}

#[crate::sqlx_test]
async fn test_allocate_instance_with_multiple_fnn_vpc_prefixes(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let mh = create_managed_host_multi_dpu(&env, 2).await;

    // Register the tenant required by both FNN VPCs used in this allocation scenario.
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID)
        .await
        .unwrap();

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
                .config(InstanceConfig::default_tenant_and_os().network(
                    dual_physical_network_config_with_vpc_prefixes(
                        first_prefix_id,
                        second_prefix_id,
                    ),
                ))
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

/// Verifies host-network promotion releases per-VPC loopbacks only when no
/// remaining host or service membership needs them. This prevents a live
/// loopback from returning to the pool for conflicting reuse.
#[crate::sqlx_test]
#[allow(deprecated)]
async fn test_fnn_vrf_loopback_release_uses_remaining_host_and_service_membership(
    pool: sqlx::PgPool,
) {
    // Enable both per-VPC FNN loopbacks and the DPF service definition used by
    // the retained-membership half of the test.
    let mut config = get_config();
    config.dpf.enabled = true;
    let mut overrides = TestEnvOverrides::with_config(config)
        .with_dpf_sdk(service_vpc_deployment_mock())
        .with_fnn_config(None);
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
    let mh = create_managed_host_with_config(&env, service_vpc_test_host_config()).await;
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
                dpu_machine_id: Some(dpu_id),
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
    // The network field drives loopback release; the remaining config and
    // metadata fields make this a valid full replacement request.
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
    let retained_entry = db::resource_pool::find_value(&mut *txn, &first_loopback)
        .await
        .expect("find retained PF loopback allocation")
        .into_iter()
        .find(|entry| entry.pool_name == env.common_pools.ethernet.pool_vpc_dpu_loopback_ip.name())
        .expect("retained PF loopback pool entry");
    assert_eq!(
        retained_entry.state.0,
        ResourcePoolEntryState::Allocated {
            owner: dpu_id.to_string(),
            owner_type: OwnerType::Machine.to_string(),
        }
    );
    let removed_entry = db::resource_pool::find_value(&mut *txn, &second_loopback)
        .await
        .expect("find removed VF loopback allocation")
        .into_iter()
        .find(|entry| entry.pool_name == env.common_pools.ethernet.pool_vpc_dpu_loopback_ip.name())
        .expect("removed VF loopback must remain in its pool");
    assert_eq!(removed_entry.state.0, ResourcePoolEntryState::Free);
    assert!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &second_vpc)
            .await
            .unwrap()
            .is_none()
    );
    txn.rollback().await.unwrap();

    // A second host keeps the removed host VPC through a service endpoint on
    // its exact DPU, so that DPU/VPC allocation must remain assigned.
    let service_host = create_service_vpc_test_host(&env).await.unwrap();
    let service_dpu_id = service_host.dpu_ids[0];
    let service_instance = service_host
        .instance_builer(&env)
        .network(
            // Both VPCs need host loopbacks before the second becomes service-only.
            single_interface_network_config_with_vfs(vec![first_segment_id, second_segment_id]),
        )
        .build()
        .await;
    let service_network_config = env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(service_dpu_id),
            },
        ))
        .await
        .unwrap()
        .into_inner();
    let service_loopback = service_network_config.tenant_interfaces[1]
        .tenant_vrf_loopback_ip
        .clone()
        .expect("service VPC loopback should be present");

    // Register the service tenant required by the DPF definition.
    create_fixture_tenant(&env, "best_org").await.unwrap();
    let service = create_networked_dpf_service(&env, "loopback-retention-service")
        .await
        .unwrap();
    let service_version = service
        .latest_version_info
        .as_ref()
        .expect("created service has a version")
        .version
        .clone();
    // Retain `service_endpoint.network_segment_id` so the post-update assertion reads the service-owned
    // segment rather than the removed tenant segment.
    let service_endpoint = seed_networked_attachment(
        &env,
        service_instance.id,
        service_dpu_id,
        &service,
        second_vpc,
    )
    .await
    .unwrap();

    // Remove the tenant interface while preserving the existing service-VPC
    // selection through the caller-visible attachment configuration.
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(first_segment_id)),
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                        service_id: service.service_id,
                        version: service_version,
                        // This exact service membership retains only the removed VF's loopback.
                        service_vpc_ids: vec![second_vpc],
                    }],
                }),
                ..Default::default()
            }),
            instance_id: Some(service_instance.id),
            metadata: Some(rpc::Metadata {
                name: "service-retained-fnn-vpc".to_string(),
                description: "tests/instance".to_string(),
                labels: vec![],
            }),
        }))
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_network_config_return_to_ready(&service_host, false)
        .await;

    // The service address and exact DPU/VPC loopback remain allocated, while
    // the unrelated first VPC allocation also remains intact.
    let mut txn = env.db_txn().await;
    assert!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &service_dpu_id, &first_vpc)
            .await
            .unwrap()
            .is_some()
    );
    assert_eq!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &service_dpu_id, &second_vpc)
            .await
            .unwrap()
            .expect("service VPC loopback should remain")
            .loopback_ip
            .to_string(),
        service_loopback,
    );
    let service_entry = db::resource_pool::find_value(&mut *txn, &service_loopback)
        .await
        .expect("find retained service loopback allocation")
        .into_iter()
        .find(|entry| entry.pool_name == env.common_pools.ethernet.pool_vpc_dpu_loopback_ip.name())
        .expect("retained service loopback pool entry");
    assert_eq!(
        service_entry.state.0,
        ResourcePoolEntryState::Allocated {
            owner: service_dpu_id.to_string(),
            owner_type: OwnerType::Machine.to_string(),
        }
    );
    let expected_service_address =
        db::instance_address::service_address_for_prefix(service_endpoint.link_prefix)
            .expect("seeded link prefix has a service address");
    let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &service_instance.id,
        &service_endpoint.network_segment_id,
    )
    .await
    .unwrap();
    assert_eq!(
        retained_addresses
            .iter()
            .map(|address| (address.address, address.prefix, address.vpc_id))
            .collect_vec(),
        vec![(
            expected_service_address,
            service_endpoint.link_prefix,
            second_vpc,
        )],
    );
}

/// Verifies a late loopback-pool release failure rolls back every earlier
/// cleanup write and leaves `ReleaseOldResources` retryable. This prevents a
/// cleared pending request or deleted loopback row from surviving a failed release.
#[crate::sqlx_test]
async fn test_fnn_vrf_loopback_pool_release_failure_rolls_back_and_retries(pool: sqlx::PgPool) {
    // Enable per-VPC loopbacks and create both tenant identities used by the
    // two-VPC fixture.
    let mut overrides = TestEnvOverrides::default().with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().use_vpc_vrf_loopback = true;
    let env = create_test_env_with_overrides(pool, overrides).await;
    for organization_id in [
        // The primary tenant owns the retained VPC.
        FIXTURE_TENANT_ORG_ID,
        // The peer tenant owns the VPC whose loopback release will fail.
        "e65a9d69-39d2-4872-a53e-e5cb87c84e75",
    ] {
        create_fixture_tenant(&env, organization_id).await.unwrap();
    }

    // The retained PF keeps the post-update network valid while the removed VF
    // supplies the exact DPU/VPC loopback whose release will fail.
    let (_, _, first_segment_id, second_vpc, _, second_segment_id) = env
        .create_vpc_and_peer_vpc_with_tenant_segments(
            rpc::forge::VpcVirtualizationType::Fnn,
            rpc::forge::VpcVirtualizationType::Fnn,
        )
        .await;
    let second_vpc = second_vpc.expect("second VPC should be present");
    let mh = create_managed_host(&env).await;
    let dpu_id = mh.dpu_ids[0];
    let tinstance = mh
        .instance_builer(&env)
        .tenant_org(FIXTURE_TENANT_ORG_ID)
        .network(single_interface_network_config_with_vfs(vec![
            first_segment_id,
            second_segment_id,
        ]))
        .build()
        .await;

    // Stage removal of only the second VPC and stop at the persisted cleanup
    // state before any old resource has been released.
    let instance = tinstance.rpc_instance().await;
    let mut requested_config = instance.config().inner().clone();
    requested_config.network = Some(single_interface_network_config(first_segment_id));
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(tinstance.id),
            config: Some(requested_config),
            metadata: Some(instance.metadata().clone()),
            if_version_match: None,
        }))
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: InstanceState::NetworkConfigUpdate {
                network_config_update_state: NetworkConfigUpdateState::WaitingForConfigSynced,
            },
        },
    )
    .await;
    mh.network_configured(&env).await;
    let release_state = ManagedHostState::Assigned {
        instance_state: InstanceState::NetworkConfigUpdate {
            network_config_update_state: NetworkConfigUpdateState::ReleaseOldResources,
        },
    };
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        release_state.clone(),
    )
    .await;

    // Capture the exact pending request and loopback before injecting the
    // failure so rollback is checked against persisted state.
    let loopback_pool_name = env
        .common_pools
        .ethernet
        .pool_vpc_dpu_loopback_ip
        .name()
        .to_string();
    let mut txn = env.db_txn().await;
    let pending_request = tinstance
        .db_instance(&mut txn)
        .await
        .update_network_config_request
        .expect("network update should remain pending before cleanup");
    let loopback_ip = db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &second_vpc)
        .await
        .unwrap()
        .expect("removed VPC should have a loopback before cleanup")
        .loopback_ip;
    let loopback_value = loopback_ip.to_string();
    txn.rollback().await.unwrap();

    // Reject only the target loopback's transition to an unallocated pool row.
    // The loopback-row deletion has already run when this constraint fires.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "ALTER TABLE resource_pool ADD CONSTRAINT reject_vpc_loopback_release
         CHECK (name <> '{loopback_pool_name}' OR value <> '{loopback_value}' OR allocated IS NOT NULL)"
    )))
    .execute(&env.pool)
    .await
    .expect("inject target loopback pool-release failure");
    env.run_machine_state_controller_iteration().await;

    // The failed iteration must retain its state and roll back every earlier
    // write, preserving the pending request, address, and loopback row.
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, release_state);
    assert!(
        matches!(
            &snapshot.host_snapshot.controller_state_outcome,
            Some(PersistentStateHandlerOutcome::Error { err, .. })
                if err.contains("reject_vpc_loopback_release")
        ),
        "expected the injected pool-release error, got {:?}",
        snapshot.host_snapshot.controller_state_outcome,
    );
    let persisted = tinstance.db_instance(&mut txn).await;
    assert_eq!(
        persisted.update_network_config_request.as_ref(),
        Some(&pending_request),
    );
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &second_segment_id)
            .await
            .unwrap(),
        1,
    );
    assert_eq!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &second_vpc)
            .await
            .unwrap()
            .expect("failed release should retain the loopback row")
            .loopback_ip,
        loopback_ip,
    );
    let retained_entry = db::resource_pool::find_value(&mut *txn, &loopback_value)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.pool_name == loopback_pool_name)
        .expect("failed release should retain the pool row");
    assert_eq!(
        retained_entry.state.0,
        ResourcePoolEntryState::Allocated {
            owner: dpu_id.to_string(),
            owner_type: OwnerType::Machine.to_string(),
        }
    );
    txn.rollback().await.unwrap();

    // Remove the transient failure and let a later controller iteration retry
    // the same persisted cleanup state.
    sqlx::query("ALTER TABLE resource_pool DROP CONSTRAINT reject_vpc_loopback_release")
        .execute(&env.pool)
        .await
        .expect("remove injected loopback pool-release failure");
    let ready_state = ManagedHostState::Assigned {
        instance_state: InstanceState::Ready,
    };
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ready_state.clone(),
    )
    .await;

    // The retry must successfully complete each previously rolled-back mutation.
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    assert_eq!(snapshot.managed_state, ready_state);
    assert!(
        tinstance
            .db_instance(&mut txn)
            .await
            .update_network_config_request
            .is_none()
    );
    assert_eq!(
        db::instance_address::count_by_segment_id(&mut txn, &second_segment_id)
            .await
            .unwrap(),
        0,
    );
    assert!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &second_vpc)
            .await
            .unwrap()
            .is_none()
    );
    let released_entry = db::resource_pool::find_value(&mut *txn, &loopback_value)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.pool_name == loopback_pool_name)
        .expect("released loopback should remain represented in its pool");
    assert_eq!(released_entry.state.0, ResourcePoolEntryState::Free);
    txn.rollback().await.unwrap();
}

/// Verifies network updates independently retain and release exact DPU/VPC loopbacks, including
/// when a persisted retained locator no longer resolves. Another DPU's host membership or service
/// endpoint must not retain unrelated pairs, while ambiguous ownership must preserve its VPC on
/// every current DPU.
#[crate::sqlx_test]
async fn test_fnn_vrf_loopback_release_is_exact_across_dpus_and_vpcs(pool: sqlx::PgPool) {
    // Enable per-VPC loopbacks and DPF service registration for the service-owned pair.
    let mut config = get_config();
    config.dpf.enabled = true;
    let mut overrides = TestEnvOverrides::with_config(config)
        .with_dpf_sdk(service_vpc_deployment_mock())
        .with_fnn_config(None);
    overrides.fnn_config.as_mut().unwrap().use_vpc_vrf_loopback = true;
    let env = create_test_env_with_overrides(pool, overrides).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID)
        .await
        .unwrap();

    // Give each VPC one distinct segment per DPU so the initial host config
    // owns all four exact DPU/VPC memberships without duplicate segments.
    let first_vpc_segments = env
        .create_vpc_and_tenant_segments_with_vpc_details(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "exact loopback vpc 1".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .rpc(),
            2,
        )
        .await;
    // Use the next two fixture prefixes for the second VPC so prefix validation
    // cannot obscure exact DPU/VPC ownership.
    let second_vpc = env
        .api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: "exact loopback vpc 2".to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .unwrap()
        .into_inner()
        .id
        .expect("second VPC has an ID");
    let second_vpc_segments = [
        create_tenant_network_segment(
            &env.api,
            Some(second_vpc),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[2],
            "EXACT_LOOPBACK_VPC2_DPU0",
            true,
        )
        .await,
        create_tenant_network_segment(
            &env.api,
            Some(second_vpc),
            FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[3],
            "EXACT_LOOPBACK_VPC2_DPU1",
            true,
        )
        .await,
    ];

    // Give the primary DPU an unrelated FNN host path so its active service endpoint remains
    // eligible while the original DPU/VPC pairs still exercise exact release behavior.
    let primary_dpu_host_vpc = create_fnn_vpc(&env, "exact loopback primary DPU host VPC").await;
    let primary_dpu_host_segment = create_tenant_network_segment(
        &env.api,
        Some(primary_dpu_host_vpc),
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[4],
        "EXACT_LOOPBACK_PRIMARY_DPU_HOST",
        true,
    )
    .await;

    // Reconcile all queued segments before instance allocation so later loopback allocation uses
    // ready DPU/VPC segments.
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    let first_vpc = db::vpc::find_by_segment(&env.pool, first_vpc_segments[0])
        .await
        .unwrap()
        .expect("first segment belongs to a VPC")
        .id;
    // Make the multi-DPU host eligible for the later DPF attachment, then
    // resolve explicit locators so each host interface owns a known DPU/VPC pair.
    let managed_host =
        create_managed_host_with_config(&env, service_vpc_test_host_config().with_dpu_count(2))
            .await;
    let mut txn = env.db_txn().await;
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &managed_host.id)
        .await
        .unwrap();
    let host = managed_host.host().db_machine(&mut txn).await;
    let dpu_locators = managed_host
        .dpu_ids
        .iter()
        .map(|dpu_id| {
            host.get_device_locator_for_dpu_id(dpu_id)
                .expect("fixture DPU has a device locator")
        })
        .collect_vec();
    txn.commit().await.unwrap();

    // `device`, `device_instance`, `network_segment_id`, `function_type`, and
    // `virtual_function_id` drive exact DPU/VPC membership. The
    // remaining fields represent an otherwise empty caller-owned interface.
    let network_interface =
        |locator: &DeviceLocator,
         segment_id: NetworkSegmentId,
         function_type: rpc::InterfaceFunctionType,
         virtual_function_id: Option<u32>| rpc::InstanceInterfaceConfig {
            function_type: function_type as i32,
            network_segment_id: Some(segment_id),
            network_details: None,
            device: Some(locator.device.clone()),
            device_instance: locator.device_instance as u32,
            virtual_function_id,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        };
    let initial_network = rpc::InstanceNetworkConfig {
        interfaces: vec![
            // DPU 0/VPC 1 must later release despite DPU 1 retaining VPC 1.
            network_interface(
                &dpu_locators[0],
                first_vpc_segments[0],
                rpc::InterfaceFunctionType::Physical,
                None,
            ),
            // DPU 0/VPC 2 must later remain solely because of its service endpoint.
            network_interface(
                &dpu_locators[0],
                second_vpc_segments[0],
                rpc::InterfaceFunctionType::Virtual,
                Some(0),
            ),
            // DPU 1/VPC 1 remains a host membership after the update.
            network_interface(
                &dpu_locators[1],
                first_vpc_segments[1],
                rpc::InterfaceFunctionType::Physical,
                None,
            ),
            // DPU 1/VPC 2 must release despite the service endpoint on DPU 0.
            network_interface(
                &dpu_locators[1],
                second_vpc_segments[1],
                rpc::InterfaceFunctionType::Virtual,
                Some(0),
            ),
        ],
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    };
    let instance = managed_host
        .instance_builer(&env)
        .tenant_org(FIXTURE_TENANT_ORG_ID)
        .network(initial_network)
        .build()
        .await;

    // Capture all four pool values before the update so retained and released
    // ownership can be verified independently after reconciliation.
    let initial_memberships = [
        // This pair loses host membership and has no service-interface membership.
        (managed_host.dpu_ids[0], first_vpc),
        // This pair loses host membership but gains service-interface membership.
        (managed_host.dpu_ids[0], second_vpc),
        // This pair keeps host membership.
        (managed_host.dpu_ids[1], first_vpc),
        // This pair loses host membership and must not inherit the other DPU's service-interface membership.
        (managed_host.dpu_ids[1], second_vpc),
    ];
    let mut initial_loopbacks = HashMap::new();
    let mut txn = env.db_txn().await;
    for (dpu_id, vpc_id) in initial_memberships {
        let loopback = db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &vpc_id)
            .await
            .unwrap()
            .expect("initial host membership has a loopback");
        initial_loopbacks.insert((dpu_id, vpc_id), loopback.loopback_ip.to_string());
    }
    assert_eq!(initial_loopbacks.values().collect::<HashSet<_>>().len(), 4);
    txn.rollback().await.unwrap();

    // Persist a DPF service registration and a service interface on DPU 0/VPC 2.
    create_fixture_tenant(&env, "best_org").await.unwrap();
    let service = create_networked_dpf_service_with_target(
        &env,
        "exact-loopback-retention-service",
        rpc::forge::DpuExtensionServiceDpuTarget::Primary,
    )
    .await
    .unwrap();
    let service_version = service
        .latest_version_info
        .as_ref()
        .expect("created service has a version")
        .version
        .clone();
    seed_networked_attachment(
        &env,
        instance.id,
        managed_host.dpu_ids[0],
        &service,
        second_vpc,
    )
    .await
    .unwrap();

    // Keep DPU 1/VPC 1 as exact caller intent while an unrelated FNN path keeps the primary DPU
    // eligible and the service selection independently retains DPU 0/VPC 2.
    let requested_config = rpc::InstanceConfig {
        tenant: Some(rpc::TenantConfig {
            tenant_organization_id: FIXTURE_TENANT_ORG_ID.to_string(),
            ..default_tenant_config()
        }),
        os: Some(default_os_config()),
        network: Some(rpc::InstanceNetworkConfig {
            interfaces: vec![
                // Keep this interface first so the unresolved-locator phase makes VPC 1 ambiguous.
                network_interface(
                    &dpu_locators[1],
                    first_vpc_segments[1],
                    rpc::InterfaceFunctionType::Physical,
                    None,
                ),
                // Keep the primary DPU on an unrelated FNN path without retaining DPU 0/VPC 1.
                network_interface(
                    &dpu_locators[0],
                    primary_dpu_host_segment,
                    rpc::InterfaceFunctionType::Physical,
                    None,
                ),
            ],
            #[allow(deprecated)]
            auto: false,
            auto_config: None,
        }),
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: service.service_id,
                version: service_version,
                // Only DPU 0/VPC 2 has a service endpoint and may be retained by it.
                service_vpc_ids: vec![second_vpc],
            }],
        }),
        ..Default::default()
    };
    let requested_metadata = rpc::Metadata {
        name: "exact-loopback-membership".to_string(),
        description: "tests/instance".to_string(),
        labels: vec![],
    };

    // Stage the request through the public API before replacing only its pending locator to model
    // persisted pre-fix state that cleanup must tolerate.
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            if_version_match: None,
            config: Some(requested_config.clone()),
            instance_id: Some(instance.id),
            metadata: Some(requested_metadata.clone()),
        }))
        .await
        .unwrap();

    // Make the retained locator unresolvable without changing its VPC or allocated resources.
    let mut txn = env.db_txn().await;
    let mut pending_request = instance
        .db_instance(&mut txn)
        .await
        .update_network_config_request
        .expect("the valid network replacement must be pending");
    let unresolved_locator = DeviceLocator {
        // The absent device drives resolution failure while the index keeps the locator explicit.
        device: "removed-bluefield-device".to_string(),
        device_instance: 0,
    };
    pending_request
        .new_config
        .interfaces
        .first_mut()
        .expect("the replacement retains one host interface")
        .device_locator = Some(unresolved_locator);
    sqlx::query("UPDATE instances SET update_network_config_request = $1 WHERE id = $2")
        .bind(sqlx::types::Json(pending_request))
        .bind(instance.id)
        .execute(txn.as_mut())
        .await
        .expect("persist the unresolved pending locator");
    txn.commit().await.unwrap();

    env.run_machine_state_controller_iteration_network_config_return_to_ready(&managed_host, false)
        .await;

    // Reload after reconciliation to prove stale cleanup completed, retained the ambiguous VPC
    // on both current DPUs, and independently released DPU 1/VPC 2.
    let mut txn = env.db_txn().await;
    assert_eq!(
        managed_host.snapshot(&mut txn).await.managed_state,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        }
    );
    assert!(
        instance
            .db_instance(&mut txn)
            .await
            .update_network_config_request
            .is_none()
    );
    assert_eq!(
        managed_host.dpu_ids.len(),
        2,
        "the fixture must prove ambiguous ownership across multiple DPUs"
    );
    for dpu_id in managed_host.dpu_ids.iter().copied() {
        let expected_loopback = &initial_loopbacks[&(dpu_id, first_vpc)];
        assert_eq!(
            db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &first_vpc)
                .await
                .unwrap()
                .expect("ambiguous VPC ownership retains every current DPU loopback")
                .loopback_ip
                .to_string(),
            *expected_loopback,
        );
        let entry = db::resource_pool::find_value(&mut *txn, expected_loopback)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| {
                entry.pool_name == env.common_pools.ethernet.pool_vpc_dpu_loopback_ip.name()
            })
            .expect("ambiguously retained loopback remains in its pool");
        assert_eq!(
            entry.state.0,
            ResourcePoolEntryState::Allocated {
                owner: dpu_id.to_string(),
                owner_type: OwnerType::Machine.to_string(),
            }
        );
    }
    let released_loopback = &initial_loopbacks[&(managed_host.dpu_ids[1], second_vpc)];
    assert!(
        db::vpc_dpu_loopback::find(txn.as_mut(), &managed_host.dpu_ids[1], &second_vpc)
            .await
            .unwrap()
            .is_none()
    );
    let released_entry = db::resource_pool::find_value(&mut *txn, released_loopback)
        .await
        .unwrap()
        .into_iter()
        .find(|entry| entry.pool_name == env.common_pools.ethernet.pool_vpc_dpu_loopback_ip.name())
        .expect("exactly released loopback remains represented in its pool");
    assert_eq!(released_entry.state.0, ResourcePoolEntryState::Free);
    txn.rollback().await.unwrap();

    // Restore the valid DPU 1/VPC 1 locator through the public API so the original exact-release
    // contract remains covered after stale cleanup.
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            if_version_match: None,
            config: Some(requested_config),
            instance_id: Some(instance.id),
            metadata: Some(requested_metadata),
        }))
        .await
        .unwrap();
    env.run_machine_state_controller_iteration_network_config_return_to_ready(&managed_host, false)
        .await;

    // Each retained pair from the original four-pair matrix keeps its row and exact pool owner.
    let retained_memberships = [
        // Host membership on DPU 1 retains VPC 1.
        (managed_host.dpu_ids[1], first_vpc),
        // Service membership on DPU 0 retains VPC 2.
        (managed_host.dpu_ids[0], second_vpc),
    ];
    let mut txn = env.db_txn().await;
    for (dpu_id, vpc_id) in retained_memberships {
        let expected_loopback = &initial_loopbacks[&(dpu_id, vpc_id)];
        assert_eq!(
            db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &vpc_id)
                .await
                .unwrap()
                .expect("retained membership keeps its loopback")
                .loopback_ip
                .to_string(),
            *expected_loopback,
        );
        let entry = db::resource_pool::find_value(&mut *txn, expected_loopback)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| {
                entry.pool_name == env.common_pools.ethernet.pool_vpc_dpu_loopback_ip.name()
            })
            .expect("retained loopback remains in its pool");
        assert_eq!(
            entry.state.0,
            ResourcePoolEntryState::Allocated {
                owner: dpu_id.to_string(),
                owner_type: OwnerType::Machine.to_string(),
            }
        );
    }

    // Each unrelated released pair loses its row and returns its pool value.
    let released_memberships = [
        // DPU 0/VPC 1 is not retained by DPU 1's host membership.
        (managed_host.dpu_ids[0], first_vpc),
        // DPU 1/VPC 2 is not retained by DPU 0's service endpoint.
        (managed_host.dpu_ids[1], second_vpc),
    ];
    for (dpu_id, vpc_id) in released_memberships {
        let released_loopback = &initial_loopbacks[&(dpu_id, vpc_id)];
        assert!(
            db::vpc_dpu_loopback::find(txn.as_mut(), &dpu_id, &vpc_id)
                .await
                .unwrap()
                .is_none()
        );
        let entry = db::resource_pool::find_value(&mut *txn, released_loopback)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| {
                entry.pool_name == env.common_pools.ethernet.pool_vpc_dpu_loopback_ip.name()
            })
            .expect("released loopback remains represented in its pool");
        assert_eq!(entry.state.0, ResourcePoolEntryState::Free);
    }
    txn.rollback().await.unwrap();
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
    let env =
        create_test_env_with_overrides(pool, TestEnvOverrides::default().with_fnn_config(None))
            .await;
    let mh = create_managed_host(&env).await;

    // Register the tenant required by both FNN VPCs used in this validation scenario.
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID)
        .await
        .unwrap();

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
                .config(InstanceConfig::default_tenant_and_os().network(
                    rpc::InstanceNetworkConfig {
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
                    },
                ))
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
        host_machine_id: mh.id.into(),
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
    let mut txn = env.pool.begin().await.unwrap();
    db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &managed_host.id)
        .await
        .unwrap();
    txn.commit().await.unwrap();
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

/// Verifies a non-DPF host uses its configured instance VF inventory even at a DPF-enabled site.
#[crate::sqlx_test]
async fn test_instance_vf_inventory_rejects_unavailable_vf_on_create_and_update(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    // The site topology selects VF14, while the non-DPF HBN inventory exposes only VF0-VF13.
    // Per-host DPF state must choose the HBN inventory for this host.
    let mut config = crate::test_support::default_config::with_dpf_intercept_topology(&[14]);
    config.dpu_config.num_of_vfs = 16;
    config
        .vmaas_config
        .as_mut()
        .expect("the DPF topology config includes VMaaS")
        .hbn_reps = Some("pf0hpf,pf0vf0-pf0vf13,pf1hpf".to_string());
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let managed_host = create_managed_host(&env).await;
    let segment_ids = env.create_vpc_and_tenant_segments(2).await;

    // VF14 is supported by the hardware population but absent from the configured inventory.
    let mut network_with_unavailable_vf =
        single_interface_network_config_with_vfs(segment_ids.clone());
    network_with_unavailable_vf.interfaces[1].virtual_function_id = Some(14);
    let create_error = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(managed_host.id)
                .config(
                    InstanceConfig::default_tenant_and_os()
                        .network(network_with_unavailable_vf.clone()),
                )
                .tonic_request(),
        )
        .await
        .expect_err("an unavailable VF must not be allocated");
    assert!(create_error.message().contains(
        "virtual function VF14 is not available in the configured instance VF inventory"
    ));

    // A PF-only instance remains valid; adding VF14 must fail without staging the replacement.
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
                    network: Some(network_with_unavailable_vf),
                    infiniband: None,
                    network_security_group_id: None,
                    dpu_extension_services: None,
                    nvlink: None,
                    spxconfig: None,
                    power_profile: None,
                }),
                metadata: Some(rpc::forge::Metadata {
                    name: "unavailable-vf-update".to_string(),
                    description: String::new(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("an unavailable VF must not be staged");
    assert!(update_error.message().contains(
        "virtual function VF14 is not available in the configured instance VF inventory"
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

/// Verifies older callers that omit VF IDs use sparse HBN inventory on create and update.
#[crate::sqlx_test]
async fn test_implicit_instance_vfs_follow_sparse_hbn_inventory_on_create_and_update(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = get_config();
    config.dpu_config.num_of_vfs = 16;
    config
        .vmaas_config
        .as_mut()
        .expect("the default test configuration includes VMaaS")
        .hbn_reps = Some("pf0hpf,pf0vf2,pf0vf5,pf1hpf".to_string());
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let create_host = create_managed_host(&env).await;
    let update_host = create_managed_host(&env).await;
    let segment_ids = env.create_vpc_and_tenant_segments(3).await;

    let implicit_network = || {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        for interface in &mut network.interfaces {
            if interface.function_type() == rpc::InterfaceFunctionType::Virtual {
                interface.virtual_function_id = None;
            }
        }
        network
    };

    let created_instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(create_host.id)
                .config(InstanceConfig::default_tenant_and_os().network(implicit_network()))
                .tonic_request(),
        )
        .await
        .expect("implicit VFs from a sparse inventory must be allocated")
        .into_inner();
    let created_vfs = created_instance
        .config
        .expect("the allocated instance includes its config")
        .network
        .expect("the allocated instance includes its network config")
        .interfaces
        .into_iter()
        .filter(|interface| interface.function_type() == rpc::InterfaceFunctionType::Virtual)
        .map(|interface| {
            interface
                .virtual_function_id
                .expect("NICo resolves every implicit VF ID")
        })
        .collect_vec();
    assert_eq!(created_vfs, vec![2, 5]);

    let update_instance = update_host
        .instance_builer(&env)
        .single_interface_network_config(segment_ids[0])
        .build()
        .await;
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: update_instance.rpc_instance().await.rpc_id(),
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(implicit_network()),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            metadata: Some(rpc::forge::Metadata {
                name: "implicit-vf-update".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .expect("an update with implicit VFs must use the sparse inventory");

    let mut txn = env.db_txn().await;
    let staged_update = update_instance
        .db_instance(&mut txn)
        .await
        .update_network_config_request
        .expect("the replacement network config must be staged");
    let staged_vfs = staged_update
        .new_config
        .interfaces
        .iter()
        .filter_map(|interface| match &interface.function_id {
            InterfaceFunctionId::Physical {} => None,
            InterfaceFunctionId::Virtual { id } => Some(*id),
        })
        .collect_vec();
    assert_eq!(staged_vfs, vec![2, 5]);
    txn.rollback().await.unwrap();
}

/// Verifies an unchanged sparse-VF network with omitted wire IDs remains a network no-op,
/// because legacy callers must not acquire expansion-only locks for metadata updates.
#[crate::sqlx_test]
async fn test_implicit_sparse_vf_noop_update_bypasses_overlap_lock(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    // Configure a sparse inventory so converter placeholders differ from the assigned VF IDs.
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = get_config();
    config.dpu_config.num_of_vfs = 16;
    config
        .vmaas_config
        .as_mut()
        .expect("the default test configuration includes VMaaS")
        .hbn_reps = Some("pf0hpf,pf0vf2,pf0vf5,pf1hpf".to_string());
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let host = create_managed_host(&env).await;
    let segment_ids = env.create_vpc_and_tenant_segments(3).await;
    let implicit_network = || {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        for interface in &mut network.interfaces {
            if interface.function_type() == rpc::InterfaceFunctionType::Virtual {
                interface.virtual_function_id = None;
            }
        }
        network
    };

    // Allocate the active instance through the legacy wire shape so it stores sparse VFs 2 and 5.
    let instance = host
        .instance_builer(&env)
        .config(
            InstanceConfig::default_tenant_and_os()
                .network(implicit_network())
                .into(),
        )
        .build()
        .await;

    // Re-submit the full network without VF IDs while changing only metadata.
    let before = instance.rpc_instance().await.into_inner();
    let mut update_config = before
        .config
        .clone()
        .expect("the instance includes its config");
    for interface in &mut update_config
        .network
        .as_mut()
        .expect("the instance includes its network config")
        .interfaces
    {
        if interface.function_type() == rpc::InterfaceFunctionType::Virtual {
            interface.virtual_function_id = None;
        }
    }
    let mut updated_metadata = before
        .metadata
        .clone()
        .expect("the instance includes its metadata");
    updated_metadata.description = "metadata-only sparse VF update".to_string();

    // Hold the expansion lock; the unchanged resolved network must complete without waiting on it.
    let mut blocker = env.db_txn().await;
    db::tenant_prefix_overlap::lock_checks(&mut blocker)
        .await
        .unwrap();
    let updated = tokio::time::timeout(
        Duration::from_secs(10),
        env.api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: before.id,
                if_version_match: None,
                config: Some(update_config),
                metadata: Some(updated_metadata.clone()),
            })),
    )
    .await
    .expect("an unchanged sparse-VF update must bypass the overlap lock")
    .expect("an unchanged sparse-VF update must succeed")
    .into_inner();
    blocker.rollback().await.unwrap();

    // Confirm only metadata changed and no network replacement was staged.
    assert_eq!(updated.metadata.as_ref(), Some(&updated_metadata));
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await
        .unwrap()
        .expect("the instance remains persisted");
    assert_eq!(
        persisted.network_config_version.to_string(),
        before.network_config_version
    );
    assert!(persisted.update_network_config_request.is_none());
    assert_eq!(
        persisted
            .config
            .network
            .interfaces
            .iter()
            .filter_map(|interface| match &interface.function_id {
                InterfaceFunctionId::Physical {} => None,
                InterfaceFunctionId::Virtual { id } => Some(*id),
            })
            .collect_vec(),
        vec![2, 5]
    );
}

/// Builds a BF4 service-VPC fixture through ordinary ingestion so persisted CX9 classification
/// can be tested without DPA rows or Kubernetes DPF provisioning.
async fn create_bf4_astra_host(env: &TestEnv, cx9_mac_suffix: u8) -> TestManagedHost {
    let host = create_managed_host_with_config(env, service_vpc_test_host_config()).await;
    let mut txn = env.db_txn().await;

    // Mark the attached DPU as BF4, which combines with the CX9 declaration to select Astra.
    let dpu = host.dpu().db_machine(&mut txn).await;
    let mut hardware_info = dpu
        .status
        .hardware_info
        .expect("the fixture DPU includes hardware information");
    hardware_info
        .dmi_data
        .as_mut()
        .expect("the fixture DPU includes DMI information")
        .product_name = "BlueField-4 SmartNIC Main Card".to_string();
    // Reopen topology ingestion so the persisted BF4 product mutation reaches host classification.
    db::machine_topology::set_topology_update_needed(txn.as_mut(), &dpu.id, true)
        .await
        .unwrap();
    db::machine_topology::create_or_update(txn.as_mut(), &dpu.id, &hardware_info)
        .await
        .unwrap();
    // Record DPF ingestion after the BF4 topology update so admission reads the updated product.
    db::machine::mark_machine_ingestion_done_with_dpf(txn.as_mut(), &host.id)
        .await
        .unwrap();

    // Declare CX9 in expected-machine data without creating a site-gated Astra DPA row.
    let host_machine = host.host().db_machine(&mut txn).await;
    let bmc_mac = host_machine
        .status
        .bmc_info
        .mac
        .expect("the fixture host includes a BMC MAC");
    let mut expected_machine = db::expected_machine::find_by_bmc_mac_address(txn.as_mut(), bmc_mac)
        .await
        .unwrap()
        .expect("the fixture host includes an expected-machine declaration");
    // Keep parallel hosts distinct while adding the expected-machine signal used with the
    // persisted BF4 product.
    expected_machine.data.interfaces.push(ExpectedInterface {
        mac_address: Some(mac_address::MacAddress::from([
            0x02,
            0,
            0,
            0,
            0,
            cx9_mac_suffix,
        ])),
        nic_type: Some("CX9".to_string()), // Drives profile classification with the DPU product.
        ..ExpectedInterface::default()
    });
    db::expected_machine::update(txn.as_mut(), &expected_machine)
        .await
        .unwrap();

    // Expected-machine CX9 remains the sole Astra signal while the site-gated row is absent.
    let astra_interfaces = db::dpa_interface::find_by_machine_id(
        txn.as_mut(),
        *host.id,
        model::dpa_interface::DpaSearchConfig {
            only_svpc: false,
            only_astra: true,
        },
    )
    .await
    .unwrap();
    assert!(astra_interfaces.is_empty());
    txn.commit().await.unwrap();

    host
}

/// Verifies topology-free BF4+CX9 hosts reject VF14 through public create and update APIs.
/// Generic DPF accepts this structurally valid VF without topology, so this is an Astra-specific
/// discriminator that protects both handlers' independent inventory lookups.
#[crate::sqlx_test]
async fn test_bf4_cx9_topology_free_public_instance_paths_use_astra_static_inventory(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    // Enable managed-host admission and keep VF14 structurally valid, but remove configured
    // topology so Astra's fixed static inventory is authoritative.
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = crate::test_support::default_config::get();
    config.dpf.enabled = true;
    config.dpu_config.num_of_vfs = 16;
    config.vmaas_config = None;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let create_host = create_bf4_astra_host(&env, 3).await;
    let update_host = create_bf4_astra_host(&env, 4).await;
    let segment_ids = env.create_vpc_and_tenant_segments(2).await;

    // Override only the VF ID: VF14 is valid for generic DPF but absent from Astra's static inventory.
    let vf14_network = || {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        network.interfaces[1].virtual_function_id = Some(14);
        network
    };

    // The create path must select Astra and reject VF14 from its static VF0 through VF13 inventory.
    let create_error = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(create_host.id)
                .config(InstanceConfig::default_tenant_and_os().network(vf14_network()))
                .tonic_request(),
        )
        .await
        .expect_err("topology-free BF4 Astra creation must reject VF14");

    // Establish an assigned Ready instance so update exercises its independent host lookup.
    let update_instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(update_host.id)
                .config(
                    InstanceConfig::default_tenant_and_os()
                        .network(single_interface_network_config(segment_ids[0])),
                )
                .tonic_request(),
        )
        .await
        .expect("the update fixture instance must be allocated")
        .into_inner();
    let update_instance_id = update_instance
        .id
        .expect("the allocated update fixture includes its ID");
    // Network updates require an assigned Ready host; DPF convergence is outside this contract.
    let mut txn = env.db_txn().await;
    db::machine::update_state(
        txn.as_mut(),
        &update_host.id,
        &ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // The update path must make the same Astra classification and reject the same VF.
    let update_error = env
        .api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(update_instance_id),
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(vf14_network()),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            metadata: Some(rpc::forge::Metadata {
                name: "bf4-astra-static-vf-update".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .expect_err("topology-free BF4 Astra update must reject VF14");

    // The Astra-specific diagnostic proves neither handler silently took generic DPF behavior.
    for error in [create_error, update_error] {
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(
            error
                .message()
                .contains("not available in the BF4 Astra static VF inventory")
        );
    }
}

/// Verifies BF4 Astra carries one sparse intercept-topology VF through public create and update.
/// This protects topology wiring; the topology-free sibling above supplies the class discriminator.
#[crate::sqlx_test]
async fn test_bf4_astra_implicit_instance_vfs_use_intercept_topology_on_create_and_update(
    _: PgPoolOptions,
    options: PgConnectOptions,
) {
    // Configure only VF14 so sequential allocation would expose admission/provisioning drift;
    // the remaining host and segment fixtures provide independent create and update baselines.
    let pool = PgPoolOptions::new().connect_with(options).await.unwrap();
    let mut config = crate::test_support::default_config::with_dpf_intercept_topology(&[14]);
    config.dpu_config.num_of_vfs = 16;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let create_host = create_bf4_astra_host(&env, 1).await;
    let update_host = create_bf4_astra_host(&env, 2).await;
    let segment_ids = env.create_vpc_and_tenant_segments(2).await;

    // Clear the otherwise-complete fixture's VF identity so create and update must allocate from
    // the configured sparse topology while retaining the remaining network setup.
    let implicit_network = || {
        let mut network = single_interface_network_config_with_vfs(segment_ids.clone());
        network.interfaces[1].virtual_function_id = None;
        network
    };

    // Create an instance through the public API with an implicit VF identity.
    let created_instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(create_host.id)
                .config(InstanceConfig::default_tenant_and_os().network(implicit_network()))
                .tonic_request(),
        )
        .await
        .expect("BF4 Astra must allocate an implicit VF from its intercept topology")
        .into_inner();

    // The response must expose the sole VF selected by the replacement inventory.
    let created_instance_id = created_instance
        .id
        .expect("the allocated instance includes its ID");
    let created_network = created_instance
        .config
        .as_ref()
        .expect("the allocated instance includes its config")
        .network
        .as_ref()
        .expect("the allocated instance includes its network config")
        .clone();
    let created_vfs = created_network
        .interfaces
        .iter()
        .filter(|interface| interface.function_type() == rpc::InterfaceFunctionType::Virtual)
        .map(|interface| {
            interface
                .virtual_function_id
                .expect("NICo resolves every implicit VF ID")
        })
        .collect_vec();
    assert_eq!(created_vfs, vec![14]);

    // Re-read through the public API to prove the resolved topology identity was persisted.
    let persisted_instances = env
        .api
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![created_instance_id],
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(persisted_instances.instances.len(), 1);
    assert_eq!(
        persisted_instances.instances[0]
            .config
            .as_ref()
            .and_then(|config| config.network.as_ref()),
        Some(&created_network)
    );

    // Allocate directly because this test needs only an existing instance as the update baseline.
    let update_instance = env
        .api
        .allocate_instance(
            InstanceAllocationRequest::builder(false)
                .machine_id(update_host.id)
                .config(
                    InstanceConfig::default_tenant_and_os()
                        .network(single_interface_network_config(segment_ids[0])),
                )
                .tonic_request(),
        )
        .await
        .expect("the update fixture instance must be allocated")
        .into_inner();
    let update_instance_id = update_instance
        .id
        .expect("the allocated update fixture includes its ID");
    // Network updates require an assigned Ready host; DPF convergence is outside this contract.
    let mut txn = env.db_txn().await;
    db::machine::update_state(
        txn.as_mut(),
        &update_host.id,
        &ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await
    .unwrap();
    txn.commit().await.unwrap();

    // Update through the public API with another implicit VF identity.
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(update_instance_id),
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(implicit_network()),
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            }),
            metadata: Some(rpc::forge::Metadata {
                name: "bf4-astra-implicit-vf-update".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await
        .expect("a BF4 Astra update must use the intercept topology");

    // Reload the staged replacement and verify persistence uses the same sparse VF identity.
    let mut txn = env.db_txn().await;
    let staged_update = db::instance::find_by_id(txn.as_mut(), update_instance_id)
        .await
        .unwrap()
        .expect("the update fixture instance remains persisted")
        .update_network_config_request
        .expect("the replacement network config must be staged");
    let staged_vfs = staged_update
        .new_config
        .interfaces
        .iter()
        .filter_map(|interface| match &interface.function_id {
            InterfaceFunctionId::Physical {} => None,
            InterfaceFunctionId::Virtual { id } => Some(*id),
        })
        .collect_vec();
    assert_eq!(staged_vfs, vec![14]);
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
    let mut txn = env.pool.begin().await.unwrap();
    for managed_host in [&create_host, &update_host] {
        db::machine::mark_machine_ingestion_done_with_dpf(&mut txn, &managed_host.id)
            .await
            .unwrap();
    }
    txn.commit().await.unwrap();
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
                machine_id: Some(create_host.id),
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

    // VF15 is the highest valid value and must pass the same public create and update paths.
    env.api
        .allocate_instance(Request::new(rpc::forge::InstanceAllocationRequest {
            machine_id: Some(create_host.id),
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

const TEST_DPF_HELM_SERVICE_DATA: &str = r#"{
    "repoURL": "oci://registry.example.com/charts",
    "chartName": "tenant-service",
    "chartVersion": "1.2.3",
    "serviceID": "tenant-service-v1",
    "deployInCluster": false,
    "security": {"privileged": false}
}"#;

/// Registers the one-interface DPF service used by service-VPC instance tests.
///
/// Keeping this request outside individual tests makes their bodies describe instance behavior
/// rather than repeat registration plumbing, while deriving the DPF service ID from the unique
/// service name lets one coordinator transaction attach multiple registrations.
async fn create_networked_dpf_service(
    env: &TestEnv,
    service_name: &str,
) -> Result<DpuExtensionService, tonic::Status> {
    create_networked_dpf_service_with_target(
        env,
        service_name,
        rpc::forge::DpuExtensionServiceDpuTarget::All,
    )
    .await
}

/// Registers the shared one-interface DPF fixture with an explicit placement policy so
/// multi-DPU tests select endpoint DPUs deterministically.
async fn create_networked_dpf_service_with_target(
    env: &TestEnv,
    service_name: &str,
    dpu_target: rpc::forge::DpuExtensionServiceDpuTarget,
) -> Result<DpuExtensionService, tonic::Status> {
    create_networked_dpf_service_with_target_and_family(
        env,
        service_name,
        dpu_target,
        rpc::forge::ServiceVpcAddressFamily::Ipv4,
    )
    .await
}

/// Registers one DPF service with explicit placement and address family so admission tests can
/// vary those immutable contracts without duplicating unrelated registration fields.
async fn create_networked_dpf_service_with_target_and_family(
    env: &TestEnv,
    service_name: &str,
    dpu_target: rpc::forge::DpuExtensionServiceDpuTarget,
    family: rpc::forge::ServiceVpcAddressFamily,
) -> Result<DpuExtensionService, tonic::Status> {
    Ok(env
        .api
        .create_dpu_extension_service(Request::new(rpc::forge::CreateDpuExtensionServiceRequest {
            dpu_target: Some(dpu_target as i32),
            service_vpc_interfaces: vec![rpc::forge::ServiceVpcInterfaceRequirement {
                address_family: family as i32,
            }],
            service_id: None,
            service_name: service_name.to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::forge::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_SERVICE_DATA.replace("tenant-service-v1", service_name),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?
        .into_inner())
}

/// Builds a DPF-enabled environment whose IPv4 service-VPC capacity and deployment lookup let
/// coordinator tests exercise transactional behavior while the production family gate stays closed.
async fn create_service_vpc_coordinator_test_env(pool: PgPool) -> TestEnv {
    create_service_vpc_coordinator_test_env_with_dpf_sdk(pool, service_vpc_deployment_mock()).await
}

/// Builds coordinator test configuration with a caller-supplied deployment classifier so tests
/// can isolate admission decisions without provisioning Kubernetes DPF resources.
async fn create_service_vpc_coordinator_test_env_with_dpf_sdk(
    pool: PgPool,
    dpf_sdk: Arc<dyn DpfOperations>,
) -> TestEnv {
    create_service_vpc_coordinator_test_env_with_capacity(pool, dpf_sdk, 2, 2).await
}

/// Builds coordinator test configuration with explicit DPU capacity so allocation tests can verify
/// exhaustion using per-`TestEnv` slot-count and endpoint-capacity values.
async fn create_service_vpc_coordinator_test_env_with_capacity(
    pool: PgPool,
    dpf_sdk: Arc<dyn DpfOperations>,
    slot_count: u32,
    max_endpoints_per_dpu: u32,
) -> TestEnv {
    create_service_vpc_coordinator_test_env_with_capacity_and_families(
        pool,
        dpf_sdk,
        slot_count,
        max_endpoints_per_dpu,
        &[model::extension_service::ServiceVpcAddressFamily::Ipv4],
    )
    .await
}

/// Builds coordinator test configuration with explicit endpoint capacity and address families so
/// family-specific allocation tests exercise the production deployment gate.
async fn create_service_vpc_coordinator_test_env_with_capacity_and_families(
    pool: PgPool,
    dpf_sdk: Arc<dyn DpfOperations>,
    slot_count: u32,
    max_endpoints_per_dpu: u32,
    enabled_families: &[model::extension_service::ServiceVpcAddressFamily],
) -> TestEnv {
    // Enable DPF while disabling the site-global VPC shortcut so test VPCs drive selection.
    let mut config = get_config();
    config.dpf.enabled = true;
    config.site_global_vpc_vni = None;
    // These limits determine whether the coordinator can assign a DPU-local endpoint slot.
    config.dpu_config.service_vpc_slot_count = slot_count;
    config.dpu_config.max_active_service_vpc_interfaces_per_dpu = max_endpoints_per_dpu;
    // The BF3 family set is the deployment-specific admission contract under test.
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families = enabled_families.iter().copied().collect();

    // Supply deterministic deployment and FNN fixtures without provisioning external DPF resources.
    create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config)
            .with_dpf_sdk(dpf_sdk)
            .with_fnn_config(None),
    )
    .await
}

/// Builds the minimal public force-delete request used to verify both service-endpoint membership
/// decisions while leaving unrelated device-deletion options disabled.
fn service_vpc_force_delete_request(
    host_id: impl std::fmt::Display,
    allow_orphaned_dpf_resources: bool,
) -> AdminForceDeleteMachineRequest {
    AdminForceDeleteMachineRequest {
        host_query: host_id.to_string(),
        delete_interfaces: false,
        delete_bmc_interfaces: false,
        delete_bmc_credentials: false,
        allow_delete_with_orphaned_dpf_crds: allow_orphaned_dpf_resources,
        delete_bmc_suppressions: false,
        delete_retained_boot_interfaces: false,
        release_preserved_addresses: false,
        wait_for_instance_dpu: false,
    }
}

/// Holds one production-valid networked attachment request and its captured host and Instance
/// values so coordinator tests can vary one concurrency or admission condition without rebuilding fixtures.
struct ServiceVpcCoordinatorFixture {
    instance_id: InstanceId,
    parent_id: VpcPrefixId,
    host_snapshot: ManagedHostStateSnapshot,
    instance_snapshot: InstanceSnapshot,
    extension_config: InstanceExtensionServicesConfig,
    request: RequestedInstanceExtensionServicesConfig,
}

/// Creates one Ready DPF attachment with exact IPv4 VPC-prefix capacity because concurrency tests
/// must pass every earlier registration, topology, VPC, slot, and allocation precondition.
async fn create_service_vpc_coordinator_fixture(
    env: &TestEnv,
    name: &str,
) -> Result<ServiceVpcCoordinatorFixture, Box<dyn std::error::Error>> {
    // Register the host and service tenants before creating their FNN VPCs.
    create_fixture_tenant(env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(env, "best_org").await?;
    // The host FNN segment supplies the DPU-local tenant path required by service activation.
    let (_, host_segment_id) = create_fnn_host_segment(
        env,
        &format!("{name} host VPC"),
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    // The service VPC prefix provides multiple /31 links so concurrency tests cannot fail on capacity.
    let service_vpc_id = create_fnn_vpc(env, &format!("{name} service VPC")).await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        env,
        service_vpc_id,
        &format!("{name} service prefix"),
        "10.223.0.0/29".parse()?,
    )
    .await;
    // Create a Ready one-DPU instance and registration that satisfy every earlier admission check.
    let host = create_service_vpc_test_host(env).await?;
    let instance = host
        .instance_builer(env)
        .single_interface_network_config(host_segment_id)
        .build()
        .await;
    let service = create_networked_dpf_service(env, &format!("{name}-service")).await?;
    mark_networked_dpf_service_ready(env, service.service_id.parse()?).await?;
    let (attachment, selection) = coordinator_attachment(&service, service_vpc_id)?;
    // Capture the host and Instance values so each test changes only the field the coordinator must revalidate.
    let mut snapshot_txn = env.db_txn().await;
    let host_snapshot = host.snapshot(&mut snapshot_txn).await;
    snapshot_txn.rollback().await?;
    let instance_snapshot = host_snapshot
        .instance
        .clone()
        .expect("coordinator fixture host has an instance");

    Ok(ServiceVpcCoordinatorFixture {
        instance_id: instance.id,
        parent_id,
        host_snapshot,
        instance_snapshot,
        extension_config: InstanceExtensionServicesConfig {
            service_configs: vec![attachment],
        },
        request: RequestedInstanceExtensionServicesConfig {
            service_configs: vec![selection],
        },
    })
}

/// Confirms a rejected coordinator fixture retained `extension_services_config`,
/// `extension_services_config_version`, `network_config`, `network_config_version`,
/// `update_network_config_request`, and `VpcPrefix.status.last_used_prefix` and created no generated
/// network segment or Instance address so admission tests prove transaction rollback rather than only errors.
async fn assert_service_vpc_fixture_unmodified(
    env: &TestEnv,
    fixture: &ServiceVpcCoordinatorFixture,
) -> Result<(), Box<dyn std::error::Error>> {
    // Re-read the Instance and configured VPC prefix after the rejecting transaction has ended.
    let persisted = db::instance::find_by_id(&env.pool, fixture.instance_id)
        .await?
        .expect("fixture instance remains stored");
    assert_eq!(
        persisted.config.extension_services,
        fixture.instance_snapshot.config.extension_services
    );
    assert_eq!(
        persisted.config.network,
        fixture.instance_snapshot.config.network
    );
    assert_eq!(
        persisted.extension_services_config_version,
        fixture.instance_snapshot.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        fixture.instance_snapshot.network_config_version
    );
    assert_eq!(
        persisted.update_network_config_request,
        fixture.instance_snapshot.update_network_config_request
    );
    let parent = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[fixture.parent_id],
    )
    .await?
    .pop()
    .expect("service parent remains stored");
    assert_eq!(parent.status.last_used_prefix, None);

    // The selected service VPC must contain no generated endpoint segment or normalized address.
    let selected_vpc_id = fixture.request.service_configs[0].service_vpc_ids[0];
    let segments = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(db::network_segment::VpcColumn, &selected_vpc_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?;
    assert!(segments.is_empty());
    let address_count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM instance_addresses WHERE vpc_id = $1")
            .bind(selected_vpc_id)
            .fetch_one(&env.pool)
            .await?;
    assert_eq!(address_count, 0);

    Ok(())
}

/// Builds a public complete-config request that changes host networking and adds the fixture's
/// service attachment so integration tests exercise `update_service_network_config` and
/// `update_config` in one transaction.
async fn service_vpc_complete_config_update_request(
    env: &TestEnv,
    fixture: &ServiceVpcCoordinatorFixture,
    host_segment_id: NetworkSegmentId,
    if_version_match: Option<String>,
) -> rpc::forge::InstanceConfigUpdateRequest {
    // Start from the public definition and replace only host networking and extension services.
    let current = env.one_instance(fixture.instance_id).await.into_inner();
    let mut config = current
        .config
        .expect("fixture instance has a public config");
    let attachment = &fixture.extension_config.service_configs[0];
    let selection = &fixture.request.service_configs[0];
    config.network = Some(single_interface_network_config(host_segment_id));
    config.dpu_extension_services = Some(rpc::forge::InstanceDpuExtensionServicesConfig {
        service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
            service_id: attachment.service_id.to_string(),
            version: attachment.version.to_string(),
            service_vpc_ids: selection.service_vpc_ids.clone(),
        }],
    });

    rpc::forge::InstanceConfigUpdateRequest {
        instance_id: Some(fixture.instance_id),
        if_version_match,
        config: Some(config),
        metadata: current.metadata,
    }
}

/// Supplies the BF3 deployment lookup used when service-VPC tests revalidate endpoint placement
/// without exercising DPF resource provisioning.
fn service_vpc_deployment_mock() -> Arc<dyn DpfOperations> {
    let mut dpf_sdk = MockDpfOperations::new();
    dpf_sdk
        .expect_deployment_type_for_dpu()
        .returning(|_, _| Ok(DpuDeploymentType::Bf3));
    Arc::new(dpf_sdk)
}

/// Keeps site-level DPF APIs enabled while routing fixture hosts through ordinary ingestion.
fn service_vpc_test_host_config() -> ManagedHostConfig {
    let mut host_config = ManagedHostConfig::default();
    host_config.expected_machine_data = Some(ExpectedMachineData {
        dpf_enabled: Some(false),
        ..Default::default()
    });
    host_config
}

/// Creates a completed host through ordinary fixture ingestion and records DPF ingestion so
/// service-VPC tests exercise endpoint behavior without provisioning DPF resources.
async fn create_service_vpc_test_host(
    env: &TestEnv,
) -> Result<TestManagedHost, Box<dyn std::error::Error>> {
    create_service_vpc_test_host_with_dpu_count(env, 1).await
}

/// Creates a completed test host with the requested DPU count so multi-target coordinator tests
/// use the same DPF-ingestion state as single-DPU production-valid fixtures.
async fn create_service_vpc_test_host_with_dpu_count(
    env: &TestEnv,
    dpu_count: usize,
) -> Result<TestManagedHost, Box<dyn std::error::Error>> {
    let host = create_managed_host_with_config(
        env,
        service_vpc_test_host_config().with_dpu_count(dpu_count),
    )
    .await;
    let mut connection = env.pool.acquire().await?;
    db::machine::mark_machine_ingestion_done_with_dpf(connection.as_mut(), &host.id).await?;
    Ok(host)
}

/// Advances one DPF registration to Ready so coordinator tests isolate instance allocation from
/// the separately tested DPUService reconciliation lifecycle.
async fn mark_networked_dpf_service_ready(
    env: &TestEnv,
    service_id: ExtensionServiceId,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut txn = env.db_txn().await;
    let service = db::extension_service::find_by_ids(txn.as_mut(), &[service_id], false, false)
        .await?
        .pop()
        .expect("created DPF registration exists");
    let state_change = service.status.controller_state.version.incremental_change();
    assert_eq!(
        db::extension_service::try_update_controller_state(
            txn.as_mut(),
            service_id,
            state_change.current,
            state_change.new,
            &ExtensionServiceLifecycleState::Ready,
        )
        .await?,
        db::ConditionalWrite::Applied(())
    );
    db::state_history::persist(
        txn.as_mut(),
        db::state_history::StateHistoryTableId::ExtensionService,
        &service_id,
        &ExtensionServiceLifecycleState::Ready,
        state_change.new,
    )
    .await?;
    txn.commit().await?;
    Ok(())
}

/// Creates matching `InstanceExtensionServiceConfig` and
/// `RequestedInstanceExtensionServiceConfig` values so direct coordinator tests preserve the split
/// between the assigned attachment ID and ordered VPC selections.
fn coordinator_attachment(
    service: &DpuExtensionService,
    vpc_id: VpcId,
) -> Result<
    (
        InstanceExtensionServiceConfig,
        RequestedInstanceExtensionServiceConfig,
    ),
    Box<dyn std::error::Error>,
> {
    let service_id = service.service_id.parse()?;
    let version = service
        .latest_version_info
        .as_ref()
        .expect("created service has a version")
        .version
        .parse()?;
    Ok((
        InstanceExtensionServiceConfig {
            id: Some(uuid::Uuid::new_v4()),
            service_id,
            version,
            dpu_target: Some(model::extension_service::DpuTarget::All),
            removed: None,
        },
        RequestedInstanceExtensionServiceConfig {
            service_id,
            version,
            service_vpc_ids: vec![vpc_id],
        },
    ))
}

/// Creates an FNN VPC without children so coordinator classification tests can delete it before
/// constructing caller selections.
async fn create_fnn_vpc(env: &TestEnv, name: &str) -> VpcId {
    env.api
        .create_vpc(
            VpcCreationRequest::builder(FIXTURE_TENANT_ORG_ID)
                .metadata(Metadata {
                    name: name.to_string(),
                    ..Default::default()
                })
                .network_virtualization_type(rpc::forge::VpcVirtualizationType::Fnn as i32)
                .tonic_request(),
        )
        .await
        .expect("FNN VPC fixture must be created")
        .into_inner()
        .id
        .expect("created VPC has an ID")
}

/// Creates one ready tenant segment in an FNN VPC so coordinator tests supply the DPU-local host
/// VPC required for service activation.
async fn create_fnn_host_segment(
    env: &TestEnv,
    name: &str,
    network: IpNetwork,
) -> (VpcId, NetworkSegmentId) {
    let vpc_id = create_fnn_vpc(env, name).await;
    let segment_id =
        create_tenant_network_segment(&env.api, Some(vpc_id), network, name, true).await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
    (vpc_id, segment_id)
}

/// Seeds an active networked attachment so compatibility tests can exercise persisted service-interface fields.
///
/// The attachment and service-interface record share one attachment ID, while the endpoint names
/// a real generated segment, prefix, normalized address, and persisted service MAC. The VPC prefix
/// is committed first because service allocation requires configured VPC capacity; returning the
/// service-interface value lets callers verify the generated network segment, network prefix, and
/// Instance address.
async fn seed_networked_attachment(
    env: &TestEnv,
    instance_id: InstanceId,
    dpu_id: DpuMachineId,
    service: &DpuExtensionService,
    vpc_id: VpcId,
) -> Result<InstanceServiceInterfaceConfig, Box<dyn std::error::Error>> {
    seed_networked_attachment_with_prefix(
        env,
        instance_id,
        dpu_id,
        service,
        vpc_id,
        0,
        "seeded service VPC IPv4 prefix",
        "10.219.0.0/30".parse()?,
    )
    .await
}

/// Seeds an additional networked attachment with an explicit slot and VPC-prefix capacity so tests
/// can construct multiple valid service-interface, network-segment, network-prefix, and
/// Instance-address record sets without overwriting earlier records.
#[allow(clippy::too_many_arguments)]
async fn seed_networked_attachment_with_prefix(
    env: &TestEnv,
    instance_id: InstanceId,
    dpu_id: DpuMachineId,
    service: &DpuExtensionService,
    vpc_id: VpcId,
    slot_index: u32,
    prefix_name: &str,
    parent_prefix: IpNetwork,
) -> Result<InstanceServiceInterfaceConfig, Box<dyn std::error::Error>> {
    let attachment_id = uuid::Uuid::new_v4();
    let service_id = service.service_id.parse()?;
    let service_version = service
        .latest_version_info
        .as_ref()
        .expect("created service has a version")
        .version
        .parse()?;

    // Add the configured IPv4 capacity that production activation requires before allocating the
    // generated network segment, network prefix, and Instance address.
    let parent_id =
        create_tenant_overlay_prefix_with_prefix(env, vpc_id, prefix_name, parent_prefix).await;

    let mut txn = env.db_txn().await;
    let mac_assignment = match db::extension_service::find_interface_macs(txn.as_mut(), service_id)
        .await?
        .into_iter()
        .find(|assignment| assignment.interface_ordinal == 0)
    {
        Some(assignment) => assignment,
        None => {
            let assignment =
                model::extension_service::ExtensionServiceInterfaceMac::candidate(service_id, 0, 0);
            db::extension_service::insert_interface_mac(txn.as_mut(), assignment).await?;
            assignment
        }
    };

    // Allocate the same segment, prefix, and address records produced by activation so compatibility
    // requests validate a persisted network segment, network prefix, and Instance address rather
    // than a synthetic service interface.
    let parent = db::vpc_prefix::lock_for_allocation(txn.as_mut(), parent_id)
        .await?
        .expect("service VPC prefix remains available");
    let allocator = PrefixAllocator::new(
        parent.id,
        parent.config.prefix,
        parent.status.last_used_prefix,
        31,
    )?;
    let link_prefix = allocator.next_free_prefix(txn.as_mut()).await?;
    let (network_segment_id, network_prefix) = allocator
        .allocate_service_network_segment_for_prefix(txn.as_mut(), vpc_id, link_prefix)
        .await?;
    db::vpc_prefix::update_last_used_prefix(txn.as_mut(), &parent.id, link_prefix).await?;
    db::instance_address::insert_service_address(
        txn.as_mut(),
        instance_id,
        network_segment_id,
        vpc_id,
        &network_prefix,
    )
    .await?;

    let stored_instance = db::instance::find_by_id(txn.as_mut(), instance_id)
        .await?
        .expect("instance remains stored");

    let current_extension_services = stored_instance.config.extension_services.clone();
    let mut seeded_extension_services = current_extension_services.clone();
    let registered_target = service
        .dpu_target
        .and_then(|target| rpc::forge::DpuExtensionServiceDpuTarget::try_from(target).ok())
        .map(Into::into)
        .expect("networked service has a registered DPU target");
    seeded_extension_services
        .service_configs
        .push(InstanceExtensionServiceConfig {
            id: Some(attachment_id),
            dpu_target: Some(registered_target),
            service_id,
            version: service_version,
            removed: None,
        });

    let mut seeded_network_config = stored_instance.config.network.clone();
    let endpoint = InstanceServiceInterfaceConfig {
        attachment_id,
        interface_ordinal: 0,
        dpu_id,
        slot_index,
        vpc_id,
        vpc_prefix_id: parent.id,
        network_segment_id,
        network_prefix_id: network_prefix.id,
        link_prefix,
        mac_address: mac_assignment.mac_address,
        internal_uuid: uuid::Uuid::new_v4(),
    };
    seeded_network_config
        .service_interfaces
        .push(endpoint.clone());

    assert!(matches!(
        db::instance::update_service_network_config(
            txn.as_mut(),
            stored_instance.id,
            stored_instance.extension_services_config_version,
            &current_extension_services,
            &seeded_extension_services,
            stored_instance.network_config_version,
            &stored_instance.config.network,
            &seeded_network_config,
            false,
        )
        .await?,
        db::ConditionalWrite::Applied(_)
    ));
    txn.commit().await?;

    Ok(endpoint)
}

/// Verifies allocation assigns and persists a non-nil UUIDv4 attachment ID so
/// later service-interface records can reference it without inventing identity
/// during reads or retries.
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

    // Keep the service nonnetworked so allocation-time attachment identity is
    // the only new persisted behavior under test.
    let service = env
        .api
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "test-service".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service"),
                credential: None,
                observability: None,
                deployment_type: 0,
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
                service_vpc_ids: vec![],
            }],
        }),
        power_profile: None,
    };

    let tinstance = mh
        .instance_builer(&env)
        .config(config.clone())
        .build()
        .await;

    // Reload persisted state to prove allocation assigned a durable, non-nil UUIDv4.
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
    let attachment = &instance_snapshot.config.extension_services.service_configs[0];
    assert_eq!(attachment.service_id, service.service_id.parse().unwrap());
    let attachment_id = attachment.id.expect("new attachment has an ID");
    assert_ne!(attachment_id, uuid::Uuid::nil());
    assert_eq!(attachment_id.get_version(), Some(uuid::Version::Random));
    txn.rollback().await?;

    // A fresh public read proves the same durable identity reaches attachment status.
    let instance = tinstance.rpc_instance().await.into_inner();
    let status = instance
        .status
        .expect("allocated instance has status")
        .dpu_extension_services
        .expect("allocated instance has extension-service status");
    assert_eq!(status.dpu_extension_services.len(), 1);
    assert_eq!(
        status.dpu_extension_services[0].attachment_id,
        Some(attachment_id.into())
    );

    Ok(())
}

/// Verifies public initial allocation creates service and host networking in one transaction,
/// preventing the later host-network write from discarding endpoints planned without an Instance.
#[crate::sqlx_test]
async fn test_allocate_instance_activates_service_vpc_and_preserves_host_network(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Prepare an unassigned DPF host plus independent host and service VPC capacity.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_service_vpc_coordinator_test_env(pool).await;
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    create_fixture_tenant(&env, "best_org").await?;
    let (_, host_segment_id) = create_fnn_host_segment(
        &env,
        "initial allocation host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let service_vpc_id = create_fnn_vpc(&env, "initial allocation service VPC").await;
    let parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        service_vpc_id,
        "initial allocation service prefix",
        "10.231.0.0/29".parse()?,
    )
    .await;
    let host = create_service_vpc_test_host(&env).await?;
    let service = create_networked_dpf_service(&env, "initial-allocation-service").await?;
    mark_networked_dpf_service_ready(&env, service.service_id.parse()?).await?;
    let requested_instance_id = InstanceId::new();

    // Open implementation support only around AllocateInstance so the coordinator receives None.
    let allocated = crate::instance::service_vpc::with_service_vpc_implementation_support_for_test(
        env.api
            .allocate_instance(Request::new(rpc::InstanceAllocationRequest {
                machine_id: Some(host.id),
                config: Some(rpc::InstanceConfig {
                    tenant: Some(default_tenant_config()),
                    os: Some(default_os_config()),
                    network: Some(single_interface_network_config(host_segment_id)),
                    dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                        service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                            service_id: service.service_id,
                            version: service
                                .latest_version_info
                                .expect("created service has a version")
                                .version,
                            service_vpc_ids: vec![service_vpc_id],
                        }],
                    }),
                    ..Default::default()
                }),
                instance_id: Some(requested_instance_id),
                instance_type_id: None,
                metadata: Some(rpc::Metadata {
                    name: "initial-service-vpc-allocation".to_string(),
                    description: String::new(),
                    labels: vec![],
                }),
                allow_unhealthy_machine: false,
            })),
    )
    .await?
    .into_inner();
    assert_eq!(allocated.id, Some(requested_instance_id));

    // A fresh public read proves the committed attachment retains its selected service VPC.
    let public_persisted = env
        .api
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![requested_instance_id],
        }))
        .await?
        .into_inner()
        .instances
        .pop()
        .expect("allocated instance remains publicly visible");
    assert_eq!(
        public_persisted
            .config
            .as_ref()
            .expect("allocated instance has a config")
            .dpu_extension_services
            .as_ref()
            .expect("allocated instance has an active service")
            .service_configs[0]
            .service_vpc_ids,
        vec![service_vpc_id]
    );

    // Database reads prove the host interface and address plus the service attachment, service
    // interface, network segment, network prefix, and Instance address all committed in the
    // allocation transaction.
    let persisted = db::instance::find_by_id(&env.pool, requested_instance_id)
        .await?
        .expect("allocated instance remains stored");
    let [attachment] = persisted
        .config
        .extension_services
        .service_configs
        .as_slice()
    else {
        panic!("initial allocation must persist one service attachment");
    };
    let [endpoint] = persisted.config.network.service_interfaces.as_slice() else {
        panic!("initial allocation must persist one service endpoint");
    };
    assert_eq!(attachment.id, Some(endpoint.attachment_id));
    assert_eq!(endpoint.vpc_id, service_vpc_id);
    assert_eq!(endpoint.vpc_prefix_id, parent_id);
    assert_eq!(
        persisted.config.network.interfaces[0].network_segment_id,
        Some(host_segment_id)
    );
    assert_eq!(
        db::instance_address::find_all_by_instance_id_and_segment_id(
            &mut *env.pool.acquire().await?,
            &requested_instance_id,
            &host_segment_id,
        )
        .await?
        .len(),
        1
    );
    let service_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut *env.pool.acquire().await?,
        &requested_instance_id,
        &endpoint.network_segment_id,
    )
    .await?;
    assert_eq!(service_addresses.len(), 1);
    assert_eq!(service_addresses[0].prefix, endpoint.link_prefix);
    let generated_segment = db::network_segment::find_by(
        &mut *env.pool.acquire().await?,
        ObjectColumnFilter::One(IdColumn, &endpoint.network_segment_id),
        NetworkSegmentSearchConfig::default(),
    )
    .await?
    .pop()
    .expect("generated service segment remains stored");
    assert_eq!(generated_segment.prefixes[0].id, endpoint.network_prefix_id);
    let parent =
        db::vpc_prefix::get_for_allocation_by_ids(&mut *env.pool.acquire().await?, &[parent_id])
            .await?
            .pop()
            .expect("service parent remains stored");
    assert_eq!(parent.status.last_used_prefix, Some(endpoint.link_prefix));

    Ok(())
}

/// Verifies an older client's empty VPC selection cannot bypass the networked-service
/// implementation gate because incomplete lifecycle support must leave no committed instance.
#[crate::sqlx_test]
async fn test_allocate_instance_rejects_networked_service_without_vpc_selection(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Enable DPF registration and prepare a host that must remain unassigned.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let mut config = get_config();
    config.dpf.enabled = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let managed_host = create_managed_host(&env).await;
    let requested_instance_id = InstanceId::new();

    // Register a service whose one declared interface requires a VPC selection.
    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "best_org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::Metadata {
                name: "best_org".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await?;
    let service = env
        .api
        .create_dpu_extension_service(Request::new(rpc::forge::CreateDpuExtensionServiceRequest {
            dpu_target: Some(rpc::forge::DpuExtensionServiceDpuTarget::All as i32),
            service_vpc_interfaces: vec![rpc::forge::ServiceVpcInterfaceRequirement {
                address_family: rpc::forge::ServiceVpcAddressFamily::Ipv4 as i32,
            }],
            service_id: None,
            service_name: "networked-allocation-service".to_string(),
            description: None,
            tenant_organization_id: "best_org".to_string(),
            service_type: rpc::forge::DpuExtensionServiceType::DpfHelmChart.into(),
            data: TEST_DPF_HELM_SERVICE_DATA.to_string(),
            credential: None,
            observability: None,
            deployment_type: 0,
        }))
        .await?
        .into_inner();

    // The empty repeated field must not bypass the binary's fail-closed implementation guard.
    let error = env
        .api
        .allocate_instance(Request::new(rpc::InstanceAllocationRequest {
            machine_id: Some(managed_host.id),
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                        service_id: service.service_id,
                        version: service
                            .latest_version_info
                            .expect("created service has a version")
                            .version,
                        service_vpc_ids: vec![],
                    }],
                }),
                ..Default::default()
            }),
            instance_id: Some(requested_instance_id),
            instance_type_id: None,
            metadata: Some(rpc::Metadata {
                name: "networked-allocation".to_string(),
                description: String::new(),
                labels: vec![],
            }),
            allow_unhealthy_machine: false,
        }))
        .await
        .expect_err("networked attachment without a VPC must be rejected");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "service VPC attachment is unavailable until network resource reconciliation is implemented"
    );

    // Re-read through the public API to prove the rejected transaction committed no instance.
    let persisted = env
        .api
        .find_instances_by_ids(Request::new(rpc::forge::InstancesByIdsRequest {
            instance_ids: vec![requested_instance_id],
        }))
        .await?
        .into_inner();
    assert!(persisted.instances.is_empty());

    Ok(())
}

/// Verifies updates reject IPv4 and IPv6 networked attachments, including an omitted VPC selection,
/// because permissive overlap policy must not bypass the independent implementation release gate.
#[crate::sqlx_test]
async fn test_update_instance_rejects_new_networked_attachment_while_lifecycle_is_incomplete(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Parse operator-facing settings, then merge only the service-VPC fields so the host retains
    // the test fixture's unrelated ingestion settings.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let mut config = get_config();
    config.dpf.enabled = true;
    let parsed_dpu_config: crate::cfg::file::DpuConfig = toml::from_str(
        r#"
            service_vpc_slot_count = 2
            max_active_service_vpc_interfaces_per_dpu = 2
            allow_service_vpc_tenant_overlap = true
        "#,
    )?;
    config.dpu_config.service_vpc_slot_count = parsed_dpu_config.service_vpc_slot_count;
    config.dpu_config.max_active_service_vpc_interfaces_per_dpu =
        parsed_dpu_config.max_active_service_vpc_interfaces_per_dpu;
    config.dpu_config.allow_service_vpc_tenant_overlap =
        parsed_dpu_config.allow_service_vpc_tenant_overlap;
    let parsed_bf3_deployment: crate::cfg::file::DpfDeploymentConfig = toml::from_str(
        r#"
            flavor_name = "bf3"
            deployment_name = "bf3"
            node_label_key = "example.com/bf3"
            service_vpc_enabled_address_families = ["ipv4", "ipv6"]
        "#,
    )?;
    config
        .dpf
        .deployments
        .bf3
        .service_vpc_enabled_address_families =
        parsed_bf3_deployment.service_vpc_enabled_address_families;
    assert_eq!(config.dpu_config.service_vpc_slot_count, 2);
    assert_eq!(
        config.dpu_config.max_active_service_vpc_interfaces_per_dpu,
        2
    );
    assert!(config.dpu_config.allow_service_vpc_tenant_overlap);
    assert!(
        config
            .dpf
            .deployments
            .bf3
            .service_vpc_enabled_address_families
            .iter()
            .any(|family| { family == model::extension_service::ServiceVpcAddressFamily::Ipv6 })
    );
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config)
            .with_dpf_sdk(service_vpc_deployment_mock())
            .with_fnn_config(None),
    )
    .await;

    // Register the owning tenant before creating its FNN host and service VPCs.
    create_fixture_tenant(&env, FIXTURE_TENANT_ORG_ID).await?;
    let (_, segment_id) = create_fnn_host_segment(
        &env,
        "implementation-gate host VPC",
        FIXTURE_TENANT_NETWORK_SEGMENT_GATEWAYS[0],
    )
    .await;
    let selected_vpc = create_fnn_vpc(&env, "implementation-gate service VPC").await;
    let ipv4_parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        selected_vpc,
        "implementation-gate IPv4 capacity",
        "10.229.0.0/29".parse()?,
    )
    .await;
    let ipv6_parent_id = create_tenant_overlay_prefix_with_prefix(
        &env,
        selected_vpc,
        "implementation-gate IPv6 capacity",
        "fd00:229::/124".parse()?,
    )
    .await;
    let managed_host = create_service_vpc_test_host(&env).await?;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    let baseline = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("implementation-gate instance remains stored");

    env.api
        .create_tenant(Request::new(rpc::forge::CreateTenantRequest {
            organization_id: "best_org".to_string(),
            routing_profile_type: None,
            metadata: Some(rpc::Metadata {
                name: "best_org".to_string(),
                description: String::new(),
                labels: vec![],
            }),
        }))
        .await?;
    let ipv4_service = create_networked_dpf_service_with_target_and_family(
        &env,
        "new-networked-ipv4-service",
        rpc::forge::DpuExtensionServiceDpuTarget::All,
        rpc::forge::ServiceVpcAddressFamily::Ipv4,
    )
    .await?;
    let ipv6_service = create_networked_dpf_service_with_target_and_family(
        &env,
        "new-networked-ipv6-service",
        rpc::forge::DpuExtensionServiceDpuTarget::All,
        rpc::forge::ServiceVpcAddressFamily::Ipv6,
    )
    .await?;

    // Advance both registrations so each case reaches the implementation release gate with a
    // Ready service.
    mark_networked_dpf_service_ready(&env, ipv4_service.service_id.parse()?).await?;
    mark_networked_dpf_service_ready(&env, ipv6_service.service_id.parse()?).await?;

    let cases = [
        // An IPv4 selection reaches the release gate even when overlap policy is permissive.
        ("selected IPv4 VPC", &ipv4_service, vec![selected_vpc]),
        // An omitted IPv4 selection cannot bypass the release gate through an empty repeated field.
        ("omitted IPv4 VPC", &ipv4_service, vec![]),
        // IPv6 remains independently unavailable even though its registration is otherwise Ready.
        ("selected IPv6 VPC", &ipv6_service, vec![selected_vpc]),
    ];
    for (scenario, service, service_vpc_ids) in cases {
        // Start from a fresh complete definition so a failed row cannot influence the next one.
        let current = instance.rpc_instance().await.into_inner();
        let mut requested_config = current.config.expect("instance has a config");
        requested_config.dpu_extension_services =
            Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service.service_id.clone(),
                    version: service
                        .latest_version_info
                        .as_ref()
                        .expect("created service has a version")
                        .version
                        .clone(),
                    service_vpc_ids,
                }],
            });

        // Both wire shapes must stop before a partial attachment can be stored.
        let error = env
            .api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance.id),
                if_version_match: None,
                config: Some(requested_config),
                metadata: current.metadata,
            }))
            .await
            .expect_err(scenario);
        assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{scenario}");
        assert_eq!(
            error.message(),
            "service VPC attachment is unavailable until network resource reconciliation is implemented",
            "{scenario}"
        );

        // Fresh public and database reads prove the gate left `config_version`,
        // `extension_services_config`, `extension_services_config_version`, `network_config`,
        // `network_config_version`, and `update_network_config_request` unchanged.
        let public_persisted = instance.rpc_instance().await.into_inner();
        assert!(
            public_persisted
                .config
                .expect("instance retains its config")
                .dpu_extension_services
                .is_none(),
            "{scenario}"
        );
        let persisted = db::instance::find_by_id(&env.pool, instance.id)
            .await?
            .expect("implementation-gate instance remains stored after rejection");
        assert_eq!(
            persisted.config.extension_services, baseline.config.extension_services,
            "{scenario}"
        );
        assert_eq!(
            persisted.config.network, baseline.config.network,
            "{scenario}"
        );
        assert_eq!(
            persisted.config_version, baseline.config_version,
            "{scenario}"
        );
        assert_eq!(
            persisted.extension_services_config_version, baseline.extension_services_config_version,
            "{scenario}"
        );
        assert_eq!(
            persisted.network_config_version, baseline.network_config_version,
            "{scenario}"
        );
        assert_eq!(
            persisted.update_network_config_request, baseline.update_network_config_request,
            "{scenario}"
        );

        // Neither address family may update `VpcPrefix.status.last_used_prefix` or create a
        // network-segment or Instance-address record.
        let parents = db::vpc_prefix::get_for_allocation_by_ids(
            &mut *env.pool.acquire().await?,
            &[ipv4_parent_id, ipv6_parent_id],
        )
        .await?;
        assert_eq!(parents.len(), 2, "{scenario}");
        assert!(
            parents
                .iter()
                .all(|parent| parent.status.last_used_prefix.is_none()),
            "{scenario}"
        );
        let generated_segments = db::network_segment::find_by(
            &mut *env.pool.acquire().await?,
            ObjectColumnFilter::One(db::network_segment::VpcColumn, &selected_vpc),
            NetworkSegmentSearchConfig::default(),
        )
        .await?;
        assert!(generated_segments.is_empty(), "{scenario}");
        let address_count: i64 =
            sqlx::query_scalar("SELECT count(*) FROM instance_addresses WHERE vpc_id = $1")
                .bind(selected_vpc)
                .fetch_one(&env.pool)
                .await?;
        assert_eq!(address_count, 0, "{scenario}");
    }

    Ok(())
}

/// Verifies whole-config updates preserve server-owned service interfaces when
/// older clients omit the VPC selection or retries repeat it, while rejecting
/// moves and new selections that would replace persisted service-interface records and their
/// network segments, prefixes, and addresses,
/// so caller intent is never silently discarded and live allocations stay owned.
/// Nested instance networking uses the same filtered public view so the DPU
/// compatibility payload cannot expose hidden endpoints.
#[crate::sqlx_test]
async fn test_update_existing_networked_attachment_preserves_service_interfaces(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Use a DPF-deployed host so host admission cannot obscure the
    // whole-definition update behavior under test.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let mut config = get_config();
    config.dpf.enabled = true;
    let env = create_test_env_with_overrides(
        pool,
        TestEnvOverrides::with_config(config).with_dpf_sdk(service_vpc_deployment_mock()),
    )
    .await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let selected_vpc = db::vpc::find_by_segment(&env.pool, segment_id)
        .await?
        .expect("created segment belongs to a VPC")
        .id;
    let managed_host = create_service_vpc_test_host(&env).await?;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;

    create_fixture_tenant(&env, "best_org").await?;
    let service = create_networked_dpf_service(&env, "existing-networked-service").await?;

    // Seed one valid attachment directly because the production implementation gate rejects
    // activation, isolating compatibility behavior for already persisted endpoints.
    let seeded_endpoint = seed_networked_attachment(
        &env,
        instance.id,
        managed_host.dpu_ids[0],
        &service,
        selected_vpc,
    )
    .await?;
    let attachment_id = seeded_endpoint.attachment_id;

    // Confirm that a caller can see the reconstructed VPC selection. Otherwise
    // the read-modify-write cases below would not exercise the wire contract.
    let seeded = instance.rpc_instance().await.into_inner();
    let seeded_attachment = &seeded
        .config
        .as_ref()
        .expect("instance has a config")
        .dpu_extension_services
        .as_ref()
        .expect("instance has an extension service")
        .service_configs[0];
    assert_eq!(seeded_attachment.service_vpc_ids, vec![selected_vpc]);

    // The nested compatibility instance must use the same filtered host-network view.
    let managed_network = env
        .api
        .get_managed_host_network_config(Request::new(
            rpc::forge::ManagedHostNetworkConfigRequest {
                dpu_machine_id: Some(managed_host.dpu_ids[0]),
            },
        ))
        .await?
        .into_inner();
    let nested = managed_network.instance.expect("nested instance");
    let nested_config = nested.config.expect("nested config");
    let nested_status = nested.status.expect("nested status");
    // Absolute counts catch endpoint leaks even if both public projections change together.
    assert_eq!(
        nested_config
            .network
            .as_ref()
            .expect("nested network config")
            .interfaces
            .len(),
        1,
    );
    assert_eq!(
        nested_status
            .network
            .as_ref()
            .expect("nested network status")
            .interfaces
            .len(),
        1,
    );
    assert_eq!(
        nested_config.network,
        seeded.config.as_ref().expect("public config").network,
    );
    assert_eq!(
        nested_status.network,
        seeded.status.as_ref().expect("public status").network,
    );

    // Capture both hidden records and their version counters. Successful
    // compatibility updates and rejected moves must not rewrite either one.
    let baseline = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored");
    let baseline_service_interfaces = baseline.config.network.service_interfaces.clone();
    let baseline_extension_services_version = baseline.extension_services_config_version;
    let baseline_network_version = baseline.network_config_version;

    let cases = [
        // An older client sends no selections; the stored selection must survive.
        ("empty selection", vec![], None),
        // Repeating the stored selection is an idempotent update.
        ("identical selection", vec![selected_vpc], None),
        // Another VPC would require moving the stored records and their allocations.
        (
            "different selection",
            vec![VpcId::new()],
            Some("the VPC selection of an existing extension-service attachment cannot be changed"),
        ),
    ];

    for (scenario, requested_vpc_ids, expected_error) in cases {
        // Build each request from a fresh public read and change only its VPC selection.
        let current = instance.rpc_instance().await.into_inner();
        let mut requested_config = current.config.expect("instance has a config");
        let requested_attachment = &mut requested_config
            .dpu_extension_services
            .as_mut()
            .expect("instance has an extension service")
            .service_configs[0];
        assert_eq!(
            requested_attachment.service_vpc_ids,
            vec![selected_vpc],
            "{scenario}"
        );
        requested_attachment.service_vpc_ids = requested_vpc_ids;
        let result = env
            .api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(instance.id),
                if_version_match: None,
                config: Some(requested_config),
                metadata: current.metadata,
            }))
            .await;

        // Each row reaches its distinct success or immutable-selection boundary.
        match (expected_error, result) {
            (None, Ok(response)) => {
                let response = response.into_inner();
                let response_attachment = &response
                    .config
                    .expect("updated instance has a config")
                    .dpu_extension_services
                    .expect("updated instance has an extension service")
                    .service_configs[0];
                assert_eq!(
                    response_attachment.service_vpc_ids,
                    vec![selected_vpc],
                    "{scenario}"
                );
            }
            (Some(expected_message), Err(error)) => {
                assert_eq!(error.code(), tonic::Code::FailedPrecondition, "{scenario}");
                assert_eq!(error.message(), expected_message, "{scenario}");
            }
            (None, Err(error)) => panic!("{scenario}: update failed: {error}"),
            (Some(expected_message), Ok(_)) => {
                panic!("{scenario}: expected update to fail with {expected_message}")
            }
        }

        // The response proves the caller-visible selection; fresh database
        // state proves the server-owned attachment identity and service
        // interface were neither replaced nor deleted.
        let persisted_public = instance.rpc_instance().await.into_inner();
        let persisted_public_attachment = &persisted_public
            .config
            .expect("persisted instance has a config")
            .dpu_extension_services
            .expect("persisted instance has an extension service")
            .service_configs[0];
        assert_eq!(
            persisted_public_attachment.service_vpc_ids,
            vec![selected_vpc],
            "{scenario}"
        );
        let persisted = db::instance::find_by_id(&env.pool, instance.id)
            .await?
            .expect("instance remains stored");
        assert_eq!(
            persisted.config.extension_services.service_configs[0].id,
            Some(attachment_id),
            "{scenario}"
        );
        assert_eq!(
            persisted.config.network.service_interfaces, baseline_service_interfaces,
            "{scenario}"
        );
        assert_eq!(
            persisted.extension_services_config_version, baseline_extension_services_version,
            "{scenario}"
        );
        assert_eq!(
            persisted.network_config_version, baseline_network_version,
            "{scenario}"
        );
    }

    // Remove the exact normalized address to model a partial persisted activation that an
    // otherwise identical retry must not silently accept.
    let service_address =
        db::instance_address::service_address_for_prefix(seeded_endpoint.link_prefix)?;
    let mut txn = env.db_txn().await;
    db::instance_address::delete_addresses_for_instance(
        txn.as_mut(),
        instance.id,
        &[(seeded_endpoint.network_segment_id, service_address)],
    )
    .await?;
    txn.commit().await?;
    let current = instance.rpc_instance().await.into_inner();

    // Repeat the exact public configuration so the no-op path must still validate backing rows.
    let error = env
        .api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(instance.id),
            if_version_match: None,
            config: current.config,
            metadata: current.metadata,
        }))
        .await
        .expect_err("an identical retry must reject a missing normalized address");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert!(
        error
            .message()
            .contains("generated network segment does not have exactly one normalized address")
    );

    // Fresh reads prove validation neither repaired the address nor rewrote either configuration.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored after inconsistent retry");
    assert_eq!(
        persisted.config.network.service_interfaces,
        baseline_service_interfaces
    );
    assert_eq!(
        persisted.extension_services_config_version,
        baseline_extension_services_version
    );
    assert_eq!(persisted.network_config_version, baseline_network_version);
    let persisted_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        &mut *env.pool.acquire().await?,
        &instance.id,
        &seeded_endpoint.network_segment_id,
    )
    .await?;
    assert!(persisted_addresses.is_empty());

    // Model an attachment accepted before its service-interface records exist.
    // This is the one meaningful difference from the equal-length move above.
    let stored = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored");
    let mut network_without_service_interfaces = stored.config.network.clone();
    network_without_service_interfaces
        .service_interfaces
        .clear();
    let mut txn = env.db_txn().await;
    db::instance::update_network_config(
        txn.as_mut(),
        stored.id,
        stored.network_config_version,
        &network_without_service_interfaces,
        false,
    )
    .await?;
    txn.commit().await?;

    // A fresh read establishes that the stored selection is genuinely absent.
    let current = instance.rpc_instance().await.into_inner();
    let mut requested_config = current.config.expect("instance has a config");
    let requested_attachment = &mut requested_config
        .dpu_extension_services
        .as_mut()
        .expect("instance has an extension service")
        .service_configs[0];
    assert!(requested_attachment.service_vpc_ids.is_empty());
    requested_attachment.service_vpc_ids = vec![selected_vpc];
    let no_interface_baseline = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored without service interfaces");
    assert!(
        no_interface_baseline
            .config
            .network
            .service_interfaces
            .is_empty()
    );

    // Adding a selection must fail instead of being accepted as a silent no-op.
    let error = env
        .api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(instance.id),
            if_version_match: None,
            config: Some(requested_config),
            metadata: current.metadata,
        }))
        .await
        .expect_err("a new selection without service-interface records must be rejected");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);
    assert_eq!(
        error.message(),
        "service VPC selections cannot be added to an existing extension-service attachment; remove it and create a new attachment after cleanup"
    );

    // Fresh public and database reads prove rejection preserved the empty `service_vpc_ids` and attachment ID.
    let persisted_public = instance.rpc_instance().await.into_inner();
    let persisted_public_attachment = &persisted_public
        .config
        .expect("persisted instance has a config")
        .dpu_extension_services
        .expect("persisted instance has an extension service")
        .service_configs[0];
    assert!(persisted_public_attachment.service_vpc_ids.is_empty());
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored");
    assert_eq!(
        persisted.config.extension_services.service_configs[0].id,
        Some(attachment_id)
    );
    assert!(persisted.config.network.service_interfaces.is_empty());
    assert_eq!(
        persisted.extension_services_config_version,
        no_interface_baseline.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        no_interface_baseline.network_config_version
    );

    Ok(())
}

/// Verifies a soft-deleted VPC prefix cannot prevent normal service detachment because persisted
/// the terminating attachment and service-interface record must remain until the prefix controller
/// can finish deleting that VPC prefix.
#[crate::sqlx_test]
async fn test_detach_networked_service_with_soft_deleted_parent_prefix(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Seed one valid service interface on a DPF-managed host with its VPC prefix, segment, prefix, and address.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let mut config = get_config();
    config.dpf.enabled = true;
    let env = create_test_env_with_overrides(pool, TestEnvOverrides::with_config(config)).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let selected_vpc = db::vpc::find_by_segment(&env.pool, segment_id)
        .await?
        .expect("created segment belongs to a VPC")
        .id;
    let managed_host = create_managed_host(&env).await;
    let mut txn = env.db_txn().await;
    db::machine::mark_machine_ingestion_done_with_dpf(txn.as_mut(), &managed_host.id).await?;
    txn.commit().await?;
    let instance = managed_host
        .instance_builer(&env)
        .single_interface_network_config(segment_id)
        .build()
        .await;
    create_fixture_tenant(&env, "best_org").await?;
    let service = create_networked_dpf_service(&env, "deleted-parent-detachment").await?;
    let endpoint = seed_networked_attachment(
        &env,
        instance.id,
        managed_host.dpu_ids[0],
        &service,
        selected_vpc,
    )
    .await?;

    // Soft-delete the configured VPC prefix while its generated network prefix still references it.
    env.api
        .delete_vpc_prefix(Request::new(rpc::forge::VpcPrefixDeletionRequest {
            id: Some(endpoint.vpc_prefix_id),
        }))
        .await?;

    // Remove the active service through the public whole-config update path.
    let current = instance.rpc_instance().await.into_inner();
    let mut requested_config = current.config.expect("instance has a config");
    requested_config.dpu_extension_services =
        Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![],
        });
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(instance.id),
            if_version_match: None,
            config: Some(requested_config),
            metadata: current.metadata,
        }))
        .await?;

    // Fresh reads confirm detachment started while the terminating attachment, endpoint, and
    // deleted VPC prefix remain.
    let persisted = db::instance::find_by_id(&env.pool, instance.id)
        .await?
        .expect("instance remains stored after detachment");
    assert_eq!(persisted.config.extension_services.service_configs.len(), 1);
    assert!(
        persisted.config.extension_services.service_configs[0]
            .removed
            .is_some()
    );
    assert_eq!(
        persisted.config.network.service_interfaces,
        vec![endpoint.clone()]
    );
    let parents = db::vpc_prefix::get_for_allocation_by_ids(
        &mut *env.pool.acquire().await?,
        &[endpoint.vpc_prefix_id],
    )
    .await?;
    assert_eq!(parents.len(), 1);
    assert!(parents[0].is_marked_as_deleted());

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
            machine_id: Some(mh.id),
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
                        service_vpc_ids: vec![],
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
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "test-service1".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service1-v1"),
                credential: None,
                observability: None,
                deployment_type: 0,
            },
        ))
        .await?
        .into_inner();

    // Update the extension service with a new version
    let service1 = env
        .api
        .update_dpu_extension_service(tonic::Request::new(
            rpc::forge::UpdateDpuExtensionServiceRequest {
                service_vpc_interfaces: None,
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
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "test-service2".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service2-v1"),
                credential: None,
                observability: None,
                deployment_type: 0,
            },
        ))
        .await?
        .into_inner();

    let service3 = env
        .api
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "test-service3".to_string(),
                description: Some("Test service for instance".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("test-service3-v1"),
                credential: None,
                observability: None,
                deployment_type: 0,
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
            machine_id: Some(mh.id),
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
                            service_vpc_ids: vec![],
                        },
                        rpc::forge::InstanceDpuExtensionServiceConfig {
                            service_id: service1.service_id.clone(),
                            version: service1
                                .latest_version_info
                                .as_ref()
                                .unwrap()
                                .version
                                .clone(),
                            service_vpc_ids: vec![],
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

/// Verifies extension-service updates reject duplicate service IDs, preserve terminating history,
/// permit endpoint-free same-version reattachment, and distinguish missing, deleted, and invalid
/// registrations so one service cannot map ambiguously to multiple attachments.
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
    // Derive an unregistered successor to service1 V2 so version validation exercises absence.
    let missing_service1_version = service1_version2
        .parse::<ConfigVersion>()?
        .increment()
        .to_string();
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
                service_vpc_ids: vec![],
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

    // Submit both registered versions before any attachment terminates so lifecycle validation
    // cannot obscure the request-local duplicate-service invariant.
    let duplicate_error = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
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
                        service_configs: vec![
                            rpc::forge::InstanceDpuExtensionServiceConfig {
                                service_id: service1.service_id.clone(),
                                version: service1_version1.clone(),
                                service_vpc_ids: vec![],
                            },
                            rpc::forge::InstanceDpuExtensionServiceConfig {
                                service_id: service1.service_id.clone(),
                                version: service1_version2.clone(),
                                service_vpc_ids: vec![],
                            },
                        ],
                    }),
                    power_profile: None,
                }),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("duplicate service identities must be rejected during update");

    // A service ID may select only one active version in an instance configuration.
    assert!(duplicate_error.message().starts_with(
        "duplicate extension services in configuration. only one version of each service is allowed"
    ));

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
                    service_vpc_ids: vec![],
                },
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service2.service_id.clone(),
                    version: service2_version.clone(),
                    service_vpc_ids: vec![],
                },
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: service3.service_id.clone(),
                    version: service3_version.clone(),
                    service_vpc_ids: vec![],
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

    // The public response exposes the three requested active attachments and omits terminating
    // service1 v1.
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

    // Record service2's attachment ID before removal so reattachment must revive it.
    let service2_attachment_id = db::instance::find_by_id(&env.pool, instance_id)
        .await?
        .expect("instance remains stored before extension-service removal")
        .config
        .extension_services
        .service_configs
        .into_iter()
        .find(|attachment| attachment.service_id.to_string() == service2.service_id)
        .and_then(|attachment| attachment.id)
        .expect("active service2 attachment has durable identity");

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

    // Preserve the exact unrelated tombstones so reattachment cannot discard cleanup history.
    let terminating_attachments = db::instance::find_by_id(&env.pool, instance_id)
        .await?
        .expect("instance remains stored after extension-service removal")
        .config
        .extension_services
        .service_configs;
    assert_eq!(terminating_attachments.len(), 4);

    // Re-add an ordinary endpoint-free service at the same version while its retained attachment
    // is terminating to exercise the public asynchronous-removal contract.
    let reattached = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
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
                            service_id: service2.service_id.clone(),
                            version: service2_version.clone(),
                            service_vpc_ids: vec![],
                        }],
                    }),
                    power_profile: None,
                }),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect("endpoint-free same-version reattachment remains supported")
        .into_inner();

    // The response and a fresh database read must expose the same revived attachment ID.
    let public_reattachment = reattached
        .status
        .as_ref()
        .expect("reattached instance has status")
        .dpu_extension_services
        .as_ref()
        .expect("reattached instance has extension-service status")
        .dpu_extension_services
        .iter()
        .find(|attachment| attachment.service_id == service2.service_id)
        .expect("service2 attachment is active in the public status");
    assert_eq!(
        public_reattachment.attachment_id,
        Some(service2_attachment_id.into())
    );
    assert!(public_reattachment.removed.is_none());
    assert_eq!(
        reattached
            .config
            .as_ref()
            .expect("reattached instance has a config")
            .dpu_extension_services
            .as_ref()
            .expect("reattached instance exposes active services")
            .service_configs
            .len(),
        1
    );
    let persisted_attachments = db::instance::find_by_id(&env.pool, instance_id)
        .await?
        .expect("reattached instance remains stored")
        .config
        .extension_services
        .service_configs;
    let persisted_reattachment = persisted_attachments
        .iter()
        .find(|attachment| attachment.service_id.to_string() == service2.service_id)
        .expect("service2 attachment is active again");
    assert_eq!(persisted_reattachment.id, Some(service2_attachment_id));
    assert!(persisted_reattachment.removed.is_none());
    assert_eq!(persisted_attachments.len(), 4);
    assert_eq!(
        persisted_attachments
            .iter()
            .filter(|attachment| attachment.service_id.to_string() != service2.service_id)
            .collect_vec(),
        terminating_attachments
            .iter()
            .filter(|attachment| attachment.service_id.to_string() != service2.service_id)
            .collect_vec(),
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
                version: missing_service1_version,
                service_vpc_ids: vec![],
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

    // Select an unknown registration so the API exposes entity absence distinctly from an invalid
    // version or a lifecycle precondition.
    let missing_service_id = ExtensionServiceId::new();
    let missing_error = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
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
                            service_id: missing_service_id.to_string(),
                            version: ConfigVersion::initial().to_string(),
                            service_vpc_ids: vec![],
                        }],
                    }),
                    power_profile: None,
                }),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("an unknown extension service must return not found");

    // Typed not-found lets clients distinguish a missing registration from a lifecycle precondition.
    assert_eq!(missing_error.code(), tonic::Code::NotFound);

    // Create and delete an otherwise unused Pod registration so its durable row remains
    // distinguishable from an identifier that never existed.
    let deleted_service = env
        .api
        .create_dpu_extension_service(tonic::Request::new(
            rpc::forge::CreateDpuExtensionServiceRequest {
                dpu_target: None,
                service_vpc_interfaces: vec![],
                service_id: None,
                service_name: "deleted-instance-service".to_string(),
                deployment_type: rpc::forge::DpuExtensionServiceDeploymentType::DemandDriven as i32,
                description: Some("Deleted service classification fixture".to_string()),
                tenant_organization_id: "best_org".to_string(),
                service_type: rpc::forge::DpuExtensionServiceType::KubernetesPod.into(),
                data: create_dpu_extension_service_data("deleted-instance-service-v1"),
                credential: None,
                observability: None,
            },
        ))
        .await?
        .into_inner();
    let deleted_version = deleted_service
        .latest_version_info
        .as_ref()
        .expect("created service has a version")
        .version
        .clone();
    env.api
        .delete_dpu_extension_service(Request::new(rpc::forge::DeleteDpuExtensionServiceRequest {
            service_id: deleted_service.service_id.clone(),
            versions: vec![],
        }))
        .await?;
    let deletion_baseline = db::instance::find_by_id(&env.pool, instance_id)
        .await?
        .expect("instance remains stored before deleted-service rejection");

    // Selecting a known deleted registration is a failed lifecycle precondition, not entity absence.
    let deleted_error = env
        .api
        .update_instance_config(tonic::Request::new(
            rpc::forge::InstanceConfigUpdateRequest {
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
                            service_id: deleted_service.service_id,
                            version: deleted_version,
                            service_vpc_ids: vec![],
                        }],
                    }),
                    power_profile: None,
                }),
                instance_id: Some(instance_id),
                metadata: Some(rpc::forge::Metadata {
                    name: "newinstance".to_string(),
                    description: "desc".to_string(),
                    labels: vec![],
                }),
            },
        ))
        .await
        .expect_err("a deleted extension service must fail as a precondition");
    assert_eq!(deleted_error.code(), tonic::Code::FailedPrecondition);

    // A fresh read proves classification changed neither attachment configuration nor either
    // configuration version.
    let persisted = db::instance::find_by_id(&env.pool, instance_id)
        .await?
        .expect("instance remains stored after deleted-service rejection");
    assert_eq!(
        persisted.config.extension_services,
        deletion_baseline.config.extension_services
    );
    assert_eq!(
        persisted.extension_services_config_version,
        deletion_baseline.extension_services_config_version
    );
    assert_eq!(
        persisted.network_config_version,
        deletion_baseline.network_config_version
    );

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
                        service_vpc_ids: vec![],
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

/// Verifies generic Kubernetes Pod termination reclaims only endpoint-free attachments while an
/// all-protected follow-up scan still performs unrelated Ready work, preventing retained endpoint
/// ownership from monopolizing controller iterations.
#[crate::sqlx_test]
async fn test_extension_service_cleanup_reclaims_only_endpoint_free_attachments(
    _: PgPoolOptions,
    options: PgConnectOptions,
) -> Result<(), Box<dyn std::error::Error>> {
    // Create three Pod attachments so cleanup must distinguish active,
    // endpoint-backed terminating, and endpoint-free terminating ownership.
    let pool = PgPoolOptions::new().connect_with(options).await?;
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let selected_vpc = db::vpc::find_by_segment(&env.pool, segment_id)
        .await?
        .expect("created segment belongs to a VPC")
        .id;
    let mh = create_managed_host(&env).await;
    let (retained_service, endpoint_backed_service, endpoint_free_service) =
        create_dpu_extension_services(&env).await?;
    let retained_service_id: ExtensionServiceId = retained_service.service_id.parse()?;
    let endpoint_backed_service_id: ExtensionServiceId =
        endpoint_backed_service.service_id.parse()?;
    let endpoint_free_service_id: ExtensionServiceId = endpoint_free_service.service_id.parse()?;
    let retained_version = retained_service
        .latest_version_info
        .as_ref()
        .expect("retained service has a version")
        .version
        .clone();
    let endpoint_backed_version = endpoint_backed_service
        .latest_version_info
        .as_ref()
        .expect("endpoint-backed service has a version")
        .version
        .clone();
    let endpoint_free_version = endpoint_free_service
        .latest_version_info
        .as_ref()
        .expect("endpoint-free service has a version")
        .version
        .clone();
    // Each attachment's lifecycle role drives cleanup. Tenant, OS, and host
    // network fields make the three-service instance otherwise valid.
    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![
                // Active control remains configured and must never enter cleanup.
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: retained_service.service_id.clone(),
                    version: retained_version.clone(),
                    service_vpc_ids: vec![],
                },
                // Terminating endpoint-backed attachment survives until its endpoint disappears.
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: endpoint_backed_service.service_id.clone(),
                    version: endpoint_backed_version,
                    service_vpc_ids: vec![],
                },
                // Terminating endpoint-free attachment is independently reclaimable.
                rpc::forge::InstanceDpuExtensionServiceConfig {
                    service_id: endpoint_free_service.service_id.clone(),
                    version: endpoint_free_version,
                    service_vpc_ids: vec![],
                },
            ],
        }),
        ..Default::default()
    };
    let tinstance = mh.instance_builer(&env).config(config).build().await;

    // Establish a synchronized running observation before requesting both detaches.
    network_configured_with_health_and_ext_services(&env, &mh.dpu_ids[0], None, None).await;
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            if_version_match: None,
            config: Some(rpc::InstanceConfig {
                tenant: Some(default_tenant_config()),
                os: Some(default_os_config()),
                network: Some(single_interface_network_config(segment_id)),
                dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
                    service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                        service_id: retained_service.service_id.clone(),
                        version: retained_version,
                        service_vpc_ids: vec![],
                    }],
                }),
                ..Default::default()
            }),
            instance_id: Some(tinstance.id),
            metadata: Some(rpc::forge::Metadata {
                name: "newinstance".to_string(),
                description: "desc".to_string(),
                labels: vec![],
            }),
        }))
        .await?;
    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: InstanceState::Ready,
        },
    )
    .await;

    // A fresh read proves callers see only the retained service while status
    // keeps all three entries.
    let rpc_instance = tinstance.rpc_instance().await.into_inner();
    let active_services = &rpc_instance
        .config
        .as_ref()
        .expect("instance has a config")
        .dpu_extension_services
        .as_ref()
        .expect("retained service remains active")
        .service_configs;
    assert_eq!(active_services.len(), 1);
    assert_eq!(active_services[0].service_id, retained_service.service_id);
    let rpc_status = rpc_instance.status.expect("instance has status");
    assert_eq!(
        rpc_status.tenant.expect("instance has tenant status").state,
        rpc::TenantState::Configuring as i32
    );
    let extension_services_status = rpc_status
        .dpu_extension_services
        .expect("instance has extension-service status");
    assert_eq!(extension_services_status.dpu_extension_services.len(), 3);
    assert_eq!(
        extension_services_status.configs_synced,
        rpc::forge::SyncState::Pending as i32
    );
    for service_id in [
        // Endpoint-backed termination remains visible until endpoint cleanup is observed.
        &endpoint_backed_service.service_id,
        // Endpoint-free termination is visible before this iteration reclaims it.
        &endpoint_free_service.service_id,
    ] {
        let removed_status = extension_services_status
            .dpu_extension_services
            .iter()
            .find(|status| status.service_id == *service_id)
            .expect("removed service remains tracked during termination");
        assert_eq!(
            removed_status.deployment_status,
            rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceUnknown as i32
        );
    }

    // Seed endpoints only for the active and one terminating attachment.
    let stored = db::instance::find_by_id(&env.pool, tinstance.id)
        .await?
        .expect("instance remains stored");
    let retained_attachment_id = stored
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|attachment| attachment.service_id == retained_service_id)
        .and_then(|attachment| attachment.id)
        .expect("retained attachment has an ID");
    let endpoint_backed_attachment_id = stored
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|attachment| attachment.service_id == endpoint_backed_service_id)
        .and_then(|attachment| attachment.id)
        .expect("endpoint-backed attachment has an ID");
    let endpoint_free_attachment_id = stored
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|attachment| attachment.service_id == endpoint_free_service_id)
        .and_then(|attachment| attachment.id)
        .expect("endpoint-free attachment has an ID");
    // Attachment identity is the cleanup key; the remaining fields make a
    // valid endpoint that can be persisted, reloaded, and compared afterward.
    let retained_service_interface = InstanceServiceInterfaceConfig {
        attachment_id: retained_attachment_id, // This active owner must survive cleanup.
        interface_ordinal: 0,
        dpu_id: mh.dpu_ids[0],
        slot_index: 0,
        vpc_id: selected_vpc,
        vpc_prefix_id: VpcPrefixId::new(),
        network_segment_id: segment_id,
        network_prefix_id: NetworkPrefixId::new(),
        link_prefix: "192.0.2.0/31".parse().expect("valid service prefix"),
        mac_address: MacAddress::new([0x02, 0, 0, 0, 0, 1]),
        internal_uuid: uuid::Uuid::new_v4(),
    };
    let endpoint_backed_service_interface = InstanceServiceInterfaceConfig {
        // A second attachment shares the same DPU/VPC slot resources. Only
        // its attachment and stable record identities are attachment-specific.
        attachment_id: endpoint_backed_attachment_id, // This terminating owner must survive.
        internal_uuid: uuid::Uuid::new_v4(),
        ..retained_service_interface.clone()
    };
    let mut seeded_network = stored.config.network.clone();
    seeded_network.service_interfaces = vec![
        retained_service_interface.clone(),
        endpoint_backed_service_interface.clone(),
    ];
    let mut txn = env.db_txn().await;
    db::instance::update_network_config(
        txn.as_mut(),
        stored.id,
        stored.network_config_version,
        &seeded_network,
        false,
    )
    .await?;
    txn.commit().await?;

    // Reload the fixture so the cleanup proof cannot pass with an ineffective seed.
    let seeded = db::instance::find_by_id(&env.pool, tinstance.id)
        .await?
        .expect("instance remains stored");
    assert_eq!(
        seeded.config.network.service_interfaces,
        vec![
            retained_service_interface.clone(),
            endpoint_backed_service_interface.clone()
        ]
    );

    // Report all three Pod services terminated without service-network cleanup evidence.
    network_configured_with_health_and_ext_services(
        &env,
        &mh.dpu_ids[0],
        None,
        Some(rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminated),
    )
    .await;
    env.run_machine_state_controller_iteration().await;

    // A fresh snapshot proves cleanup removes only the endpoint-free
    // terminating attachment and preserves both endpoint owners.
    let mut txn = env.db_txn().await;
    let snapshot = mh.snapshot(&mut txn).await;
    let persisted = snapshot.instance.expect("instance remains assigned");
    assert_eq!(persisted.config.extension_services.service_configs.len(), 2);
    assert_eq!(
        persisted
            .config
            .extension_services
            .service_configs
            .iter()
            .map(|attachment| attachment.service_id)
            .collect::<HashSet<_>>(),
        HashSet::from([retained_service_id, endpoint_backed_service_id])
    );
    let retained_attachment = persisted
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|attachment| attachment.service_id == retained_service_id)
        .expect("retained attachment remains present");
    assert_eq!(retained_attachment.service_id, retained_service_id);
    assert_eq!(retained_attachment.id, Some(retained_attachment_id));
    assert!(retained_attachment.removed.is_none());
    let endpoint_backed_attachment = persisted
        .config
        .extension_services
        .service_configs
        .iter()
        .find(|attachment| attachment.service_id == endpoint_backed_service_id)
        .expect("terminating attachment remains as a cleanup tombstone");
    assert_eq!(
        endpoint_backed_attachment.id,
        Some(endpoint_backed_attachment_id)
    );
    assert!(endpoint_backed_attachment.removed.is_some());
    assert!(
        persisted
            .config
            .extension_services
            .service_configs
            .iter()
            .all(|attachment| attachment.id != Some(endpoint_free_attachment_id))
    );
    assert_eq!(
        persisted.config.network.service_interfaces,
        vec![
            retained_service_interface,
            endpoint_backed_service_interface
        ]
    );

    // The exact Pod observation proves cleanup ran from the intended generic
    // termination signal rather than DPF label-derived placement status.
    let pod_observation = persisted
        .observations
        .extension_services
        .get(&mh.dpu_ids[0])
        .and_then(|observations| observations.for_service_type(ExtensionServiceType::KubernetesPod))
        .expect("DPU reported Kubernetes Pod service status");
    assert_eq!(pod_observation.extension_service_statuses.len(), 3);
    assert_eq!(
        pod_observation
            .extension_service_statuses
            .iter()
            .map(|status| status.service_id)
            .collect::<HashSet<_>>(),
        HashSet::from([
            retained_service_id,
            endpoint_backed_service_id,
            endpoint_free_service_id,
        ])
    );
    assert!(
        pod_observation
            .extension_service_statuses
            .iter()
            .all(|status| {
                status.service_type == ExtensionServiceType::KubernetesPod
                    && status.overall_state
                        == model::instance::status::extension_service::
                            ExtensionServiceDeploymentStatus::Terminated
            })
    );

    // Make periodic boot observation due so the next all-protected cleanup scan must continue
    // through Ready instead of returning with an empty cleanup transaction.
    let backdate_update = sqlx::query(
        "UPDATE machine_boot_interfaces \
         SET observed_at = CURRENT_TIMESTAMP - INTERVAL '100 years' \
         WHERE machine_id = $1 AND desired_version = verified_version",
    )
    .bind(mh.host().id)
    .execute(txn.as_mut())
    .await?;
    assert_eq!(backdate_update.rows_affected(), 1);
    txn.commit().await?;

    // The remaining endpoint-backed tombstones must not suppress periodic boot observation.
    env.redfish_sim.set_is_bios_setup(true);
    env.redfish_sim.set_is_boot_order_setup(true);
    let redfish_checkpoint = env.redfish_sim.timepoint();
    env.run_machine_state_controller_iteration().await;
    let redfish_actions = env
        .redfish_sim
        .actions_since(&redfish_checkpoint)
        .all_hosts();
    assert!(
        redfish_actions
            .iter()
            .any(|action| matches!(action, RedfishSimAction::IsBootOrderSetup { .. })),
        "endpoint-protected cleanup must allow unrelated Ready observation: {redfish_actions:?}"
    );

    Ok(())
}

/// Verifies cleanup winning after a combined service/network request read rejects the stale request
/// and rolls back metadata, `update_network_config_request`, and normalized addresses, preventing a
/// failed attachment comparison from committing partial configuration.
#[crate::sqlx_test]
async fn test_extension_cleanup_rejects_an_instance_update_from_before_cleanup(
    pool: PgPool,
) -> Result<(), Box<dyn std::error::Error>> {
    let env = create_test_env(pool).await;
    let (segment_id, replacement_segment_id) = env.create_vpc_and_dual_tenant_segment().await;
    let mh = create_managed_host(&env).await;
    let (removed_service, new_service, _) = create_dpu_extension_services(&env).await?;
    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(segment_id)),
        dpu_extension_services: Some(rpc::forge::InstanceDpuExtensionServicesConfig {
            service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
                service_id: removed_service.service_id,
                version: removed_service.latest_version_info.unwrap().version,
                service_vpc_ids: vec![],
            }],
        }),
        ..Default::default()
    };
    let tinstance = mh.instance_builer(&env).config(config).build().await;
    let instance = tinstance.rpc_instance().await.into_inner();
    let mut detached_config = instance.config.unwrap();
    detached_config.dpu_extension_services = None;
    env.api
        .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
            instance_id: Some(tinstance.id),
            config: Some(detached_config.clone()),
            metadata: instance.metadata,
            if_version_match: None,
        }))
        .await?;
    network_configured_with_health_and_ext_services(
        &env,
        &mh.dpu_ids[0],
        None,
        Some(rpc::forge::DpuExtensionServiceDeploymentStatus::DpuExtensionServiceTerminated),
    )
    .await;

    let before_cleanup = db::instance::find_by_id(&env.pool, tinstance.id)
        .await?
        .unwrap();
    assert_eq!(
        before_cleanup
            .config
            .extension_services
            .service_configs
            .len(),
        1
    );
    assert!(
        before_cleanup.config.extension_services.service_configs[0]
            .removed
            .is_some()
    );

    // Capture the public configuration and persisted `instance_addresses` before the losing request.
    let public_before_failed_update = tinstance.rpc_instance().await.into_inner();
    let expected_metadata = public_before_failed_update.metadata.clone();
    let expected_network = public_before_failed_update
        .config
        .as_ref()
        .and_then(|config| config.network.clone());
    let mut txn = env.db_txn().await;
    let baseline_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &tinstance.id,
        &segment_id,
    )
    .await?
    .into_iter()
    .map(|address| (address.address, address.prefix, address.vpc_id))
    .collect_vec();
    assert!(!baseline_addresses.is_empty());
    txn.commit().await?;

    // The API locks service definitions after reading and merging attachments.
    // Blocking on the new service lets the controller clean up the old one.
    let mut service_lock = env.db_txn().await;
    let blocker_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(service_lock.as_mut())
        .await?;
    db::extension_service::find_by_ids(
        service_lock.as_mut(),
        &[new_service.service_id.parse()?],
        false,
        true,
    )
    .await?;
    detached_config.dpu_extension_services = Some(rpc::forge::InstanceDpuExtensionServicesConfig {
        service_configs: vec![rpc::forge::InstanceDpuExtensionServiceConfig {
            service_id: new_service.service_id,
            version: new_service.latest_version_info.unwrap().version,
            service_vpc_ids: vec![],
        }],
    });
    detached_config.network = Some(single_interface_network_config(replacement_segment_id));
    let update =
        env.api
            .update_instance_config(Request::new(rpc::forge::InstanceConfigUpdateRequest {
                instance_id: Some(tinstance.id),
                config: Some(detached_config),
                metadata: Some(rpc::Metadata {
                    name: "must-not-be-saved".to_string(),
                    ..Default::default()
                }),
                if_version_match: None,
            }));
    let cleanup = async {
        common::postgres::wait_for_blocked_query(&env.pool, blocker_pid, "extension_services")
            .await;
        env.run_machine_state_controller_iteration().await;
        let cleaned = db::instance::find_by_id(&env.pool, tinstance.id)
            .await?
            .unwrap();
        assert!(cleaned.config.extension_services.service_configs.is_empty());
        assert_eq!(
            cleaned.extension_services_config_version,
            before_cleanup.extension_services_config_version,
            "cleanup must not request another DPU configuration generation",
        );

        // Release the service lock only after cleanup persists attachment removal so the blocked
        // request resumes with stale attachments and must roll back.
        service_lock.commit().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let (update_result, cleanup_result) = tokio::join!(update, cleanup);
    cleanup_result?;
    let error = update_result.expect_err("the API must reject the attachments read before cleanup");
    assert_eq!(error.code(), tonic::Code::FailedPrecondition);

    // A fresh public read must expose neither the requested metadata nor network replacement.
    let public_after_failed_update = tinstance.rpc_instance().await.into_inner();
    assert_eq!(public_after_failed_update.metadata, expected_metadata);
    assert_eq!(
        public_after_failed_update
            .config
            .and_then(|config| config.network),
        expected_network,
    );

    // The stored configurations must retain cleanup without a partial `update_network_config_request`.
    let persisted = db::instance::find_by_id(&env.pool, tinstance.id)
        .await?
        .unwrap();
    assert!(
        persisted
            .config
            .extension_services
            .service_configs
            .is_empty()
    );
    assert_eq!(persisted.config_version, before_cleanup.config_version);
    assert_eq!(persisted.metadata, before_cleanup.metadata);
    assert_eq!(
        persisted.network_config_version,
        before_cleanup.network_config_version,
    );
    assert_eq!(persisted.config.network, before_cleanup.config.network);
    assert!(persisted.update_network_config_request.is_none());
    assert_eq!(
        persisted.extension_services_config_version,
        before_cleanup.extension_services_config_version,
    );

    // The failed replacement must preserve the original `instance_addresses` and create no allocation.
    let mut txn = env.db_txn().await;
    let retained_addresses = db::instance_address::find_all_by_instance_id_and_segment_id(
        txn.as_mut(),
        &tinstance.id,
        &segment_id,
    )
    .await?
    .into_iter()
    .map(|address| (address.address, address.prefix, address.vpc_id))
    .collect_vec();
    assert_eq!(retained_addresses, baseline_addresses);
    assert!(
        db::instance_address::find_all_by_instance_id_and_segment_id(
            txn.as_mut(),
            &tinstance.id,
            &replacement_segment_id,
        )
        .await?
        .is_empty(),
    );
    txn.commit().await?;
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
                service_vpc_ids: vec![],
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
    assert_eq!(dpu_status.dpu_machine_id, Some(mh.dpu().id));
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
            machine_id: Some(mh.id),
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
            machine_id: Some(mh.id),
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
    use model::bmc_suppression::{BmcSuppressionSource, BmcSuppressionSubsystem};
    use model::machine::HostPlatformConfigurationState;

    const FACTORY_USER: &str = "root";
    const FACTORY_PW: &str = "factory-default";
    const PER_DEVICE_PW: &str = "prev-per-device";

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
            && let Some(suppression) = db::bmc_suppression::find(
                &env.pool,
                host_bmc_mac,
                BmcSuppressionSubsystem::SiteExplorer,
                BmcSuppressionSource::FactoryResetBmc,
            )
            .await
            .unwrap()
            && suppression.acknowledged_at.is_none()
        {
            let mut txn = env.pool.begin().await.unwrap();
            db::bmc_suppression::acknowledge(
                txn.as_mut(),
                host_bmc_mac,
                BmcSuppressionSubsystem::SiteExplorer,
            )
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
        !db::bmc_suppression::is_suppressed(
            &env.pool,
            host_bmc_mac,
            BmcSuppressionSubsystem::SiteExplorer,
        )
        .await
        .unwrap(),
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
