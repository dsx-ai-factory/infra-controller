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

use std::ops::DerefMut;
use std::time::SystemTime;

use carbide_uuid::instance::InstanceId;
use carbide_uuid::machine::MachineId;
use carbide_uuid::machine_validation::MachineValidationId;
use carbide_uuid::network::NetworkSegmentId;
use carbide_uuid::vpc::VpcPrefixId;
use model::instance::config::network::DeviceLocator;
use model::instance::config::nvlink::InstanceNvLinkConfig;
use model::instance::snapshot::InstanceSnapshot;
use model::instance::status::network::InstanceNetworkStatusObservation;
use model::machine::health_override::HARDWARE_HEALTH_OVERRIDE_PREFIX;
use model::machine::{
    CleanupContext, CleanupState, MachineState, MachineValidatingState, ManagedHostState,
    StableHostMachine, ValidationState,
};
use rpc::forge::InstanceDpuExtensionServicesConfig;
use rpc::forge::forge_server::Forge;
use rpc::forge::instance_interface_config::NetworkDetails;
use rpc::{InstanceReleaseRequest, Timestamp};

use super::{TestEnv, inject_machine_measurements, persist_machine_validation_result};
use crate::tests::common::api_fixtures::{RpcInstance, TestManagedHost};

pub(in crate::tests) struct TestInstanceBuilder<'a, 'b> {
    env: &'a TestEnv,
    config: rpc::InstanceConfig,
    tenant: rpc::TenantConfig,
    metadata: Option<rpc::Metadata>,
    mh: &'b TestManagedHost,
}

impl<'a, 'b> TestInstanceBuilder<'a, 'b> {
    pub(in crate::tests) fn new(env: &'a TestEnv, mh: &'b TestManagedHost) -> Self {
        Self {
            env,
            config: rpc::InstanceConfig {
                tenant: None,
                os: Some(default_os_config()),
                network: None,
                infiniband: None,
                network_security_group_id: None,
                dpu_extension_services: None,
                nvlink: None,
                spxconfig: None,
                power_profile: None,
            },
            tenant: default_tenant_config(),
            metadata: None,
            mh,
        }
    }

    pub(in crate::tests) fn config(mut self, config: rpc::InstanceConfig) -> Self {
        self.config = config;
        self
    }

    pub(in crate::tests) fn network(mut self, network: rpc::InstanceNetworkConfig) -> Self {
        self.config.network = Some(network);
        self
    }

    pub(in crate::tests) fn extension_services(
        mut self,
        extension_services: InstanceDpuExtensionServicesConfig,
    ) -> Self {
        self.config.dpu_extension_services = Some(extension_services);
        self
    }

    pub(in crate::tests) fn single_interface_network_config(
        self,
        segment_id: NetworkSegmentId,
    ) -> Self {
        self.network(single_interface_network_config(segment_id))
    }

    pub(in crate::tests) fn keyset_ids(mut self, ids: &[&str]) -> Self {
        self.tenant.tenant_keyset_ids = ids.iter().map(|s| (*s).into()).collect();
        self
    }

    pub(in crate::tests) fn metadata(mut self, metadata: rpc::Metadata) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub(in crate::tests) fn hostname(mut self, hostname: impl Into<String>) -> Self {
        self.tenant.hostname = Some(hostname.into());
        self
    }

    pub(in crate::tests) fn tenant_org(mut self, tenant_org: impl Into<String>) -> Self {
        self.tenant.tenant_organization_id = tenant_org.into();
        self
    }

    pub(in crate::tests) async fn build(self) -> TestInstance<'a, 'b> {
        let (tinstance, _) = self.build_and_return().await;
        tinstance
    }

    pub(in crate::tests) async fn build_and_return(
        mut self,
    ) -> (TestInstance<'a, 'b>, RpcInstance) {
        if self.config.tenant.is_none() {
            self.config.tenant = Some(self.tenant);
        }
        let instance_id = self
            .env
            .api
            .allocate_instance(tonic::Request::new(rpc::InstanceAllocationRequest {
                instance_id: None,
                machine_id: Some(self.mh.host().id),
                instance_type_id: None,
                config: Some(self.config),
                metadata: self.metadata,
                allow_unhealthy_machine: false,
            }))
            .await
            .expect("Create instance failed.")
            .into_inner()
            .id
            .expect("Missing instance ID");

        advance_created_instance_into_ready_state(self.env, self.mh).await;
        let tinstance = TestInstance {
            id: instance_id,
            env: self.env,
            mh: self.mh,
        };
        let rpc_instance = tinstance.rpc_instance().await;
        (tinstance, rpc_instance)
    }
}

pub(in crate::tests) struct TestInstance<'a, 'b> {
    pub(in crate::tests) id: InstanceId,
    env: &'a TestEnv,
    mh: &'b TestManagedHost,
}

type Txn<'a> = sqlx::Transaction<'a, sqlx::Postgres>;

impl<'a, 'b> TestInstance<'a, 'b> {
    pub(in crate::tests) async fn db_instance(&self, txn: &mut Txn<'_>) -> InstanceSnapshot {
        db::instance::find_by_id(txn.as_mut(), self.id)
            .await
            .unwrap()
            .unwrap()
    }

    pub(in crate::tests) async fn rpc_instance(&self) -> RpcInstance {
        let mut result = self
            .env
            .api
            .find_instances_by_ids(tonic::Request::new(rpc::forge::InstancesByIdsRequest {
                instance_ids: vec![self.id],
            }))
            .await
            .unwrap()
            .into_inner();
        assert_eq!(result.instances.len(), 1);
        RpcInstance::new(result.instances.remove(0))
    }

    pub(in crate::tests) async fn delete(&self) {
        self.mh.delete_instance(self.env, self.id).await
    }
}

pub(in crate::tests) async fn create_instance_with_ib_config<'a, 'b>(
    env: &'a TestEnv,
    mh: &'b TestManagedHost,
    ib_config: rpc::forge::InstanceInfinibandConfig,
    network_segment_id: NetworkSegmentId,
) -> (TestInstance<'a, 'b>, RpcInstance) {
    mh.instance_builer(env)
        .config(config_for_ib_config(ib_config, network_segment_id))
        .build_and_return()
        .await
}

pub(in crate::tests) fn single_interface_network_config(
    segment_id: NetworkSegmentId,
) -> rpc::InstanceNetworkConfig {
    rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(segment_id),
            network_details: Some(NetworkDetails::SegmentId(segment_id)),
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

pub(in crate::tests) fn single_interface_network_config_with_vfs(
    segment_ids: Vec<NetworkSegmentId>,
) -> rpc::InstanceNetworkConfig {
    let mut segment_iter = segment_ids.into_iter().enumerate();
    let (_function_id, segment_id) = segment_iter.next().unwrap();
    let mut interfaces = vec![rpc::InstanceInterfaceConfig {
        function_type: rpc::InterfaceFunctionType::Physical as i32,
        network_segment_id: Some(segment_id),
        network_details: Some(NetworkDetails::SegmentId(segment_id)),
        device: None,
        device_instance: 0,
        virtual_function_id: None,
        ip_address: None,
        ipv6_interface_config: None,
        routing_profile: None,
    }];

    interfaces.extend(
        segment_iter.map(|(function_id, segment_id)| rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Virtual as i32,
            network_segment_id: Some(segment_id),
            network_details: Some(NetworkDetails::SegmentId(segment_id)),
            device: None,
            device_instance: 0,
            virtual_function_id: Some(function_id as u32),
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        }),
    );

    rpc::InstanceNetworkConfig {
        interfaces,
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    }
}

pub(in crate::tests) fn interface_network_config_with_devices(
    segment_ids: &[NetworkSegmentId],
    device_locators: &[DeviceLocator],
) -> rpc::InstanceNetworkConfig {
    let interfaces = device_locators
        .iter()
        .zip(segment_ids)
        .map(|(dl, segment_id)| rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: Some(*segment_id),
            network_details: Some(NetworkDetails::SegmentId(*segment_id)),
            device: Some(dl.device.clone()),
            device_instance: dl.device_instance as u32,
            virtual_function_id: None,
            ip_address: None,
            ipv6_interface_config: None,
            routing_profile: None,
        })
        .collect();
    rpc::InstanceNetworkConfig {
        interfaces,
        #[allow(deprecated)]
        auto: false,
        auto_config: None,
    }
}

pub(in crate::tests) fn single_interface_network_config_with_vpc_prefix(
    prefix_id: VpcPrefixId,
) -> rpc::InstanceNetworkConfig {
    rpc::InstanceNetworkConfig {
        interfaces: vec![rpc::InstanceInterfaceConfig {
            function_type: rpc::InterfaceFunctionType::Physical as i32,
            network_segment_id: None,
            network_details: Some(NetworkDetails::VpcPrefixId(prefix_id)),
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
    }
}

pub(in crate::tests) fn default_os_config() -> rpc::forge::InstanceOperatingSystemConfig {
    rpc::forge::InstanceOperatingSystemConfig {
        phone_home_enabled: false,
        run_provisioning_instructions_on_every_boot: false,
        user_data: Some("SomeRandomData".to_string()),
        variant: Some(rpc::forge::instance_operating_system_config::Variant::Ipxe(
            rpc::forge::InlineIpxe {
                ipxe_script: "SomeRandomiPxe".to_string(),
            },
        )),
    }
}

pub(in crate::tests) fn default_tenant_config() -> rpc::TenantConfig {
    rpc::TenantConfig {
        tenant_organization_id: "Tenant1".to_string(),
        tenant_keyset_ids: vec![],
        hostname: None,
    }
}

pub(in crate::tests) fn config_for_ib_config(
    ib_config: rpc::forge::InstanceInfinibandConfig,
    network_segment_id: NetworkSegmentId,
) -> rpc::forge::InstanceConfig {
    rpc::forge::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(network_segment_id)),
        infiniband: Some(ib_config),
        nvlink: None,
        spxconfig: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        power_profile: None,
    }
}

pub(in crate::tests) fn config_for_nvlink_config(
    nvl_config: rpc::forge::InstanceNvLinkConfig,
    network_segment_id: NetworkSegmentId,
) -> rpc::forge::InstanceConfig {
    rpc::forge::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(default_os_config()),
        network: Some(single_interface_network_config(network_segment_id)),
        infiniband: None,
        nvlink: Some(nvl_config),
        spxconfig: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        power_profile: None,
    }
}

pub(in crate::tests) async fn advance_created_instance_into_state(
    env: &TestEnv,
    mh: &TestManagedHost,
    state_check_fn: impl Fn(&StableHostMachine) -> bool,
) {
    // Run network state machine here.
    env.run_network_segment_controller_iteration().await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        10,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::WaitingForNetworkConfig,
        },
    )
    .await;

    // Check whether we went through expected states
    // - DpaProvisioning
    // - WaitingForDpaToBeReady
    // - WaitingForNetworkSegmentToBeReady
    // - WaitingForNetworkConfig
    assert_eq!(
        mh.host().parsed_history(Some(4)).await,
        vec![
            ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::DpaProvisioning,
            },
            ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::WaitingForDpaToBeReady,
            },
            ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::WaitingForNetworkSegmentToBeReady,
            },
            ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::WaitingForNetworkConfig,
            }
        ]
    );

    // Now IB needs to get configured and DPU agent needs to acknowledge the latest config
    mh.network_configured(env).await;
    // If IB is used, another state controller iteration might be required to bind the IB ports
    // and get actually out of the state
    // This iteration will call bind_ib_ports
    env.run_machine_state_controller_iteration().await;
    env.run_ib_fabric_monitor_iteration().await;
    env.run_ib_fabric_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;
    env.run_nvl_partition_monitor_iteration().await;
    super::simulate_hardware_health_report(
        env,
        &mh.host().id,
        health_report::HealthReport::empty(format!("{HARDWARE_HEALTH_OVERRIDE_PREFIX}health")),
    )
    .await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        20,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::WaitingForRebootToReady,
        },
    )
    .await;
    env.run_machine_state_controller_iteration().await;
    mh.host().reboot_completed().await;

    // State controller continues to run till target state
    env.run_machine_state_controller_iteration_until_state_condition(
        &mh.host().id,
        20,
        state_check_fn,
    )
    .await;
}

pub(in crate::tests) async fn advance_created_instance_into_ready_state(
    env: &TestEnv,
    mh: &TestManagedHost,
) {
    advance_created_instance_into_state(env, mh, |machine| {
        matches!(
            machine.state.value,
            ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::Ready,
            }
        )
    })
    .await;

    assert_eq!(
        mh.host().parsed_history(Some(2)).await,
        vec![
            ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::WaitingForRebootToReady,
            },
            ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::Ready,
            }
        ]
    );
}

pub(in crate::tests) async fn delete_instance(
    env: &TestEnv,
    instance_id: InstanceId,
    mh: &TestManagedHost,
) {
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
    let instance = env.one_instance(instance_id).await;
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

    mh.network_configured(env).await;

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        2,
        ManagedHostState::Assigned {
            instance_state: model::machine::InstanceState::WaitingForDpusToUp,
        },
    )
    .await;

    mh.network_configured(env).await;

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
    handle_delete_post_bootingwithdiscoveryimage(env, mh).await;

    assert!(
        env.find_instances(vec![instance_id])
            .await
            .instances
            .is_empty()
    );

    // Run network state machine handler here.
    env.run_network_segment_controller_iteration().await;
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;
    env.run_network_segment_controller_iteration().await;
    env.run_network_segment_controller_iteration().await;
}

pub(in crate::tests) async fn handle_delete_post_bootingwithdiscoveryimage(
    env: &TestEnv,
    mh: &TestManagedHost,
) {
    let mut txn = env.pool.begin().await.unwrap();
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
    mh.network_configured(env).await;
    env.run_machine_state_controller_iteration().await;
    let state = mh.host().rpc_machine().await.state;
    if state == "Assigned/WaitingForNetworkReconfig" {
        // Also report that we are no longer on the tenant network for IB
        // That can require one more state controller iteration after the fabric
        // monitor supplied the results
        env.run_ib_fabric_monitor_iteration().await;
        env.run_ib_fabric_monitor_iteration().await;
        env.run_machine_state_controller_iteration().await;
    }

    if env.attestation_enabled {
        inject_machine_measurements(env, mh.host().id).await;
    }

    env.run_machine_state_controller_iteration_until_state_matches(
        &mh.host().id,
        3,
        ManagedHostState::WaitingForCleanup {
            cleanup_state: CleanupState::HostCleanup {
                boss_controller_id: None,
            },
            cleanup_context: CleanupContext::Deprovision,
        },
    )
    .await;

    let mut txn = env.pool.begin().await.unwrap();
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
        test_id: Some("instance".to_string()),
    };

    let response = mh.host().forge_agent_control().await;
    let validation_id = &response.data.unwrap().pair[1].value;

    machine_validation_result.validation_id = Some(validation_id.parse().unwrap());
    persist_machine_validation_result(env, machine_validation_result.clone()).await;

    let mut txn = env.pool.begin().await.unwrap();
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

    let mut txn = env.pool.begin().await.unwrap();
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
}

#[allow(dead_code)]
pub(in crate::tests) async fn update_instance_network_status_observation(
    dpu_id: &MachineId,
    obs: &InstanceNetworkStatusObservation,
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
) {
    let query = "UPDATE machines SET network_status_observation = jsonb_set(network_status_observation, ARRAY['instance_network_observation'], $1) WHERE id=$2";
    let _query_result = sqlx::query(query)
        .bind(sqlx::types::Json(obs))
        .bind(dpu_id.to_string())
        .execute(txn.deref_mut())
        .await
        .unwrap();
}

pub(in crate::tests) async fn create_instance_with_nvlink_config<'a, 'b>(
    env: &'a TestEnv,
    mh: &'b TestManagedHost,
    nvl_config: rpc::forge::InstanceNvLinkConfig,
    network_segment_id: NetworkSegmentId,
) -> (TestInstance<'a, 'b>, RpcInstance) {
    mh.instance_builer(env)
        .config(config_for_nvlink_config(nvl_config, network_segment_id))
        .build_and_return()
        .await
}

pub(in crate::tests) async fn update_instance_nvlink_config(
    txn: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: &InstanceId,
    config: &InstanceNvLinkConfig,
) {
    let query = "UPDATE instances SET nvlink_config=$1::json where id = $2::uuid returning id";
    let _query_result = sqlx::query(query)
        .bind(sqlx::types::Json(config))
        .bind(id.to_string())
        .execute(txn.deref_mut())
        .await
        .unwrap();
}
