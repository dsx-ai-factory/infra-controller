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

use carbide_uuid::instance::InstanceId;
use carbide_uuid::network::NetworkSegmentId;
use common::api_fixtures::{TestEnv, TestManagedHost, create_test_env};
use rpc::forge::forge_server::Forge;

use crate::tests::common;
use crate::tests::common::api_fixtures::create_managed_host;
use crate::tests::common::api_fixtures::instance::{
    TestInstance, default_os_config, default_tenant_config, single_interface_network_config,
};

const RETRYABLE_CUSTOM_IPXE: &str = "set nico-retry-provisioning 1\nSomeRandomiPxe";

#[crate::sqlx_test]
async fn test_instance_uses_custom_ipxe_only_once(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.pool.begin().await.unwrap();
    let host_interface = mh.host().first_interface(&mut txn).await;
    txn.rollback().await.unwrap();
    let host_arch = rpc::forge::MachineArchitecture::X86;

    let tinstance = create_instance(&env, &mh, false, segment_id).await;
    assert!(
        !tinstance
            .rpc_instance()
            .await
            .config()
            .os()
            .run_provisioning_instructions_on_every_boot
    );

    // The one-time custom script is marked for retries within this iPXE session.
    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert_eq!(pxe.pxe_script, RETRYABLE_CUSTOM_IPXE);

    // Second boot should return "exit"
    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert!(
        pxe.pxe_script.contains("Current state: Assigned/Ready"),
        "Actual script: {}",
        pxe.pxe_script
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    // A regular reboot attempt should still lead to returning "exit"
    invoke_instance_power(&env, tinstance.id, false).await;
    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert!(
        pxe.pxe_script.contains("Current state: Assigned/Ready"),
        "Actual script: {}",
        pxe.pxe_script
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));

    // A reboot with flag `boot_with_custom_ipxe` should provide the custom iPXE
    // The reboot is handled by the state machine, which makes sure the boot order is configured properly.
    invoke_instance_power(&env, tinstance.id, true).await;
    env.run_machine_state_controller_iteration_until_state_condition(&mh.id, 5, |machine| {
        matches!(
            machine.current_state(),
            model::machine::ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::HostPlatformConfiguration {
                    platform_config_state:
                        model::machine::HostPlatformConfigurationState::CheckHostConfig
                }
            }
        )
    })
    .await;
    mh.network_configured(&env).await;
    env.run_machine_state_controller_iteration_until_state_condition(&mh.id, 5, |machine| {
        matches!(
            machine.current_state(),
            model::machine::ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::WaitingForDpusToUp
            }
        )
    })
    .await;
    mh.network_configured(&env).await;
    env.run_machine_state_controller_iteration_until_state_condition(&mh.id, 5, |machine| {
        matches!(
            machine.current_state(),
            model::machine::ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::WaitingForRebootToReady
            }
        )
    })
    .await;
    env.run_machine_state_controller_iteration().await;
    mh.host().reboot_completed().await;
    env.run_machine_state_controller_iteration_until_state_condition(&mh.id, 5, |machine| {
        matches!(
            machine.current_state(),
            model::machine::ManagedHostState::Assigned {
                instance_state: model::machine::InstanceState::Ready
            }
        )
    })
    .await;
    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert_eq!(pxe.pxe_script, RETRYABLE_CUSTOM_IPXE);
    env.run_machine_state_controller_iteration().await;

    // The next reboot should again lead to returning "exit"
    invoke_instance_power(&env, tinstance.id, false).await;
    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert!(
        pxe.pxe_script.contains("Current state: Assigned/Ready"),
        "Actual script: {}",
        pxe.pxe_script
    );
    assert!(pxe.pxe_script.contains(
        "This state assumes an OS is provisioned and will exit into the OS in 5 seconds."
    ));
}

#[crate::sqlx_test]
async fn invoke_instance_power_requires_instance_id(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;

    let err = env
        .api
        .invoke_instance_power(tonic::Request::new(
            rpc::forge::InstancePowerRequest::default(),
        ))
        .await
        .unwrap_err();

    assert_eq!(err.code(), tonic::Code::InvalidArgument);
    assert!(err.message().contains("instance_id"));
}

#[crate::sqlx_test]
async fn test_instance_always_boot_with_custom_ipxe(pool: sqlx::PgPool) {
    let env = create_test_env(pool).await;
    let segment_id = env.create_vpc_and_tenant_segment().await;
    let mh = create_managed_host(&env).await;

    let mut txn = env.pool.begin().await.unwrap();
    let host_interface = mh.host().first_interface(&mut txn).await;
    txn.rollback().await.unwrap();
    let host_arch = rpc::forge::MachineArchitecture::X86;

    let tinstance = create_instance(&env, &mh, true, segment_id).await;
    assert!(
        tinstance
            .rpc_instance()
            .await
            .config()
            .os()
            .run_provisioning_instructions_on_every_boot
    );

    // The allocation's one-time request is marked for retries within this iPXE session.
    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert_eq!(pxe.pxe_script, RETRYABLE_CUSTOM_IPXE);

    // Later requests use `run_provisioning_instructions_on_every_boot` without the
    // one-time retry marker.
    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert_eq!(pxe.pxe_script, "SomeRandomiPxe");

    // A regular reboot attempt should also return custom iPXE instructions
    invoke_instance_power(&env, tinstance.id, false).await;
    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert_eq!(pxe.pxe_script, "SomeRandomiPxe");

    // A reboot with flag `boot_with_custom_ipxe` should also return custom iPXE instructions
    invoke_instance_power(&env, tinstance.id, true).await;
    let pxe = host_interface.get_pxe_instructions(host_arch).await;
    assert_eq!(pxe.pxe_script, "SomeRandomiPxe");
}

async fn invoke_instance_power(
    env: &TestEnv,
    instance_id: InstanceId,
    boot_with_custom_ipxe: bool,
) {
    env.api
        .invoke_instance_power(tonic::Request::new(rpc::forge::InstancePowerRequest {
            instance_id: Some(instance_id),
            operation: rpc::forge::instance_power_request::Operation::PowerReset as _,
            boot_with_custom_ipxe,
            apply_updates_on_reboot: false,
        }))
        .await
        .unwrap();
}

pub(in crate::tests) async fn create_instance<'a, 'b>(
    env: &'a TestEnv,
    mh: &'b TestManagedHost,
    run_provisioning_instructions_on_every_boot: bool,
    segment_id: NetworkSegmentId,
) -> TestInstance<'a, 'b> {
    let mut os: rpc::forge::InstanceOperatingSystemConfig = default_os_config();
    os.run_provisioning_instructions_on_every_boot = run_provisioning_instructions_on_every_boot;

    let config = rpc::InstanceConfig {
        tenant: Some(default_tenant_config()),
        os: Some(os),
        network: Some(single_interface_network_config(segment_id)),
        infiniband: None,
        network_security_group_id: None,
        dpu_extension_services: None,
        nvlink: None,
        spxconfig: None,
        power_profile: None,
    };
    mh.instance_builer(env).config(config).build().await
}
